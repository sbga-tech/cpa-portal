use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    admin_api::{AdminApiAuthenticated, AdminApiError},
    clients::keeper::{RankingMetric, RankingPeriod},
    db,
    error::AppError,
    services::{quota, ranking},
    state::AppState,
};

const DEFAULT_PERIOD: RankingPeriod = RankingPeriod::Today;
const DEFAULT_METRIC: RankingMetric = RankingMetric::TotalTokens;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/users", get(users))
        .route("/api/admin/v1/ranking", get(local_ranking))
        .route("/api/admin/v1/quota", get(quota_snapshot))
        .route("/api/admin/v1/quota/routing", get(quota_routing))
        .route(
            "/api/admin/v1/quota/history/{auth_index}",
            get(quota_history),
        )
        .route(
            "/api/admin/v1/quota/reset-credits/{auth_index}",
            get(quota_reset_credits),
        )
}

#[derive(Debug, Deserialize)]
struct RankingQuery {
    period: Option<String>,
    metric: Option<String>,
}

#[derive(Deserialize)]
struct QuotaHistoryQuery {
    window_role: Option<String>,
}

#[derive(Serialize)]
struct UsersResponse {
    generated_at: String,
    users: Vec<AdminUser>,
}

#[derive(Serialize)]
struct AdminUser {
    id: i64,
    github_id: i64,
    github_login: String,
    github_name: String,
    avatar_url: String,
    created_at: String,
    last_login_at: String,
}

async fn users(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
) -> Result<Response, AdminApiError> {
    let users = db::list_users(&state.db)
        .await
        .map_err(|error| {
            AdminApiError::internal(
                "database_error",
                "Could not list Portal users.",
                "list admin API users",
                error,
            )
        })?
        .into_iter()
        .map(|user| {
            Ok(AdminUser {
                id: user.id,
                github_id: user.github_id,
                github_login: user.github_login,
                github_name: user.github_name,
                avatar_url: user.avatar_url,
                created_at: unix_timestamp(user.created_at)?,
                last_login_at: unix_timestamp(user.last_login_at)?,
            })
        })
        .collect::<Result<Vec<_>, AdminApiError>>()?;

    json_response(UsersResponse {
        generated_at: format_time(OffsetDateTime::now_utc())?,
        users,
    })
}

async fn local_ranking(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
    Query(query): Query<RankingQuery>,
) -> Result<Response, AdminApiError> {
    let period = query
        .period
        .as_deref()
        .unwrap_or(DEFAULT_PERIOD.as_ref())
        .parse()
        .map_err(|_| {
            AdminApiError::bad_request("invalid_period", "The ranking period is invalid.")
        })?;
    let metric = query
        .metric
        .as_deref()
        .unwrap_or(DEFAULT_METRIC.as_ref())
        .parse()
        .map_err(|_| {
            AdminApiError::bad_request("invalid_metric", "The ranking metric is invalid.")
        })?;
    let leaderboard = ranking::local_leaderboard(&state, period, metric)
        .await
        .map_err(|error| {
            map_app_error(
                "local_ranking_failed",
                "Could not load the local ranking.",
                "load admin API ranking",
                error,
            )
        })?;
    json_response(leaderboard)
}

async fn quota_snapshot(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
) -> Result<Response, AdminApiError> {
    let snapshot = quota::snapshot(&state).await.map_err(|error| {
        map_app_error(
            "quota_failed",
            "Could not load quota data.",
            "load admin API quota snapshot",
            error,
        )
    })?;
    json_response(snapshot)
}

async fn quota_routing(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
) -> Result<Response, AdminApiError> {
    let snapshot = quota::routing_snapshot(&state).await.map_err(|error| {
        map_app_error(
            "quota_routing_failed",
            "Could not load quota routing data.",
            "load admin API quota routing snapshot",
            error,
        )
    })?;
    json_response(snapshot)
}

async fn quota_history(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
    Path(auth_index): Path<String>,
    Query(query): Query<QuotaHistoryQuery>,
) -> Result<Response, AdminApiError> {
    let history = state
        .keeper
        .quota_history(&auth_index, query.window_role.as_deref())
        .await
        .map_err(|error| {
            map_app_error(
                "quota_history_failed",
                "Could not load quota history.",
                "load admin API quota history",
                error,
            )
        })?;
    json_response(history)
}

async fn quota_reset_credits(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
    Path(auth_index): Path<String>,
) -> Result<Response, AdminApiError> {
    let credits = state
        .keeper
        .quota_reset_credits(&auth_index)
        .await
        .map_err(|error| {
            map_app_error(
                "quota_reset_credits_failed",
                "Could not load quota reset credits.",
                "load admin API quota reset credits",
                error,
            )
        })?;
    json_response(credits)
}

fn json_response(value: impl Serialize) -> Result<Response, AdminApiError> {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn unix_timestamp(value: i64) -> Result<String, AdminApiError> {
    let timestamp = OffsetDateTime::from_unix_timestamp(value).map_err(|error| {
        AdminApiError::internal(
            "invalid_timestamp",
            "Portal user data contains an invalid timestamp.",
            "format admin API user timestamp",
            error,
        )
    })?;
    format_time(timestamp)
}

fn format_time(value: OffsetDateTime) -> Result<String, AdminApiError> {
    value.format(&Rfc3339).map_err(|error| {
        AdminApiError::internal(
            "timestamp_format_failed",
            "Could not format a response timestamp.",
            "format admin API timestamp",
            error,
        )
    })
}

fn map_app_error(
    code: &'static str,
    message: &'static str,
    operation: &'static str,
    error: AppError,
) -> AdminApiError {
    match error {
        AppError::BadRequest(_) => AdminApiError::bad_request(code, message),
        AppError::Upstream(_) | AppError::Http(_) => {
            AdminApiError::upstream(code, message, operation, error)
        },
        _ => AdminApiError::internal(code, message, operation, error),
    }
}
