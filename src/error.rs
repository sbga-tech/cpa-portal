use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("new account registration is disabled")]
    RegistrationClosed,
    #[error("configuration error: {0}")]
    Config(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Session(#[from] tower_sessions::session::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Public view of a failed browser request. `AppError` attaches it to the
/// response so that `routes::render_error_page` can render it with the site
/// layout; internal error details stay in the server log.
#[derive(Debug, Clone)]
pub struct ErrorPage {
    pub status: StatusCode,
    pub title: &'static str,
    pub message: String,
}

impl AppError {
    fn page(&self) -> ErrorPage {
        let (status, title, message) = match self {
            Self::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, "Request failed", message.clone())
            },
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Sign-in required",
                "Sign in again to continue.".to_owned(),
            ),
            Self::RegistrationClosed => (
                StatusCode::FORBIDDEN,
                "Registration closed",
                "This portal is not accepting new accounts. Existing accounts can still sign in."
                    .to_owned(),
            ),
            Self::Upstream(_) | Self::Http(_) => (
                StatusCode::BAD_GATEWAY,
                "Service unavailable",
                "A backend service did not respond correctly. Try again later.".to_owned(),
            ),
            Self::Config(_)
            | Self::Database(_)
            | Self::Session(_)
            | Self::Template(_)
            | Self::Url(_)
            | Self::Other(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The portal could not complete this request. Try again later.".to_owned(),
            ),
        };
        ErrorPage {
            status,
            title,
            message,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let page = self.page();
        if page.status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }
        let mut response = (page.status, format!("{}\n", page.message)).into_response();
        response.extensions_mut().insert(page);
        response
    }
}

pub type AppResult<T> = Result<T, AppError>;
