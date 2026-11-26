use axum::{
    http::{Method, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::uploads::MAX_FILE_SIZE;

use super::{Browser, ORIGIN, json_body};

const PATH: &str = "/api/files/uploads";

async fn reserve(browser: &Browser, cookie: &str, body: Value) -> Response {
    browser
        .call(Method::POST, PATH, cookie, Some(ORIGIN), Some(body))
        .await
}

pub(super) async fn verify_upload_reservations(
    browser: &Browser,
    db: &PgPool,
    cookie_a: &str,
    owner_a: Uuid,
    cookie_b: &str,
    owner_b: Uuid,
) {
    let key = Uuid::new_v4();
    let payload =
        json!({"request_key": key, "original_name": "시험 파일.bin", "size_bytes": MAX_FILE_SIZE});

    let response = reserve(browser, "", payload.clone()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = browser
        .call(
            Method::POST,
            PATH,
            cookie_a,
            Some("https://unrelated.example.invalid"),
            Some(payload.clone()),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    for (field, value) in [
        ("size_bytes", json!(-1)),
        ("original_name", json!("../unexpected")),
        ("request_key", json!(Uuid::nil())),
        ("owner_id", json!(owner_b)),
    ] {
        let mut invalid = payload.clone();
        invalid[field] = value;
        let response = reserve(browser, cookie_a, invalid).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}"
        );
    }

    let mut oversized = payload.clone();
    oversized["size_bytes"] = json!(MAX_FILE_SIZE + 1);
    let response = reserve(browser, cookie_a, oversized).await;
    let error = json_body(response, StatusCode::PAYLOAD_TOO_LARGE).await;
    assert_eq!(error["error"]["code"], "file_too_large");

    let mut oversized_body = payload.clone();
    oversized_body["original_name"] = json!("x".repeat(4096));
    let response = reserve(browser, cookie_a, oversized_body).await;
    let error = json_body(response, StatusCode::PAYLOAD_TOO_LARGE).await;
    assert_eq!(error["error"]["code"], "request_too_large");

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM app.files")
            .fetch_one(db)
            .await
            .unwrap(),
        0
    );

    let (first, retry) = tokio::join!(
        reserve(browser, cookie_a, payload.clone()),
        reserve(browser, cookie_a, payload.clone()),
    );
    let (created, repeated) = if first.status() == StatusCode::CREATED {
        (first, retry)
    } else {
        (retry, first)
    };
    let created = json_body(created, StatusCode::CREATED).await;
    let repeated = json_body(repeated, StatusCode::OK).await;
    assert_eq!(repeated, created);
    assert_eq!(created["size_bytes"], MAX_FILE_SIZE);
    assert_eq!(created["status"], "pending");

    let file_id = created["file_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let upload_id = created["upload_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let saved = sqlx::query_as::<_, (Uuid, Uuid, String, i64, String)>(
        "SELECT f.owner_id, u.id, f.original_name, f.size_bytes, f.status
         FROM app.files f JOIN app.uploads u ON u.file_id = f.id
         WHERE f.id = $1",
    )
    .bind(file_id)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(
        saved,
        (
            owner_a,
            upload_id,
            "시험 파일.bin".into(),
            MAX_FILE_SIZE,
            "pending".into()
        )
    );

    let mut changed = payload.clone();
    changed["size_bytes"] = json!(0);
    let response = reserve(browser, cookie_a, changed.clone()).await;
    let error = json_body(response, StatusCode::CONFLICT).await;
    assert_eq!(error["error"]["code"], "reservation_conflict");

    // Identical request keys are isolated to the authenticated owner.
    let other = json_body(
        reserve(browser, cookie_b, changed).await,
        StatusCode::CREATED,
    )
    .await;
    assert_ne!(other["file_id"], created["file_id"]);
    assert_ne!(other["upload_id"], created["upload_id"]);
    assert_eq!(other["size_bytes"], 0);
    let owners = sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT f.owner_id, f.size_bytes FROM app.files f
         JOIN app.uploads u ON u.file_id = f.id
         WHERE f.reservation_key = $1 ORDER BY f.size_bytes",
    )
    .bind(key)
    .fetch_all(db)
    .await
    .unwrap();
    assert_eq!(owners, [(owner_b, 0), (owner_a, MAX_FILE_SIZE)]);
}
