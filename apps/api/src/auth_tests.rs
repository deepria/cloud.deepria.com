use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form,
    body::{Body, to_bytes},
    http::{Method, Request as HttpRequest},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

use super::*;
use crate::{MIGRATIONS, router};

#[test]
fn return_paths_and_cookie_policy() {
    assert_eq!(return_path("/memos"), Some("/memos"));
    assert_eq!(return_path("/memos/new"), Some("/memos/new"));
    for path in [
        "//example.com",
        "/memos/../",
        "/memos/new?go=elsewhere",
        "/memos/%2F",
    ] {
        assert_eq!(return_path(path), None);
    }
    assert!(Auth::new("http://example.com".into()).is_err());
    assert!(Auth::new("https://example.com/".into()).is_err());
    let auth = Auth::new("https://example.com".into()).unwrap();
    let cookie = auth.cookie(true, "sample", SESSION_SECONDS);
    let cookie = cookie.to_str().unwrap();
    for part in [
        "__Host-deepria-session=",
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Lax",
    ] {
        assert!(cookie.contains(part), "{cookie}");
    }
    assert!(!cookie.contains("Domain"));
}

const ORIGIN: &str = "http://127.0.0.1:8080";
const HOST: &str = "127.0.0.1:8080";

#[derive(Clone, Default)]
struct Provider {
    codes: Arc<Mutex<HashMap<String, (String, u64)>>>,
}

async fn mock_token(
    State(provider): State<Provider>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(code) = form.get("code") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some((challenge, identity)) = provider.codes.lock().unwrap().remove(code) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if form.get("redirect_uri") != Some(&format!("{ORIGIN}{CALLBACK_PATH}"))
        || form.get("client_secret").map(String::as_str) != Some("fixture-secret")
        || form
            .get("code_verifier")
            .map(|verifier| URL_SAFE_NO_PAD.encode(hash(verifier)))
            .as_deref()
            != Some(&challenge)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    Json(json!({"access_token": format!("fixture-{identity}"), "token_type": "bearer"}))
        .into_response()
}

async fn mock_identity(headers: HeaderMap) -> Response {
    let identity = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer fixture-"))
        .and_then(|v| v.parse::<u64>().ok());
    match identity {
        Some(id) => {
            Json(json!({"id": id, "email": "same-fixture@example.invalid"})).into_response()
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

struct Browser {
    app: Router,
    provider: Provider,
}

impl Browser {
    async fn call(
        &self,
        method: Method,
        path: &str,
        cookie: &str,
        origin: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST);
        if !cookie.is_empty() {
            builder = builder.header(header::COOKIE, cookie);
        }
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        let payload = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        self.app
            .clone()
            .oneshot(builder.body(payload).unwrap())
            .await
            .unwrap()
    }

    async fn attempt(&self, identity: u64) -> (String, String, String) {
        let response = self
            .call(
                Method::GET,
                "/api/auth/github/start?return_to=%2Fmemos%2Fnew",
                "",
                None,
                None,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = cookie_from(&response, "deepria-dev-oauth");
        let url = Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["redirect_uri"], format!("{ORIGIN}{CALLBACK_PATH}"));
        let code = Uuid::new_v4().to_string();
        self.provider
            .codes
            .lock()
            .unwrap()
            .insert(code.clone(), (query["code_challenge"].clone(), identity));
        let path = format!("{CALLBACK_PATH}?state={}&code={code}", query["state"]);
        (path, cookie, query["state"].clone())
    }

    async fn login(&self, identity: u64) -> String {
        let (path, binder, _) = self.attempt(identity).await;
        let response = self.call(Method::GET, &path, &binder, None, None).await;
        assert_eq!(response.headers()[header::LOCATION], "/memos/new");
        cookie_from(&response, "deepria-dev-session")
    }
}

fn cookie_from(response: &Response, name: &str) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap())
        .find(|value| value.split_once('=').is_some_and(|(key, _)| key == name))
        .unwrap_or_else(|| panic!("missing cookie {name}"))
        .to_owned()
}

async fn json_body(response: Response, status: StatusCode) -> Value {
    let actual = response.status();
    let body = to_bytes(response.into_body(), 640 * 1024).await.unwrap();
    assert_eq!(actual, status, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
#[ignore = "requires a fresh PostgreSQL cloud_test database in TEST_DATABASE_URL"]
async fn oauth_sessions_and_memos_postgres() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let url = Url::parse(&database_url).unwrap();
    assert_eq!(
        url.path(),
        "/cloud_test",
        "a dedicated cloud_test database is required"
    );
    let db = PgPool::connect(&database_url).await.unwrap();
    let app_schema_exists =
        sqlx::query_scalar::<_, bool>("SELECT to_regnamespace('app') IS NOT NULL")
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!app_schema_exists, "use a fresh test database");
    MIGRATIONS.run(&db).await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = Provider::default();
    let mock = Router::new()
        .route("/token", post(mock_token))
        .route("/user", get(mock_identity))
        .with_state(provider.clone());
    let mock_server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let mut auth = Auth::new(ORIGIN.into()).unwrap();
    auth.github = Some(
        GitHub::new(
            "fixture-client".into(),
            "fixture-secret".into(),
            ORIGIN,
            &format!("{fixture_origin}/authorize"),
            &format!("{fixture_origin}/token"),
            &format!("{fixture_origin}/user"),
        )
        .unwrap(),
    );
    let browser = Browser {
        app: router(AppState {
            db: db.clone(),
            auth,
        }),
        provider,
    };

    let (a, duplicate) = tokio::join!(
        provision_github(&db, "1101", "Account A"),
        provision_github(&db, "1101", "Account A")
    );
    let account_a = a.ok().unwrap();
    assert_eq!(duplicate.ok().unwrap(), account_a);
    let account_b = provision_github(&db, "1102", "Account B")
        .await
        .ok()
        .unwrap();
    assert_ne!(account_a, account_b);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM app.users")
            .fetch_one(&db)
            .await
            .unwrap(),
        2
    );

    let unauthorized = browser.call(Method::GET, "/api/me", "", None, None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let bad_return = browser
        .call(
            Method::GET,
            "/api/auth/github/start?return_to=%2F%2Fexample.com",
            "",
            None,
            None,
        )
        .await;
    assert_eq!(bad_return.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let (expired_path, expired_cookie, expired_state) = browser.attempt(1101).await;
    sqlx::query("UPDATE app.oauth_attempts SET expires_at = now() WHERE state_hash = $1")
        .bind(hash(&expired_state))
        .execute(&db)
        .await
        .unwrap();
    let expired = browser
        .call(Method::GET, &expired_path, &expired_cookie, None, None)
        .await;
    assert_eq!(expired.headers()[header::LOCATION], "/?login=invalid");

    let (path, binder, _) = browser.attempt(1101).await;
    let bad_binder = browser.call(Method::GET, &path, "", None, None).await;
    assert_eq!(bad_binder.headers()[header::LOCATION], "/?login=invalid");
    let (first, second) = tokio::join!(
        browser.call(Method::GET, &path, &binder, None, None),
        browser.call(Method::GET, &path, &binder, None, None),
    );
    let (successful, replay) = if first.headers()[header::LOCATION] == "/memos/new" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(successful.headers()[header::LOCATION], "/memos/new");
    assert_eq!(replay.headers()[header::LOCATION], "/?login=invalid");
    let a = cookie_from(&successful, "deepria-dev-session");
    let b = browser.login(1102).await;
    let me = json_body(
        browser.call(Method::GET, "/api/me", &a, None, None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(me["id"], account_a.to_string());

    let (path, binder, _) = browser.attempt(9999).await;
    let denial = browser.call(Method::GET, &path, &binder, None, None).await;
    assert_eq!(denial.headers()[header::LOCATION], "/?login=denied");
    let spoof = browser
        .app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .uri("/api/me")
                .header(header::HOST, "wrong.example.invalid")
                .header(header::COOKIE, &a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(spoof.status(), StatusCode::FORBIDDEN);

    let write = json!({"request_key": Uuid::new_v4(), "title": "first memo", "body": "fixture"});
    for origin in [None, Some("https://wrong.example.invalid")] {
        let blocked = browser
            .call(Method::POST, "/api/memos", &a, origin, Some(write.clone()))
            .await;
        assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    }
    let memo = json_body(
        browser
            .call(
                Method::POST,
                "/api/memos",
                &a,
                Some(ORIGIN),
                Some(write.clone()),
            )
            .await,
        StatusCode::CREATED,
    )
    .await;
    let retried = json_body(
        browser
            .call(Method::POST, "/api/memos", &a, Some(ORIGIN), Some(write))
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(memo["id"], retried["id"]);
    let path = format!("/api/memos/{}", memo["id"].as_str().unwrap());

    let b_page = json_body(
        browser
            .call(Method::GET, "/api/memos", &b, None, None)
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(b_page["items"], json!([]));
    for (method, body) in [
        (Method::GET, None),
        (
            Method::PATCH,
            Some(json!({"title":"other","body":"other","version":1})),
        ),
        (Method::DELETE, Some(json!({"version":1}))),
    ] {
        let blocked = browser.call(method, &path, &b, Some(ORIGIN), body).await;
        assert_eq!(blocked.status(), StatusCode::NOT_FOUND);
    }

    let edit = json!({"title": "changed", "body": "content", "version": 1});
    let (edit_a, edit_b) = tokio::join!(
        browser.call(Method::PATCH, &path, &a, Some(ORIGIN), Some(edit.clone())),
        browser.call(Method::PATCH, &path, &a, Some(ORIGIN), Some(edit)),
    );
    let mut statuses = [edit_a.status(), edit_b.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let stale_delete = browser
        .call(
            Method::DELETE,
            &path,
            &a,
            Some(ORIGIN),
            Some(json!({"version": 1})),
        )
        .await;
    assert_eq!(stale_delete.status(), StatusCode::CONFLICT);

    let second = json!({"request_key": Uuid::new_v4(), "title":"second memo", "body":""});
    assert_eq!(
        browser
            .call(Method::POST, "/api/memos", &a, Some(ORIGIN), Some(second))
            .await
            .status(),
        StatusCode::CREATED
    );
    let first_page = json_body(
        browser
            .call(Method::GET, "/api/memos?limit=1", &a, None, None)
            .await,
        StatusCode::OK,
    )
    .await;
    let cursor = first_page["next_cursor"].as_str().unwrap();
    let next_path = format!("/api/memos?limit=1&cursor={cursor}");
    let next_page = json_body(
        browser.call(Method::GET, &next_path, &a, None, None).await,
        StatusCode::OK,
    )
    .await;
    assert_ne!(first_page["items"][0]["id"], next_page["items"][0]["id"]);
    assert_eq!(next_page["next_cursor"], Value::Null);

    let logout = browser
        .call(Method::POST, "/api/auth/logout", &a, Some(ORIGIN), None)
        .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        browser
            .call(Method::GET, &path, &a, None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let relogin = browser.login(1101).await;
    let saved = json_body(
        browser.call(Method::GET, &path, &relogin, None, None).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["title"], "changed");
    assert_eq!(
        browser
            .call(
                Method::DELETE,
                &path,
                &relogin,
                Some(ORIGIN),
                Some(json!({"version": 2}))
            )
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        browser
            .call(Method::GET, &path, &relogin, None, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    sqlx::query("UPDATE app.sessions SET expires_at = now() WHERE user_id = $1")
        .bind(account_a)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        browser
            .call(Method::GET, "/api/me", &relogin, None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE app.users SET active = false WHERE id = $1")
        .bind(account_b)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        browser
            .call(Method::GET, "/api/me", &b, None, None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let (path, binder, _) = browser.attempt(1102).await;
    let inactive = browser.call(Method::GET, &path, &binder, None, None).await;
    assert_eq!(inactive.headers()[header::LOCATION], "/?login=denied");
    mock_server.abort();
}
