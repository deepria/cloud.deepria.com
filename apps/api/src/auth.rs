use std::time::Duration;

use axum::{
    Extension, Json, Router,
    extract::{Query, Request, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cookie::{Cookie, SameSite};
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    AppState, RequestId,
    error::{Context, Error, ResultResponse},
};

const CALLBACK_PATH: &str = "/api/auth/github/callback";
const ISSUER: &str = "https://github.com";
const ATTEMPT_SECONDS: i64 = 600;
const SESSION_SECONDS: i64 = 7 * 24 * 60 * 60;

type OAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Clone)]
pub(crate) struct Auth {
    origin: String,
    authority: String,
    secure: bool,
    github: Option<GitHub>,
}

#[derive(Clone)]
pub(crate) struct GitHub {
    oauth: OAuthClient,
    http: Client,
    profile_url: Url,
}

impl Auth {
    pub(crate) fn from_env() -> Result<Self, &'static str> {
        let origin =
            std::env::var("PUBLIC_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
        let mut auth = Self::new(origin)?;
        let id = std::env::var("GITHUB_OAUTH_CLIENT_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let secret = std::env::var("GITHUB_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|value| !value.trim().is_empty());
        match (id, secret) {
            (None, None) if !auth.secure => {}
            (Some(id), Some(secret)) => {
                auth.github = Some(GitHub::new(
                    id,
                    secret,
                    &auth.origin,
                    "https://github.com/login/oauth/authorize",
                    "https://github.com/login/oauth/access_token",
                    "https://api.github.com/user",
                )?);
            }
            _ => {
                return Err("set both GitHub OAuth credentials for HTTPS or to enable local login");
            }
        }
        Ok(auth)
    }

    pub(crate) fn new(origin: String) -> Result<Self, &'static str> {
        let url = Url::parse(&origin).map_err(|_| "invalid PUBLIC_ORIGIN")?;
        let secure = url.scheme() == "https";
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if url.origin().ascii_serialization() != origin
            || !(secure || (local && url.scheme() == "http"))
        {
            return Err("PUBLIC_ORIGIN must be a canonical HTTPS origin or HTTP loopback origin");
        }

        Ok(Self {
            authority: origin.split_once("://").expect("validated origin").1.into(),
            origin,
            secure,
            github: None,
        })
    }

    fn matches_host(&self, headers: &HeaderMap) -> bool {
        headers.get(header::HOST).is_some_and(|value| {
            value
                .as_bytes()
                .eq_ignore_ascii_case(self.authority.as_bytes())
        })
    }

    fn matches_origin(&self, headers: &HeaderMap) -> bool {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        origins
            .next()
            .is_some_and(|value| value == self.origin.as_str())
            && origins.next().is_none()
    }

    fn cookie_name(&self, session: bool) -> &'static str {
        match (self.secure, session) {
            (true, true) => "__Host-deepria-session",
            (true, false) => "__Host-deepria-oauth",
            (false, true) => "deepria-dev-session",
            (false, false) => "deepria-dev-oauth",
        }
    }

    fn cookie(&self, session: bool, token: &str, seconds: i64) -> HeaderValue {
        let cookie = Cookie::build((self.cookie_name(session), token))
            .path("/")
            .secure(self.secure)
            .http_only(true)
            .same_site(SameSite::Lax)
            .max_age(cookie::time::Duration::seconds(seconds))
            .build();
        HeaderValue::from_str(&cookie.to_string()).expect("cookie uses only ASCII tokens")
    }

    fn cookie_hash(&self, headers: &HeaderMap, session: bool) -> Option<Vec<u8>> {
        let name = self.cookie_name(session);
        let mut cookies = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(Cookie::split_parse)
            .filter_map(Result::ok)
            .filter(|cookie| cookie.name() == name);
        let cookie = cookies.next()?;
        if cookies.next().is_some() || cookie.value().len() != 43 {
            return None;
        }
        Some(hash(cookie.value()))
    }
}

impl GitHub {
    fn new(
        id: String,
        secret: String,
        origin: &str,
        auth_url: &str,
        token_url: &str,
        profile_url: &str,
    ) -> Result<Self, &'static str> {
        let oauth = BasicClient::new(ClientId::new(id))
            .set_client_secret(ClientSecret::new(secret))
            .set_auth_type(AuthType::RequestBody)
            .set_auth_uri(AuthUrl::new(auth_url.into()).map_err(|_| "invalid authorization URL")?)
            .set_token_uri(TokenUrl::new(token_url.into()).map_err(|_| "invalid token URL")?)
            .set_redirect_uri(
                RedirectUrl::new(format!("{origin}{CALLBACK_PATH}"))
                    .map_err(|_| "invalid callback URL")?,
            );
        let http = Client::builder()
            .user_agent("Deepria-Cloud")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| "could not initialize OAuth HTTP client")?;
        Ok(Self {
            oauth,
            http,
            profile_url: Url::parse(profile_url).map_err(|_| "invalid profile URL")?,
        })
    }

    async fn identity(&self, code: String, verifier: String) -> Result<String, Error> {
        let token = self
            .oauth
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier))
            .request_async(&self.http)
            .await
            .map_err(|_| Error::Unavailable)?;
        let response = self
            .http
            .get(self.profile_url.clone())
            .header(header::ACCEPT, "application/vnd.github+json")
            .bearer_auth(token.access_token().secret())
            .send()
            .await
            .map_err(|_| Error::Unavailable)?
            .error_for_status()
            .map_err(|_| Error::Unavailable)?;
        let GitHubIdentity { id } = response.json().await.map_err(|_| Error::Unavailable)?;
        if id == 0 {
            return Err(Error::Unavailable);
        }
        Ok(id.to_string())
    }
}

#[derive(Deserialize)]
struct GitHubIdentity {
    id: u64,
}

#[derive(Clone, Serialize, sqlx::FromRow)]
pub(crate) struct User {
    pub(crate) id: Uuid,
    display_name: String,
}

pub(crate) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/providers", get(providers))
        .route("/api/auth/github/start", get(start))
        .route(CALLBACK_PATH, get(callback))
}

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/api/me", get(me))
        .route("/api/auth/logout", post(logout))
}

async fn providers(State(state): State<AppState>) -> Json<Vec<&'static str>> {
    Json(if state.auth.github.is_some() {
        vec!["github"]
    } else {
        vec![]
    })
}

pub(crate) async fn require_user(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let id = *request
        .extensions()
        .get::<RequestId>()
        .expect("request context");
    let headers = request.headers();
    if !state.auth.matches_host(headers)
        || (!request.method().is_safe() && !state.auth.matches_origin(headers))
    {
        return Error::Forbidden.response(id);
    }
    let Some(token_hash) = state.auth.cookie_hash(headers, true) else {
        return Error::Unauthorized.response(id);
    };

    let user = sqlx::query_as::<_, User>(
        "SELECT u.id, u.display_name FROM app.sessions s JOIN app.users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() AND u.active",
    )
    .bind(token_hash)
    .fetch_optional(&state.db)
    .await;
    match user {
        Ok(Some(user)) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        Ok(None) => Error::Unauthorized.response(id),
        Err(error) => Error::from(error).response(id),
    }
}

async fn me(Extension(user): Extension<User>) -> Json<User> {
    Json(user)
}

async fn logout(State(state): State<AppState>, ctx: Context, headers: HeaderMap) -> ResultResponse {
    let token_hash = state
        .auth
        .cookie_hash(&headers, true)
        .ok_or_else(|| ctx.fail(Error::Unauthorized))?;
    sqlx::query(
        "UPDATE app.sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .execute(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, state.auth.cookie(true, "", 0))],
    )
        .into_response())
}

fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn random_token() -> Result<String, Error> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn return_path(value: &str) -> Option<&str> {
    match value {
        "/" | "/memos" | "/memos/new" => Some(value),
        _ => value
            .strip_prefix("/memos/")
            .filter(|id| id.len() == 36 && Uuid::parse_str(id).is_ok())
            .map(|_| value),
    }
}

#[derive(Deserialize)]
struct StartQuery {
    return_to: Option<String>,
}

async fn start(
    State(state): State<AppState>,
    ctx: Context,
    headers: HeaderMap,
    query: Result<Query<StartQuery>, QueryRejection>,
) -> ResultResponse {
    if !state.auth.matches_host(&headers) {
        return Err(ctx.fail(Error::Forbidden));
    }
    let Query(query) = query.map_err(|_| ctx.fail(Error::Invalid))?;
    let return_to = return_path(query.return_to.as_deref().unwrap_or("/memos"))
        .ok_or_else(|| ctx.fail(Error::Invalid))?;
    let github = state
        .auth
        .github
        .as_ref()
        .ok_or_else(|| ctx.fail(Error::Unavailable))?;
    let state_token = random_token().map_err(|error| ctx.fail(error))?;
    let browser_token = random_token().map_err(|error| ctx.fail(error))?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, _) = github
        .oauth
        .authorize_url(|| CsrfToken::new(state_token.clone()))
        .set_pkce_challenge(challenge)
        .url();

    sqlx::query("DELETE FROM app.oauth_attempts WHERE expires_at <= now()")
        .execute(&state.db)
        .await
        .map_err(|error| ctx.fail(error))?;
    sqlx::query(
        "INSERT INTO app.oauth_attempts (state_hash, browser_hash, pkce_verifier, return_to, expires_at)
         VALUES ($1, $2, $3, $4, now() + $5 * interval '1 second')"
    ).bind(hash(&state_token)).bind(hash(&browser_token)).bind(verifier.secret()).bind(return_to)
        .bind(ATTEMPT_SECONDS as f64)
        .execute(&state.db).await.map_err(|error| ctx.fail(error))?;

    Ok(redirect(
        url.as_str(),
        [state.auth.cookie(false, &browser_token, ATTEMPT_SECONDS)],
    ))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    ctx: Context,
    headers: HeaderMap,
    query: Result<Query<CallbackQuery>, QueryRejection>,
) -> ResultResponse {
    if !state.auth.matches_host(&headers) {
        return Err(ctx.fail(Error::Forbidden));
    }
    let response = finish_login(&state, &headers, query).await;
    let clear_attempt = state.auth.cookie(false, "", 0);
    Ok(match response {
        Ok((destination, token)) => redirect(
            &destination,
            [
                state.auth.cookie(true, &token, SESSION_SECONDS),
                clear_attempt,
            ],
        ),
        Err(Error::Unavailable) => redirect("/?login=unavailable", [clear_attempt]),
        Err(Error::Forbidden) => redirect("/?login=denied", [clear_attempt]),
        Err(_) => redirect("/?login=invalid", [clear_attempt]),
    })
}

async fn finish_login(
    state: &AppState,
    headers: &HeaderMap,
    query: Result<Query<CallbackQuery>, QueryRejection>,
) -> Result<(String, String), Error> {
    let Query(query) = query.map_err(|_| Error::Invalid)?;
    let github = state.auth.github.as_ref().ok_or(Error::Unavailable)?;
    let browser_hash = state
        .auth
        .cookie_hash(headers, false)
        .ok_or(Error::Invalid)?;
    if query.state.len() != 43 {
        return Err(Error::Invalid);
    }

    let (verifier, return_to) = sqlx::query_as::<_, (String, String)>(
        "DELETE FROM app.oauth_attempts WHERE state_hash = $1 AND browser_hash = $2 AND expires_at > now()
         RETURNING pkce_verifier, return_to"
    ).bind(hash(&query.state)).bind(browser_hash).fetch_optional(&state.db).await?.ok_or(Error::Invalid)?;
    if query.error.is_some() {
        return Err(Error::Invalid);
    }
    let code = query
        .code
        .filter(|code| !code.is_empty() && code.len() <= 2048)
        .ok_or(Error::Invalid)?;
    let subject = github.identity(code, verifier).await?;
    let return_to = return_path(&return_to).ok_or(Error::Invalid)?.to_owned();
    let token = random_token()?;
    let mut tx = state.db.begin().await?;
    let result = sqlx::query(
        "INSERT INTO app.sessions (token_hash, user_id, expires_at)
         SELECT $1, u.id, now() + $2 * interval '1 second'
         FROM app.users u JOIN app.oauth_accounts a ON a.user_id = u.id
         WHERE u.active AND a.provider = 'github' AND a.issuer = $3 AND a.subject = $4",
    )
    .bind(hash(&token))
    .bind(SESSION_SECONDS as f64)
    .bind(ISSUER)
    .bind(subject)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(Error::Forbidden);
    }
    if let Some(previous) = state.auth.cookie_hash(headers, true) {
        sqlx::query("UPDATE app.sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL")
            .bind(previous).execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM app.sessions WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((return_to, token))
}

fn redirect(location: &str, cookies: impl IntoIterator<Item = HeaderValue>) -> Response {
    let mut response = Redirect::to(location).into_response();
    for value in cookies {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

pub(crate) async fn provision_github(
    db: &PgPool,
    subject: &str,
    display_name: &str,
) -> Result<Uuid, Error> {
    let id = subject.parse::<u64>().map_err(|_| Error::Invalid)?;
    let subject = id.to_string();
    let display_name = display_name.trim();
    if id == 0
        || !(1..=100).contains(&display_name.chars().count())
        || display_name.chars().any(char::is_control)
    {
        return Err(Error::Invalid);
    }
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('github:' || $1, 0))")
        .bind(&subject)
        .execute(&mut *tx)
        .await?;

    if let Some(user_id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM app.oauth_accounts WHERE provider = 'github' AND issuer = $1 AND subject = $2"
    ).bind(ISSUER).bind(&subject).fetch_optional(&mut *tx).await? {
        tx.commit().await?;
        return Ok(user_id);
    }

    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO app.users (id, display_name) VALUES ($1, $2)")
        .bind(user_id)
        .bind(display_name)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO app.oauth_accounts (provider, issuer, subject, user_id) VALUES ('github', $1, $2, $3)")
        .bind(ISSUER).bind(subject).bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(user_id)
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
