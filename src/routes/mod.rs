use axum::{Router, extract::State, middleware, response::Response};

use crate::{
    error::ErrorPage,
    state::AppState,
    templates::{ErrorTemplate, render},
};

mod admin;
mod auth;
mod dashboard;
mod ranking;

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(admin::router())
        .merge(dashboard::router())
        .merge(auth::router())
        .merge(ranking::router())
        .layer(middleware::map_response_with_state(
            state.clone(),
            render_error_page,
        ))
        .with_state(state)
}

/// Replaces the plain-text body of an `AppError` response with the HTML error
/// page.
async fn render_error_page(State(state): State<AppState>, response: Response) -> Response {
    let Some(page) = response.extensions().get::<ErrorPage>().cloned() else {
        return response;
    };

    match render(ErrorTemplate {
        site_name: state.config.server.site_name.as_ref(),
        status: page.status,
        title: page.title,
        message: &page.message,
    }) {
        Ok(mut rendered) => {
            *rendered.status_mut() = page.status;
            rendered
        },
        Err(error) => {
            tracing::error!(error = %error, "render error page");
            response
        },
    }
}
