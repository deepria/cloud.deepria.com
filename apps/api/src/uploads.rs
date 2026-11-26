use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    auth::User,
    error::{Context, Error, ResultResponse},
};

pub(crate) const MAX_FILE_SIZE: i64 = 10 * 1024 * 1024 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/files/uploads",
        post(reserve).layer(DefaultBodyLimit::max(4096)),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservationRequest {
    request_key: Uuid,
    original_name: String,
    size_bytes: i64,
}

impl ReservationRequest {
    fn validate(&self) -> Result<(), Error> {
        if self.request_key.is_nil()
            || self.size_bytes < 0
            || self.original_name.trim().is_empty()
            || self.original_name.chars().count() > 255
            || matches!(self.original_name.as_str(), "." | "..")
            || self.original_name.contains(['/', '\\'])
            || self.original_name.chars().any(char::is_control)
        {
            return Err(Error::Invalid);
        }
        if self.size_bytes > MAX_FILE_SIZE {
            return Err(Error::FileTooLarge);
        }
        Ok(())
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct Reservation {
    file_id: Uuid,
    upload_id: Uuid,
    original_name: String,
    size_bytes: i64,
    status: String,
}

async fn reserve(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    input: Result<Json<ReservationRequest>, JsonRejection>,
) -> ResultResponse {
    let Json(input) = input.map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ctx.fail(Error::RequestTooLarge)
        } else {
            ctx.fail(error)
        }
    })?;
    input.validate().map_err(|error| ctx.fail(error))?;

    // Both rows are created by the same SQL statement. Retried reservations use
    // the existing rows scoped to this user and request key.
    let created = sqlx::query_as::<_, Reservation>(
        "WITH new_file AS (
            INSERT INTO app.files (id, owner_id, reservation_key, original_name, size_bytes)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (owner_id, reservation_key) DO NOTHING
            RETURNING id, original_name, size_bytes, status
         ), new_upload AS (
            INSERT INTO app.uploads (id, file_id)
            SELECT $6, id FROM new_file
            RETURNING id, file_id
         )
         SELECT f.id AS file_id, u.id AS upload_id, f.original_name, f.size_bytes, f.status
         FROM new_file f JOIN new_upload u ON u.file_id = f.id",
    )
    .bind(Uuid::new_v4())
    .bind(owner.id)
    .bind(input.request_key)
    .bind(&input.original_name)
    .bind(input.size_bytes)
    .bind(Uuid::new_v4())
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?;

    if let Some(reservation) = created {
        return Ok((StatusCode::CREATED, Json(reservation)).into_response());
    }

    // A separate query sees the committed row if another request won the race.
    let reservation = sqlx::query_as::<_, Reservation>(
        "SELECT f.id AS file_id, u.id AS upload_id, f.original_name, f.size_bytes, f.status
         FROM app.files f JOIN app.uploads u ON u.file_id = f.id
         WHERE f.owner_id = $1 AND f.reservation_key = $2",
    )
    .bind(owner.id)
    .bind(input.request_key)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?
    .filter(|reservation| {
        reservation.original_name == input.original_name
            && reservation.size_bytes == input.size_bytes
    })
    .ok_or_else(|| ctx.fail(Error::ReservationConflict))?;

    Ok(Json(reservation).into_response())
}
