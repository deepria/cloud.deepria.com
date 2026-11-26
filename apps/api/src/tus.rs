use std::{
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use axum::{
    Extension, Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{head, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body::{Body as HttpBody, Frame};
use http_body_util::Limited;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use subtle::ConstantTimeEq;
use sync_wrapper::SyncWrapper;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    AppState,
    auth::User,
    error::{Context, Error, ResultResponse},
    uploads::MAX_FILE_SIZE,
};

const TUS_VERSION: &str = "1.0.0";
const MAX_CHUNK: usize = 16 * 1024 * 1024;
const TOKEN_HEADER: &str = "x-deepria-tus-token";
const PATCH_TYPE: &str = "application/offset+octet-stream";

#[derive(Clone)]
pub(crate) struct Gateway {
    upstream: Url,
    client: Client,
    token: HeaderValue,
    // Keep database connections available for the callback while start/cancel
    // requests hold row locks and wait on tusd.
    controls: Arc<Semaphore>,
}

impl Gateway {
    pub(crate) fn from_env() -> Result<Option<Self>, &'static str> {
        let upstream = std::env::var("TUS_UPSTREAM_URL").unwrap_or_default();
        let token = std::env::var("TUS_INTERNAL_TOKEN").unwrap_or_default();
        if upstream.is_empty() && token.is_empty() {
            return Ok(None);
        }

        let upstream = Url::parse(&upstream).map_err(|_| "invalid TUS_UPSTREAM_URL")?;
        if !matches!(upstream.scheme(), "http" | "https")
            || upstream.host_str().is_none()
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.query().is_some()
            || upstream.fragment().is_some()
            || upstream.path() != "/files/"
        {
            return Err("TUS_UPSTREAM_URL must be an internal HTTP(S) URL ending in /files/");
        }
        if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("TUS_INTERNAL_TOKEN must contain at least 32 hex characters");
        }

        let mut token = HeaderValue::from_str(&token).map_err(|_| "invalid TUS_INTERNAL_TOKEN")?;
        token.set_sensitive(true);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| "could not initialize tusd HTTP client")?;
        Ok(Some(Self {
            upstream,
            client,
            token,
            controls: Arc::new(Semaphore::new(2)),
        }))
    }

    fn item_url(&self, id: Uuid) -> Result<Url, Error> {
        self.upstream
            .join(&id.to_string())
            .map_err(|_| Error::Unavailable)
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("tus-resumable", TUS_VERSION)
    }

    async fn check(&self, id: Uuid) -> Result<reqwest::Response, Error> {
        self.request(Method::HEAD, self.item_url(id)?)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| Error::Unavailable)
    }

    async fn create(&self, id: Uuid, length: i64) -> Result<reqwest::Response, Error> {
        self.request(Method::POST, self.upstream.clone())
            .header(TOKEN_HEADER, self.token.clone())
            .header("upload-length", length)
            .header(
                "upload-metadata",
                format!("reservation {}", STANDARD.encode(id.to_string())),
            )
            .header(header::CONTENT_LENGTH, 0)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|_| Error::Unavailable)
    }
}

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new().route(
        "/uploads/{id}",
        head(inspect).post(start).patch(append).delete(cancel),
    )
}

pub(crate) fn internal_routes() -> Router<AppState> {
    Router::new().route(
        "/internal/tusd",
        post(hook).layer(DefaultBodyLimit::max(8192)),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HookRequest {
    #[serde(rename = "Type")]
    kind: String,
    event: HookEvent,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HookEvent {
    upload: HookUpload,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HookUpload {
    size: i64,
    size_is_deferred: bool,
    #[serde(rename = "MetaData")]
    metadata: HookMetadata,
}

#[derive(Deserialize)]
struct HookMetadata {
    reservation: Uuid,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct HookResponse {
    change_file_info: HookFileInfo,
}

#[derive(Serialize)]
struct HookFileInfo {
    #[serde(rename = "ID")]
    id: Uuid,
    #[serde(rename = "MetaData")]
    metadata: std::collections::BTreeMap<String, String>,
}

async fn hook(
    State(state): State<AppState>,
    ctx: Context,
    headers: HeaderMap,
    input: Result<Json<HookRequest>, JsonRejection>,
) -> ResultResponse {
    let Some(gateway) = state.tus else {
        return Err(ctx.fail(Error::Forbidden));
    };
    let supplied = unique_header(&headers, TOKEN_HEADER).ok().flatten();
    if !supplied.is_some_and(|value| bool::from(gateway.token.as_bytes().ct_eq(value.as_bytes()))) {
        return Err(ctx.fail(Error::Forbidden));
    }
    let Json(input) = input.map_err(|error| ctx.fail(error))?;
    if input.kind != "pre-create"
        || input.event.upload.size_is_deferred
        || !(0..=MAX_FILE_SIZE).contains(&input.event.upload.size)
        || input.event.upload.metadata.reservation.is_nil()
    {
        return Err(ctx.fail(Error::Invalid));
    }

    let id = input.event.upload.metadata.reservation;
    // tusd truncates a file when a custom ID is created twice. Claim it once,
    // even if the first request loses its response before the gateway commits.
    let claimed = sqlx::query(
        "UPDATE app.uploads u SET creation_claimed = true FROM app.files f
         WHERE u.id = $1 AND u.file_id = f.id AND NOT u.creation_claimed
           AND f.status = 'pending' AND f.size_bytes = $2",
    )
    .bind(id)
    .bind(input.event.upload.size)
    .execute(&state.db)
    .await
    .map_err(|e| ctx.fail(e))?;
    if claimed.rows_affected() != 1 {
        return Err(ctx.fail(Error::ReservationConflict));
    }

    Ok(Json(HookResponse {
        change_file_info: HookFileInfo {
            id,
            metadata: std::collections::BTreeMap::new(),
        },
    })
    .into_response())
}

#[derive(sqlx::FromRow)]
struct Upload {
    size_bytes: i64,
    status: String,
}

const OWNED_UPLOAD: &str =
    "SELECT f.size_bytes, f.status FROM app.uploads u JOIN app.files f ON f.id = u.file_id
     WHERE u.id = $1 AND f.owner_id = $2";

async fn load(pool: &PgPool, owner: Uuid, upload: Uuid) -> Result<Upload, Error> {
    sqlx::query_as::<_, Upload>(OWNED_UPLOAD)
        .bind(upload)
        .bind(owner)
        .fetch_optional(pool)
        .await?
        .ok_or(Error::UploadMissing)
}

async fn lock(tx: &mut PgConnection, owner: Uuid, upload: Uuid) -> Result<Upload, Error> {
    sqlx::query_as::<_, Upload>(
        "SELECT f.size_bytes, f.status FROM app.uploads u JOIN app.files f ON f.id = u.file_id
         WHERE u.id = $1 AND f.owner_id = $2 FOR UPDATE OF f",
    )
    .bind(upload)
    .bind(owner)
    .fetch_optional(tx)
    .await?
    .ok_or(Error::UploadMissing)
}

fn unique_header<'a>(headers: &'a HeaderMap, key: &str) -> Result<Option<&'a HeaderValue>, Error> {
    let mut values = headers.get_all(key).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(Error::Invalid);
    }
    Ok(value)
}

fn number_header(headers: &HeaderMap, key: &str) -> Result<Option<i64>, Error> {
    unique_header(headers, key)?
        .map(|value| {
            let value = value.to_str().map_err(|_| Error::Invalid)?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::Invalid);
            }
            value.parse::<i64>().map_err(|_| Error::Invalid)
        })
        .transpose()
}

fn unsupported_version(headers: &HeaderMap) -> Option<Response> {
    if matches!(unique_header(headers, "tus-resumable"), Ok(Some(value)) if value == TUS_VERSION) {
        None
    } else {
        Some(
            (
                StatusCode::PRECONDITION_FAILED,
                [("tus-resumable", TUS_VERSION), ("tus-version", TUS_VERSION)],
            )
                .into_response(),
        )
    }
}

fn upstream_response(upstream: reqwest::Response, ctx: Context) -> ResultResponse {
    let status = upstream.status();
    if status.is_server_error() || status.is_redirection() {
        return Err(ctx.fail(Error::Unavailable));
    }
    let mut response = (status, [("tus-resumable", TUS_VERSION)]).into_response();
    for key in ["upload-offset", "upload-length", "tus-version"] {
        if let Some(value) = upstream.headers().get(key) {
            response.headers_mut().insert(key, value.clone());
        }
    }
    Ok(response)
}

async fn empty_body(request: Request, ctx: Context) -> Result<(), crate::error::Failure> {
    tokio::time::timeout(Duration::from_secs(3), to_bytes(request.into_body(), 0))
        .await
        .map_err(|_| ctx.fail(Error::Invalid))?
        .map(|_| ())
        .map_err(|_| ctx.fail(Error::Invalid))
}

async fn start(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    Path(id): Path<Uuid>,
    request: Request,
) -> ResultResponse {
    if let Some(response) = unsupported_version(request.headers()) {
        return Ok(response);
    }
    let length = number_header(request.headers(), "upload-length")
        .map_err(|e| ctx.fail(e))?
        .ok_or_else(|| ctx.fail(Error::Invalid))?;
    if request.headers().contains_key("upload-defer-length")
        || request.headers().contains_key("upload-concat")
    {
        return Err(ctx.fail(Error::Invalid));
    }
    empty_body(request, ctx).await?;
    let gateway = state
        .tus
        .as_ref()
        .ok_or_else(|| ctx.fail(Error::Unavailable))?;
    let _permit = gateway
        .controls
        .try_acquire()
        .map_err(|_| ctx.fail(Error::Unavailable))?;
    let mut tx = state.db.begin().await.map_err(|e| ctx.fail(e))?;
    let reservation = lock(&mut tx, owner.id, id).await.map_err(|e| ctx.fail(e))?;
    if reservation.status == "cancelled" {
        return Err(ctx.fail(Error::UploadMissing));
    }
    if !matches!(
        reservation.status.as_str(),
        "pending" | "uploading" | "verifying"
    ) {
        return Err(ctx.fail(Error::ReservationConflict));
    }
    if length != reservation.size_bytes {
        return Err(ctx.fail(Error::ReservationConflict));
    }

    let found = gateway.check(id).await.map_err(|e| ctx.fail(e))?;
    let offset = match found.status() {
        StatusCode::OK | StatusCode::NO_CONTENT
            if number_header(found.headers(), "upload-length")
                .ok()
                .flatten()
                == Some(length) =>
        {
            number_header(found.headers(), "upload-offset")
                .ok()
                .flatten()
                .filter(|offset| *offset <= length)
                .ok_or_else(|| ctx.fail(Error::Unavailable))?
        }
        StatusCode::NOT_FOUND if reservation.status == "pending" => {
            let created = gateway.create(id, length).await.map_err(|e| ctx.fail(e))?;
            let actual_id = created
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|url| url.rsplit('/').next())
                .and_then(|id| Uuid::parse_str(id).ok());
            if created.status() != StatusCode::CREATED || actual_id != Some(id) {
                return Err(ctx.fail(Error::Unavailable));
            }
            0
        }
        _ => return Err(ctx.fail(Error::Unavailable)),
    };

    let status = if offset == length {
        "verifying"
    } else {
        "uploading"
    };
    sqlx::query("UPDATE app.files SET status = $1 FROM app.uploads u WHERE u.file_id = files.id AND u.id = $2")
        .bind(status).bind(id).execute(&mut *tx).await.map_err(|e| ctx.fail(e))?;
    tx.commit().await.map_err(|e| ctx.fail(e))?;

    Ok((
        StatusCode::CREATED,
        [
            (header::LOCATION, format!("/uploads/{id}")),
            (
                header::HeaderName::from_static("tus-resumable"),
                TUS_VERSION.into(),
            ),
        ],
    )
        .into_response())
}

async fn inspect(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ResultResponse {
    if let Some(response) = unsupported_version(&headers) {
        return Ok(response);
    }
    let reservation = load(&state.db, owner.id, id)
        .await
        .map_err(|e| ctx.fail(e))?;
    if !matches!(reservation.status.as_str(), "uploading" | "verifying") {
        return Err(ctx.fail(Error::UploadMissing));
    }
    let gateway = state
        .tus
        .as_ref()
        .ok_or_else(|| ctx.fail(Error::Unavailable))?;
    let response = gateway.check(id).await.map_err(|e| ctx.fail(e))?;
    observe_completion(&state.db, id, &reservation, &response, ctx).await?;
    upstream_response(response, ctx)
}

async fn append(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    Path(id): Path<Uuid>,
    request: Request,
) -> ResultResponse {
    let headers = request.headers();
    if let Some(response) = unsupported_version(headers) {
        return Ok(response);
    }
    let reservation = load(&state.db, owner.id, id)
        .await
        .map_err(|e| ctx.fail(e))?;
    if !matches!(reservation.status.as_str(), "uploading" | "verifying") {
        return Err(ctx.fail(Error::UploadMissing));
    }
    if unique_header(headers, "content-type").map_err(|e| ctx.fail(e))?
        != Some(&HeaderValue::from_static(PATCH_TYPE))
    {
        return Ok((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            [("tus-resumable", TUS_VERSION)],
        )
            .into_response());
    }
    let offset = number_header(headers, "upload-offset")
        .map_err(|e| ctx.fail(e))?
        .ok_or_else(|| ctx.fail(Error::Invalid))?;
    let length = number_header(headers, "content-length").map_err(|e| ctx.fail(e))?;
    if length.is_some_and(|size| size > MAX_CHUNK as i64) {
        return Err(ctx.fail(Error::RequestTooLarge));
    }
    let gateway = state
        .tus
        .as_ref()
        .ok_or_else(|| ctx.fail(Error::Unavailable))?;
    let mut chunk = gateway
        .request(
            Method::PATCH,
            gateway.item_url(id).map_err(|e| ctx.fail(e))?,
        )
        .header("upload-offset", offset)
        .header(header::CONTENT_TYPE, PATCH_TYPE);
    if let Some(length) = length {
        chunk = chunk.header(header::CONTENT_LENGTH, length);
    }
    let body = ChunkBody(SyncWrapper::new(Limited::new(
        request.into_body(),
        MAX_CHUNK,
    )));
    let response = chunk
        .body(reqwest::Body::wrap(body))
        .send()
        .await
        .map_err(|_| ctx.fail(Error::Unavailable))?;
    observe_completion(&state.db, id, &reservation, &response, ctx).await?;
    upstream_response(response, ctx)
}

async fn observe_completion(
    db: &PgPool,
    id: Uuid,
    reservation: &Upload,
    response: &reqwest::Response,
    ctx: Context,
) -> Result<(), crate::error::Failure> {
    if reservation.status == "uploading"
        && response.status().is_success()
        && number_header(response.headers(), "upload-offset")
            .ok()
            .flatten()
            == Some(reservation.size_bytes)
    {
        sqlx::query(
            "UPDATE app.files SET status = 'verifying' FROM app.uploads u
             WHERE u.file_id = files.id AND u.id = $1 AND files.status = 'uploading'",
        )
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| ctx.fail(e))?;
    }
    Ok(())
}

async fn cancel(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    Path(id): Path<Uuid>,
    request: Request,
) -> ResultResponse {
    if let Some(response) = unsupported_version(request.headers()) {
        return Ok(response);
    }
    empty_body(request, ctx).await?;
    let gateway = state
        .tus
        .as_ref()
        .ok_or_else(|| ctx.fail(Error::Unavailable))?;
    let _permit = gateway
        .controls
        .try_acquire()
        .map_err(|_| ctx.fail(Error::Unavailable))?;
    let mut tx = state.db.begin().await.map_err(|e| ctx.fail(e))?;
    let reservation = lock(&mut tx, owner.id, id).await.map_err(|e| ctx.fail(e))?;
    if reservation.status == "cancelled" {
        return Ok((StatusCode::NO_CONTENT, [("tus-resumable", TUS_VERSION)]).into_response());
    }
    if !matches!(
        reservation.status.as_str(),
        "pending" | "uploading" | "verifying"
    ) {
        return Err(ctx.fail(Error::ReservationConflict));
    }

    let response = gateway
        .request(
            Method::DELETE,
            gateway.item_url(id).map_err(|e| ctx.fail(e))?,
        )
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| ctx.fail(Error::Unavailable))?;
    if !matches!(
        response.status(),
        StatusCode::NO_CONTENT | StatusCode::NOT_FOUND | StatusCode::GONE
    ) {
        return upstream_response(response, ctx);
    }
    sqlx::query(
        "UPDATE app.files SET status = 'cancelled' FROM app.uploads u WHERE u.file_id = files.id AND u.id = $1",
    ).bind(id).execute(&mut *tx).await.map_err(|e| ctx.fail(e))?;
    tx.commit().await.map_err(|e| ctx.fail(e))?;
    Ok((StatusCode::NO_CONTENT, [("tus-resumable", TUS_VERSION)]).into_response())
}

struct ChunkBody(SyncWrapper<Limited<Body>>);

impl HttpBody for ChunkBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.0).get_pin_mut().poll_frame(cx)
    }
}
