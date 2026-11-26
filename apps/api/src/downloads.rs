use std::fmt::Write;

use axum::{
    Extension, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use uuid::Uuid;

use crate::{
    AppState,
    auth::User,
    error::{Context, Error, ResultResponse},
};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/files/{id}/content", get(content))
}

async fn content(
    State(state): State<AppState>,
    Extension(owner): Extension<User>,
    ctx: Context,
    Path(id): Path<Uuid>,
) -> ResultResponse {
    let (upload_id, name) = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT u.id, f.original_name FROM app.files f
         JOIN app.uploads u ON u.file_id = f.id
         WHERE f.id = $1 AND f.owner_id = $2 AND f.status = 'ready'",
    )
    .bind(id)
    .bind(owner.id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ctx.fail(error))?
    .ok_or_else(|| ctx.fail(Error::FileMissing))?;

    Ok((
        StatusCode::OK,
        [
            (
                header::HeaderName::from_static("x-accel-redirect"),
                HeaderValue::from_str(&format!("/_protected/files/{upload_id}"))
                    .expect("UUID yields a valid internal URI"),
            ),
            (header::CONTENT_DISPOSITION, attachment_name(&name)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            ),
        ],
    )
        .into_response())
}

fn attachment_name(name: &str) -> HeaderValue {
    let fallback: String = name
        .chars()
        .map(|ch| match ch {
            ' ' | '-' | '.' | '_' => ch,
            ch if ch.is_ascii_alphanumeric() => ch,
            _ => '_',
        })
        .collect();

    let mut value = format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            value.push(char::from(byte));
        } else {
            write!(value, "%{byte:02X}").expect("writing to a String is infallible");
        }
    }

    HeaderValue::from_str(&value).expect("encoded filenames contain only header-safe ASCII")
}
