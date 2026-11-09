use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderValue, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use tower_sessions::Session;

use crate::{
    error::{AppError, AppResult},
    services::{
        ranking::{self, LeaderboardEntry, RankingMetric, RankingPeriod},
        user::current_user,
    },
    state::AppState,
    templates::{RankingEntry, RankingOption, RankingTemplate, render},
};

/// Periods offered in the selector; any other valid period still works
/// through the query string.
const PERIOD_OPTIONS: [(&str, &str); 8] = [
    ("d", "Today"),
    ("d-1", "Yesterday"),
    ("w", "This week"),
    ("w-1", "Last week"),
    ("m", "This month"),
    ("m-1", "Last month"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
];

pub fn router() -> Router<AppState> {
    Router::new().route("/ranking", get(index))
}

#[derive(Debug, Deserialize)]
struct RankingQuery {
    period: Option<String>,
    metric: Option<String>,
}

async fn index(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<RankingQuery>,
) -> AppResult<Response> {
    if current_user(&state, &session).await?.is_none() {
        return Ok(axum::response::Redirect::to("/").into_response());
    }

    let period = match query.period.as_deref() {
        Some(value) => value
            .parse()
            .map_err(|()| AppError::BadRequest("invalid ranking period".into()))?,
        None => RankingPeriod::TODAY,
    };
    let metric = match query.metric.as_deref() {
        Some(value) => value
            .parse()
            .map_err(|()| AppError::BadRequest("invalid ranking metric".into()))?,
        None => RankingMetric::Tokens,
    };
    let leaderboard = ranking::leaderboard(&state, period, metric, None, false).await?;

    let generated_at = leaderboard
        .generated_at
        .format(&Rfc3339)
        .map_err(|error| AppError::Other(anyhow::anyhow!(error)))?;
    let entries: Vec<RankingEntry> = leaderboard
        .entries
        .iter()
        .map(|entry| map_entry(entry, metric))
        .collect();
    let period_value = period.to_string();
    let period_label = PERIOD_OPTIONS
        .iter()
        .find(|(value, _)| *value == period_value)
        .map_or(period_value.as_str(), |(_, label)| label);
    let date_range = if leaderboard.start_date == leaderboard.end_date {
        leaderboard.start_date.clone()
    } else {
        format!("{} ~ {}", leaderboard.start_date, leaderboard.end_date)
    };
    let periods = period_options(&period_value);
    let metrics = metric_options(metric);

    let mut response = render(RankingTemplate {
        site_name: state.config.server.site_name.as_ref(),
        period_label,
        metric_label: metric_label(metric),
        date_range: &date_range,
        periods: &periods,
        metrics: &metrics,
        generated_at: &generated_at,
        entries: &entries,
    })?;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn metric_label(metric: RankingMetric) -> &'static str {
    match metric {
        RankingMetric::Tokens => "Total tokens",
        RankingMetric::Cost => "Estimated cost",
        RankingMetric::Requests => "Requests",
        RankingMetric::Cache => "Cache-read rate",
    }
}

/// The preset periods, plus the current one when it came from the query
/// string, so changing the metric keeps the period.
fn period_options(selected: &str) -> Vec<RankingOption> {
    let mut options = PERIOD_OPTIONS
        .into_iter()
        .map(|(value, label)| RankingOption {
            value: value.to_owned(),
            label: label.to_owned(),
            selected: value == selected,
        })
        .collect::<Vec<_>>();
    if !options.iter().any(|option| option.selected) {
        options.push(RankingOption {
            value: selected.to_owned(),
            label: selected.to_owned(),
            selected: true,
        });
    }
    options
}

fn metric_options(selected: RankingMetric) -> Vec<RankingOption> {
    RankingMetric::ALL
        .into_iter()
        .map(|value| RankingOption {
            value: value.as_str().to_owned(),
            label: metric_label(value).to_owned(),
            selected: value == selected,
        })
        .collect()
}

fn map_entry(entry: &LeaderboardEntry, metric: RankingMetric) -> RankingEntry {
    RankingEntry {
        rank: entry.rank.to_string(),
        avatar_url: entry.user.avatar_url.clone(),
        display_name: entry.user.github_login.clone(),
        secondary_name: entry.user.github_name.clone(),
        value: format_metric(entry.value, metric),
    }
}

fn format_metric(value: i64, metric: RankingMetric) -> String {
    match metric {
        RankingMetric::Tokens | RankingMetric::Requests => format_integer(value),
        RankingMetric::Cost => format_micro_usd(value),
        RankingMetric::Cache => format!("{:.2}%", value as f64 / 10_000.0),
    }
}

fn format_micro_usd(value: i64) -> String {
    let cents = (value + 5_000) / 10_000;
    format!("${}.{:02}", format_integer(cents / 100), cents % 100)
}

fn format_integer(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}
