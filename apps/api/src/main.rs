mod auth;
mod error;
mod memos;
mod uploads;

use std::{env, net::SocketAddr, process::ExitCode, time::Duration};

use axum::{
    Json, Router,
    extract::{Extension, MatchedPath, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};
use uuid::Uuid;

static MIGRATIONS: Migrator = sqlx::migrate!("../../db/migrations");
const DB_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct AppState {
    db: PgPool,
    auth: auth::Auth,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt().json().with_target(false).init();

    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => serve().await,
        [command] if command == "migrate" => migrate().await,
        [command, subject, display_name] if command == "provision-github" => {
            provision_github(subject, display_name).await
        }
        _ => Err("expected no arguments, migrate, or provision-github <numeric-id> <display-name>"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            tracing::error!(message);
            ExitCode::FAILURE
        }
    }
}

fn database_pool(key: &str, max_connections: u32) -> Result<PgPool, &'static str> {
    let database_url = env::var(key).map_err(|_| match key {
        "DATABASE_URL" => "DATABASE_URL is required",
        _ => "MIGRATION_DATABASE_URL is required",
    })?;

    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        return Err("database URL requires a PostgreSQL scheme");
    }

    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(DB_TIMEOUT)
        .connect_lazy(&database_url)
        .map_err(|_| "invalid PostgreSQL configuration")
}

async fn migrate() -> Result<(), &'static str> {
    let pool = database_pool("MIGRATION_DATABASE_URL", 1)?;
    MIGRATIONS
        .run(&pool)
        .await
        .map_err(|_| "migration failed; check database availability, grants and schema history")?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn provision_github(subject: &str, display_name: &str) -> Result<(), &'static str> {
    let db = database_pool("DATABASE_URL", 1)?;
    let user_id = auth::provision_github(&db, subject, display_name)
        .await
        .map_err(|_| "could not provision GitHub identity; check numeric ID, name and database")?;
    println!("{user_id}");
    Ok(())
}

async fn serve() -> Result<(), &'static str> {
    let bind: SocketAddr = env::var("API_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()
        .map_err(|_| "API_BIND must be a socket address")?;
    let db = database_pool("DATABASE_URL", 4)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| "could not bind HTTP listener")?;

    tracing::info!(%bind, "HTTP listener started");
    let auth = auth::Auth::from_env()?;
    axum::serve(listener, router(AppState { db, auth }))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|_| "HTTP server failed")
}

fn router(state: AppState) -> Router {
    let protected = auth::protected_routes()
        .merge(memos::routes())
        .merge(uploads::routes())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_user,
        ));
    Router::new()
        .merge(auth::public_routes())
        .merge(protected)
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/health", get(health))
        .route("/api/ready", get(ready))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn(trace_request))
        .with_state(state)
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Clone, Copy)]
struct RequestId(Uuid);

async fn trace_request(mut request: Request, next: Next) -> Response {
    let request_id = RequestId(Uuid::new_v4());
    request.extensions_mut().insert(request_id);
    let method = request.method().clone();
    let route = request.extensions().get::<MatchedPath>().cloned();
    let started = std::time::Instant::now();

    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id.0.to_string()).expect("UUID is a valid header value"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    tracing::info!(
        request_id = %request_id.0,
        %method,
        route = route.as_ref().map_or("<unmatched>", MatchedPath::as_str),
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "HTTP request"
    );
    response
}

#[derive(Serialize)]
struct ServiceStatus {
    status: &'static str,
}

async fn health() -> Json<ServiceStatus> {
    Json(ServiceStatus { status: "ok" })
}

async fn ready(State(state): State<AppState>, Extension(id): Extension<RequestId>) -> Response {
    if matches!(
        tokio::time::timeout(DB_TIMEOUT, schema_is_current(&state.db)).await,
        Ok(Ok(true))
    ) {
        return Json(ServiceStatus { status: "ready" }).into_response();
    }

    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "service_unavailable",
        "서비스가 준비 중입니다.",
        id,
    )
}

async fn schema_is_current(db: &PgPool) -> Result<bool, sqlx::Error> {
    let app_schema_exists: bool = sqlx::query_scalar("SELECT to_regnamespace('app') IS NOT NULL")
        .fetch_one(db)
        .await?;
    if !app_schema_exists {
        return Ok(false);
    }

    let applied = sqlx::query_as::<_, (i64, Vec<u8>, bool)>(
        "SELECT version, checksum, success FROM public._sqlx_migrations ORDER BY version",
    )
    .fetch_all(db)
    .await?;
    let mut expected: Vec<_> = MIGRATIONS
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .collect();
    expected.sort_by_key(|migration| migration.version);

    Ok(applied.len() == expected.len()
        && applied
            .into_iter()
            .zip(expected)
            .all(|((version, checksum, success), migration)| {
                success && version == migration.version && checksum == migration.checksum.as_ref()
            }))
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
    request_id: String,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

fn api_error(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    id: RequestId,
) -> Response {
    (
        status,
        Json(ErrorEnvelope {
            error: ErrorBody { code, message },
            request_id: id.0.to_string(),
        }),
    )
        .into_response()
}

async fn not_found(Extension(id): Extension<RequestId>) -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "not_found",
        "요청한 경로를 찾을 수 없습니다.",
        id,
    )
}

async fn method_not_allowed(Extension(id): Extension<RequestId>) -> Response {
    api_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "이 경로에서 지원하지 않는 요청 방식입니다.",
        id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_does_not_need_a_database_and_fallbacks_have_request_ids() {
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://127.0.0.1/cloud_test")
            .unwrap();
        let app = router(AppState {
            db,
            auth: auth::Auth::new("http://127.0.0.1:8080".into()).unwrap(),
        });

        for (method, path, status) in [
            ("GET", "/health", StatusCode::OK),
            ("GET", "/api/health", StatusCode::OK),
            ("GET", "/api/missing", StatusCode::NOT_FOUND),
            ("POST", "/api/health", StatusCode::METHOD_NOT_ALLOWED),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            let request_id = response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .to_owned();
            assert!(Uuid::parse_str(&request_id).is_ok());
            if !status.is_success() {
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                assert!(std::str::from_utf8(&body).unwrap().contains(&request_id));
            }
        }
    }
}
