//! User API for the signed-in user's API keys.

use axum::{
    Json, Router,
    extract::{
        FromRequestParts, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{Method, StatusCode, header::ORIGIN, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::{
    api_error::ApiError,
    db::User,
    error::AppError,
    services::{
        api_keys::{self, OwnedKey},
        user::current_user,
    },
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/keys", get(list).post(create))
        .route("/api/v1/keys/{id}", patch(rename).delete(revoke))
        .route("/api/v1/keys/{id}/rotate", post(rotate))
}

/// The signed-in user. Requests that change state must come from Portal's own
/// origin.
struct ApiUser(User);

impl FromRequestParts<AppState> for ApiUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if !matches!(parts.method, Method::GET | Method::HEAD) {
            let expected = state.config.server.public_base_url.as_ref().origin();
            let same_origin = parts
                .headers
                .get(ORIGIN)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|origin| origin == expected.ascii_serialization());
            if !same_origin {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "cross_origin",
                    "Requests must come from the Portal page.",
                ));
            }
        }
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|(_, message)| {
                ApiError::internal(
                    "session_failed",
                    "Could not read the session.",
                    "load session",
                    message,
                )
            })?;
        match current_user(state, &session).await {
            Ok(Some(user)) => Ok(Self(user)),
            Ok(None) => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "Sign in first.",
            )),
            Err(error) => Err(ApiError::from_app(
                "session_failed",
                "Could not read the session.",
                "load session user",
                error,
            )),
        }
    }
}

#[derive(Serialize)]
struct KeysResponse {
    keys: Vec<OwnedKey>,
}

#[derive(Deserialize)]
struct LabelRequest {
    label: String,
}

async fn list(ApiUser(user): ApiUser, State(state): State<AppState>) -> Result<Response, ApiError> {
    let keys = api_keys::list(&state, user.id)
        .await
        .map_err(|error| key_error("list API keys", error))?;
    Ok(Json(KeysResponse { keys }).into_response())
}

async fn create(
    ApiUser(user): ApiUser,
    State(state): State<AppState>,
    body: Result<Json<LabelRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body.map_err(invalid_body)?;
    let key = api_keys::create(&state, user.id, &body.label)
        .await
        .map_err(|error| key_error("create API key", error))?;
    Ok((StatusCode::CREATED, Json(key)).into_response())
}

async fn rename(
    ApiUser(user): ApiUser,
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<LabelRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = id.map_err(invalid_path)?;
    let Json(body) = body.map_err(invalid_body)?;
    let key = api_keys::rename(&state, user.id, id, &body.label)
        .await
        .map_err(|error| key_error("rename API key", error))?;
    Ok(Json(key).into_response())
}

async fn revoke(
    ApiUser(user): ApiUser,
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = id.map_err(invalid_path)?;
    api_keys::revoke(&state, user.id, id)
        .await
        .map_err(|error| key_error("revoke API key", error))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn rotate(
    ApiUser(user): ApiUser,
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = id.map_err(invalid_path)?;
    let key = api_keys::rotate(&state, user.id, id)
        .await
        .map_err(|error| key_error("rotate API key", error))?;
    Ok((StatusCode::CREATED, Json(key)).into_response())
}

/// A failed CPA sync happens after the change is stored, and the periodic
/// sync applies it later.
fn key_error(operation: &'static str, error: AppError) -> ApiError {
    match error {
        AppError::Upstream(_) | AppError::Http(_) => ApiError::upstream(
            "cpa_sync_pending",
            "The change is saved, but CPA has not applied it yet. Portal retries within a minute.",
            operation,
            error,
        ),
        error => ApiError::from_app(
            "key_request_failed",
            "Could not update the keys.",
            operation,
            error,
        ),
    }
}

fn invalid_body(rejection: JsonRejection) -> ApiError {
    ApiError::bad_request("invalid_body", rejection.body_text())
}

fn invalid_path(rejection: PathRejection) -> ApiError {
    ApiError::bad_request("invalid_path", rejection.body_text())
}
