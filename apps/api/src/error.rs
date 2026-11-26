use axum::{
    extract::{FromRequestParts, rejection::JsonRejection},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{RequestId, api_error};

pub(crate) enum Error {
    Unauthorized,
    Forbidden,
    Missing,
    Conflict,
    Invalid,
    TooLarge,
    Unavailable,
}

impl Error {
    pub(crate) fn response(self, id: RequestId) -> Response {
        use StatusCode as Status;
        let (status, code, message) = match self {
            Self::Unauthorized => (Status::UNAUTHORIZED, "unauthorized", "로그인이 필요합니다."),
            Self::Forbidden => (
                Status::FORBIDDEN,
                "forbidden",
                "이 요청은 허용되지 않습니다.",
            ),
            Self::Missing => (Status::NOT_FOUND, "not_found", "메모를 찾을 수 없습니다."),
            Self::Conflict => (
                Status::CONFLICT,
                "version_conflict",
                "메모가 다른 곳에서 수정됐습니다.",
            ),
            Self::Invalid => (
                Status::UNPROCESSABLE_ENTITY,
                "invalid_input",
                "입력값을 확인해 주세요.",
            ),
            Self::TooLarge => (
                Status::PAYLOAD_TOO_LARGE,
                "too_large",
                "메모가 너무 큽니다.",
            ),
            Self::Unavailable => (
                Status::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "잠시 후 다시 시도해 주세요.",
            ),
        };
        api_error(status, code, message, id)
    }
}

impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

impl From<JsonRejection> for Error {
    fn from(error: JsonRejection) -> Self {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self::TooLarge
        } else {
            Self::Invalid
        }
    }
}

pub(crate) type ResultResponse = Result<Response, Failure>;

pub(crate) struct Failure {
    error: Error,
    id: RequestId,
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        self.error.response(self.id)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Context(pub(crate) RequestId);

impl<S: Send + Sync> FromRequestParts<S> for Context {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<RequestId>()
            .copied()
            .map(Self)
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl Context {
    pub(crate) fn fail(self, error: impl Into<Error>) -> Failure {
        Failure {
            error: error.into(),
            id: self.0,
        }
    }
}
