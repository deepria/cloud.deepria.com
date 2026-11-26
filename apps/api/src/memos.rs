use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    AppState,
    auth::User,
    error::{Context, Error, ResultResponse},
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/memos", get(list).post(create))
        .route("/api/memos/{id}", get(read).patch(update).delete(delete))
        .layer(DefaultBodyLimit::max(640 * 1024))
}

#[derive(Serialize, sqlx::FromRow)]
struct MemoSummary {
    id: Uuid,
    title: String,
    version: i64,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

#[derive(Serialize, sqlx::FromRow)]
struct Memo {
    id: Uuid,
    title: String,
    body: String,
    version: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

#[derive(Serialize)]
struct MemoPage {
    items: Vec<MemoSummary>,
    next_cursor: Option<String>,
}

struct Cursor(OffsetDateTime, Uuid);

impl Cursor {
    fn encode(memo: &MemoSummary) -> String {
        URL_SAFE_NO_PAD.encode(format!(
            "{}:{}",
            memo.updated_at.unix_timestamp_nanos() / 1_000,
            memo.id
        ))
    }

    fn decode(value: &str) -> Option<Self> {
        if value.len() > 80 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
        let (micros, id) = std::str::from_utf8(&bytes).ok()?.split_once(':')?;
        Some(Self(
            OffsetDateTime::from_unix_timestamp_nanos(
                i128::from(micros.parse::<i64>().ok()?) * 1_000,
            )
            .ok()?,
            id.parse().ok()?,
        ))
    }
}

async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    ctx: Context,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> ResultResponse {
    let Query(query) = query.map_err(|_| ctx.fail(Error::Invalid))?;
    let limit = query.limit.unwrap_or(30);
    if !(1..=100).contains(&limit) {
        return Err(ctx.fail(Error::Invalid));
    }
    let mut items = if let Some(value) = query.cursor {
        let Cursor(timestamp, id) = Cursor::decode(&value).ok_or_else(|| ctx.fail(Error::Invalid))?;
        sqlx::query_as::<_, MemoSummary>(
            "SELECT id, title, version, updated_at FROM app.memos WHERE owner_id = $1 AND (updated_at, id) < ($2, $3)
             ORDER BY updated_at DESC, id DESC LIMIT $4"
        )
        .bind(user.id).bind(timestamp).bind(id).bind(i64::from(limit) + 1)
        .fetch_all(&state.db).await
    } else {
        sqlx::query_as::<_, MemoSummary>(
            "SELECT id, title, version, updated_at FROM app.memos WHERE owner_id = $1
             ORDER BY updated_at DESC, id DESC LIMIT $2"
        )
        .bind(user.id).bind(i64::from(limit) + 1).fetch_all(&state.db).await
    }.map_err(|error| ctx.fail(error))?;

    let next_cursor = if items.len() > limit as usize {
        items.pop();
        items.last().map(Cursor::encode)
    } else {
        None
    };
    Ok(Json(MemoPage { items, next_cursor }).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    title: String,
    body: String,
}

impl Content {
    fn valid(&self) -> bool {
        self.title.chars().count() <= 200
            && self.body.chars().count() <= 100_000
            && !self.title.contains('\0')
            && !self.body.contains('\0')
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    title: String,
    body: String,
    version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    version: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewMemo {
    request_key: Uuid,
    title: String,
    body: String,
}

async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    ctx: Context,
    input: Result<Json<NewMemo>, JsonRejection>,
) -> ResultResponse {
    let Json(NewMemo {
        request_key,
        title,
        body,
    }) = input.map_err(|error| ctx.fail(error))?;
    let input = Content { title, body };
    if request_key.is_nil() || !input.valid() {
        return Err(ctx.fail(Error::Invalid));
    }
    let memo = sqlx::query_as::<_, Memo>(
        "INSERT INTO app.memos (id, owner_id, title, body, create_key) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_id, create_key) DO NOTHING
         RETURNING id, title, body, version, created_at, updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(input.title)
    .bind(input.body)
    .bind(request_key)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?;
    if let Some(memo) = memo {
        return Ok((StatusCode::CREATED, Json(memo)).into_response());
    }

    let existing = sqlx::query_as::<_, Memo>(
        "SELECT id, title, body, version, created_at, updated_at
         FROM app.memos WHERE owner_id = $1 AND create_key = $2",
    )
    .bind(user.id)
    .bind(request_key)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?
    .ok_or_else(|| ctx.fail(Error::Conflict))?;
    Ok(Json(existing).into_response())
}

fn parse_id(path: &str) -> Result<Uuid, Error> {
    path.parse().map_err(|_| Error::Missing)
}

async fn read(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    ctx: Context,
    Path(path): Path<String>,
) -> ResultResponse {
    let id = parse_id(&path).map_err(|error| ctx.fail(error))?;
    let memo = sqlx::query_as::<_, Memo>(
        "SELECT id, title, body, version, created_at, updated_at FROM app.memos WHERE owner_id = $1 AND id = $2"
    )
    .bind(user.id).bind(id).fetch_optional(&state.db).await
    .map_err(|error| ctx.fail(error))?.ok_or_else(|| ctx.fail(Error::Missing))?;
    Ok(Json(memo).into_response())
}

async fn update(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    ctx: Context,
    Path(path): Path<String>,
    input: Result<Json<Edit>, JsonRejection>,
) -> ResultResponse {
    let id = parse_id(&path).map_err(|error| ctx.fail(error))?;
    let Json(Edit {
        title,
        body,
        version,
    }) = input.map_err(|error| ctx.fail(error))?;
    let content = Content { title, body };
    if version < 1 || !content.valid() {
        return Err(ctx.fail(Error::Invalid));
    }

    let memo = sqlx::query_as::<_, Memo>(
        "UPDATE app.memos SET title = $4, body = $5, updated_at = clock_timestamp(), version = version + 1
         WHERE owner_id = $1 AND id = $2 AND version = $3
         RETURNING id, title, body, version, created_at, updated_at"
    )
    .bind(user.id).bind(id).bind(version).bind(content.title).bind(content.body)
    .fetch_optional(&state.db).await.map_err(|error| ctx.fail(error))?;
    match memo {
        Some(memo) => Ok(Json(memo).into_response()),
        None => Err(ctx.fail(missing_or_conflict(&state.db, user.id, id).await)),
    }
}

async fn delete(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    ctx: Context,
    Path(path): Path<String>,
    input: Result<Json<Version>, JsonRejection>,
) -> ResultResponse {
    let id = parse_id(&path).map_err(|error| ctx.fail(error))?;
    let Json(Version { version }) = input.map_err(|error| ctx.fail(error))?;
    if version < 1 {
        return Err(ctx.fail(Error::Invalid));
    }
    let result =
        sqlx::query("DELETE FROM app.memos WHERE owner_id = $1 AND id = $2 AND version = $3")
            .bind(user.id)
            .bind(id)
            .bind(version)
            .execute(&state.db)
            .await
            .map_err(|error| ctx.fail(error))?;
    if result.rows_affected() == 0 {
        return Err(ctx.fail(missing_or_conflict(&state.db, user.id, id).await));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn missing_or_conflict(db: &PgPool, owner: Uuid, id: Uuid) -> Error {
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM app.memos WHERE owner_id = $1 AND id = $2)",
    )
    .bind(owner)
    .bind(id)
    .fetch_one(db)
    .await
    {
        Ok(true) => Error::Conflict,
        Ok(false) => Error::Missing,
        Err(error) => error.into(),
    }
}
