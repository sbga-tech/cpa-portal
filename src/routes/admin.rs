use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    admin_api::AdminApiAuthenticated,
    api_error::ApiError,
    db,
    error::AppError,
    services::{
        quota,
        ranking::{self, RankingMetric, RankingPeriod},
    },
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/users", get(users))
        .route("/api/admin/v1/ranking", get(ranking))
        .route("/api/admin/v1/users/{github_id}/usage", get(user_usage))
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
    matcher: Option<String>,
}

#[derive(Deserialize)]
struct QuotaHistoryQuery {
    window_role: Option<String>,
}

#[derive(Deserialize)]
struct UsageQuery {
    period: Option<String>,
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
) -> Result<Response, ApiError> {
    let users = db::list_users(&state.db)
        .await
        .map_err(|error| {
            ApiError::internal(
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
        .collect::<Result<Vec<_>, ApiError>>()?;

    json_response(UsersResponse {
        generated_at: format_time(OffsetDateTime::now_utc())?,
        users,
    })
}

async fn ranking(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
    Query(query): Query<RankingQuery>,
) -> Result<Response, ApiError> {
    let period = match query.period.as_deref() {
        Some(value) => value.parse().map_err(|()| {
            ApiError::bad_request("invalid_period", "The ranking period is invalid.")
        })?,
        None => RankingPeriod::TODAY,
    };
    let metric = match query.metric.as_deref() {
        Some(value) => value.parse().map_err(|()| {
            ApiError::bad_request("invalid_metric", "The ranking metric is invalid.")
        })?,
        None => RankingMetric::Tokens,
    };
    let leaderboard = ranking::leaderboard(&state, period, metric, query.matcher.as_deref(), true)
        .await
        .map_err(|error| match error {
            AppError::BadRequest(_) => ApiError::bad_request(
                "invalid_ranking_query",
                "The ranking period or model matcher is out of range.",
            ),
            error => ApiError::from_app(
                "ranking_failed",
                "Could not load the ranking.",
                "load admin API ranking",
                error,
            ),
        })?;
    json_response(leaderboard)
}

/// The default period of a usage profile: the last seven days.
const DEFAULT_USAGE_PERIOD: RankingPeriod = RankingPeriod::Last {
    unit: ranking::PeriodUnit::Day,
    count: 7,
};

async fn user_usage(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
    Path(github_id): Path<i64>,
    Query(query): Query<UsageQuery>,
) -> Result<Response, ApiError> {
    let period = match query.period.as_deref() {
        Some(value) => value.parse().map_err(|()| {
            ApiError::bad_request("invalid_period", "The usage period is invalid.")
        })?,
        None => DEFAULT_USAGE_PERIOD,
    };
    let profile = ranking::usage_profile(&state, github_id, period)
        .await
        .map_err(|error| {
            ApiError::from_app(
                "usage_failed",
                "Could not load the usage profile.",
                "load admin API usage profile",
                error,
            )
        })?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_user",
                "No Portal user has this GitHub id.",
            )
        })?;
    json_response(profile)
}

async fn quota_snapshot(
    _auth: AdminApiAuthenticated,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let snapshot = quota::snapshot(&state).await.map_err(|error| {
        ApiError::from_app(
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
) -> Result<Response, ApiError> {
    let snapshot = quota::routing_snapshot(&state).await.map_err(|error| {
        ApiError::from_app(
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
) -> Result<Response, ApiError> {
    let history = state
        .keeper
        .quota_history(&auth_index, query.window_role.as_deref())
        .await
        .map_err(|error| {
            ApiError::from_app(
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
) -> Result<Response, ApiError> {
    let credits = state
        .keeper
        .quota_reset_credits(&auth_index)
        .await
        .map_err(|error| {
            ApiError::from_app(
                "quota_reset_credits_failed",
                "Could not load quota reset credits.",
                "load admin API quota reset credits",
                error,
            )
        })?;
    json_response(credits)
}

fn json_response(value: impl Serialize) -> Result<Response, ApiError> {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn unix_timestamp(value: i64) -> Result<String, ApiError> {
    let timestamp = OffsetDateTime::from_unix_timestamp(value).map_err(|error| {
        ApiError::internal(
            "invalid_timestamp",
            "Portal user data contains an invalid timestamp.",
            "format admin API user timestamp",
            error,
        )
    })?;
    format_time(timestamp)
}

fn format_time(value: OffsetDateTime) -> Result<String, ApiError> {
    value.format(&Rfc3339).map_err(|error| {
        ApiError::internal(
            "timestamp_format_failed",
            "Could not format a response timestamp.",
            "format admin API timestamp",
            error,
        )
    })
}
