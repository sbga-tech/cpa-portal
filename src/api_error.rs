use std::borrow::Cow;

use axum::{
    Json,
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::error::AppError;

/// JSON error of the Admin API and the user API: a stable code and a public
/// message. Internal details only go to the log.
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: Cow<'static, str>,
    bearer_challenge: bool,
}

impl ApiError {
    pub fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            bearer_challenge: false,
        }
    }

    /// Missing or wrong Admin API Bearer token.
    pub fn bearer_unauthorized() -> Self {
        Self {
            bearer_challenge: true,
            ..Self::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "A valid Bearer token is required.",
            )
        }
    }

    pub fn bad_request(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn internal(
        code: &'static str,
        message: &'static str,
        operation: &'static str,
        error: impl std::fmt::Display,
    ) -> Self {
        tracing::error!(operation, error = %error, "API request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, message)
    }

    pub fn upstream(
        code: &'static str,
        message: &'static str,
        operation: &'static str,
        error: impl std::fmt::Display,
    ) -> Self {
        tracing::error!(operation, error = %error, "API upstream request failed");
        Self::new(StatusCode::BAD_GATEWAY, code, message)
    }

    /// Maps an application error to `code`, keeping the message of a bad
    /// request and using `message` for failures.
    pub fn from_app(
        code: &'static str,
        message: &'static str,
        operation: &'static str,
        error: AppError,
    ) -> Self {
        match error {
            AppError::BadRequest(detail) => Self::bad_request(code, detail),
            AppError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found", "No such item."),
            AppError::Upstream(_) | AppError::Http(_) => {
                Self::upstream(code, message, operation, error)
            },
            _ => Self::internal(code, message, operation, error),
        }
    }
}

#[derive(Serialize)]
struct ApiErrorEnvelope<'a> {
    error: ApiErrorBody<'a>,
}

#[derive(Serialize)]
struct ApiErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ApiErrorEnvelope {
                error: ApiErrorBody {
                    code: self.code,
                    message: &self.message,
                },
            }),
        )
            .into_response();
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if self.bearer_challenge {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
