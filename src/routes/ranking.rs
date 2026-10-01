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
    clients::keeper::{RankingMetric, RankingPeriod},
    error::{AppError, AppResult},
    services::{
        ranking::{self, PortalLeaderboardEntry},
        user::current_user,
    },
    state::AppState,
    templates::{RankingComponent, RankingEntry, RankingOption, RankingTemplate, render},
};

const DEFAULT_PERIOD: RankingPeriod = RankingPeriod::Today;
const DEFAULT_METRIC: RankingMetric = RankingMetric::TotalTokens;

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

    let period = parse_period(query.period.as_deref())?;
    let metric = parse_metric(query.metric.as_deref())?;
    let leaderboard = ranking::local_leaderboard(&state, period, metric, None, false).await?;

    let generated_at = leaderboard
        .generated_at
        .format(&Rfc3339)
        .map_err(|error| AppError::Other(anyhow::anyhow!(error)))?;
    let entries: Vec<RankingEntry> = leaderboard
        .entries
        .iter()
        .map(|entry| map_entry(entry, metric))
        .collect();
    let score_explanation = if metric == RankingMetric::Overall {
        "Overall combines all metrics into one score."
    } else {
        ""
    };
    let periods = period_options(period);
    let metrics = metric_options(metric);
    let empty_message = empty_message(period, metric);

    let mut response = render(RankingTemplate {
        site_name: state.config.server.site_name.as_ref(),
        period_label: period_label(period),
        metric_label: metric_label(metric),
        periods: &periods,
        metrics: &metrics,
        generated_at: &generated_at,
        stale: leaderboard.stale,
        score_explanation,
        entries: &entries,
        empty_message,
    })?;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn parse_period(value: Option<&str>) -> AppResult<RankingPeriod> {
    value
        .unwrap_or(DEFAULT_PERIOD.as_ref())
        .parse()
        .map_err(|_| AppError::BadRequest("invalid ranking period".into()))
}

fn parse_metric(value: Option<&str>) -> AppResult<RankingMetric> {
    value
        .unwrap_or(DEFAULT_METRIC.as_ref())
        .parse()
        .map_err(|_| AppError::BadRequest("invalid ranking metric".into()))
}
fn period_label(period: RankingPeriod) -> &'static str {
    match period {
        RankingPeriod::Today => "Today",
        RankingPeriod::Yesterday => "Yesterday",
        RankingPeriod::CurrentMonth => "Current month",
        RankingPeriod::PreviousMonth => "Previous month",
    }
}

fn metric_label(metric: RankingMetric) -> &'static str {
    match metric {
        RankingMetric::Overall => "Overall",
        RankingMetric::TotalTokens => "Total tokens",
        RankingMetric::RequestCount => "Requests",
        RankingMetric::CacheReadRate => "Cache-read rate",
        RankingMetric::TTFTAverage => "Average TTFT",
        RankingMetric::LatencyAverage => "Average latency",
        RankingMetric::PeakTPM => "Peak TPM",
        RankingMetric::PeakRPM => "Peak RPM",
    }
}

fn period_options(selected: RankingPeriod) -> Vec<RankingOption> {
    [
        RankingPeriod::Today,
        RankingPeriod::Yesterday,
        RankingPeriod::CurrentMonth,
        RankingPeriod::PreviousMonth,
    ]
    .into_iter()
    .map(|value| RankingOption {
        value: value.as_ref().to_owned(),
        label: period_label(value).to_owned(),
        selected: value == selected,
    })
    .collect()
}

fn metric_options(selected: RankingMetric) -> Vec<RankingOption> {
    [
        RankingMetric::Overall,
        RankingMetric::TotalTokens,
        RankingMetric::RequestCount,
        RankingMetric::CacheReadRate,
        RankingMetric::TTFTAverage,
        RankingMetric::LatencyAverage,
        RankingMetric::PeakTPM,
        RankingMetric::PeakRPM,
    ]
    .into_iter()
    .map(|value| RankingOption {
        value: value.as_ref().to_owned(),
        label: metric_label(value).to_owned(),
        selected: value == selected,
    })
    .collect()
}

fn map_entry(entry: &PortalLeaderboardEntry, metric: RankingMetric) -> RankingEntry {
    let components = if metric == RankingMetric::Overall {
        entry
            .metrics
            .as_ref()
            .map(|metrics| {
                [
                    ("tokens", metrics.total_tokens, RankingMetric::TotalTokens),
                    (
                        "requests",
                        metrics.request_count,
                        RankingMetric::RequestCount,
                    ),
                    (
                        "cache",
                        metrics.cache_read_rate,
                        RankingMetric::CacheReadRate,
                    ),
                    ("TTFT", metrics.ttft_average, RankingMetric::TTFTAverage),
                    (
                        "latency",
                        metrics.latency_average,
                        RankingMetric::LatencyAverage,
                    ),
                    ("TPM", metrics.peak_tpm, RankingMetric::PeakTPM),
                    ("RPM", metrics.peak_rpm, RankingMetric::PeakRPM),
                ]
                .into_iter()
                .map(|(label, value, component_metric)| RankingComponent {
                    label: label.to_owned(),
                    value: format_metric(value, component_metric),
                })
                .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    RankingEntry {
        rank: entry.rank.to_string(),
        avatar_url: entry.user.avatar_url.clone(),
        display_name: entry.user.github_login.clone(),
        secondary_name: entry.user.github_name.clone(),
        value: format_metric(entry.value, metric),
        components,
    }
}

fn format_metric(value: i64, metric: RankingMetric) -> String {
    match metric {
        RankingMetric::CacheReadRate => format_scaled_percentage(value),
        RankingMetric::TTFTAverage | RankingMetric::LatencyAverage => {
            format!("{:.3} ms", value as f64 / 1_000.0)
        },
        RankingMetric::PeakTPM | RankingMetric::PeakRPM => {
            format!("{:.2}", value as f64 / 5.0)
        },
        RankingMetric::Overall => format_integer(value),
        RankingMetric::TotalTokens | RankingMetric::RequestCount => format_integer(value),
    }
}

fn format_scaled_percentage(value: i64) -> String {
    format!("{:.2}%", value as f64 / 10_000.0)
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

fn empty_message(period: RankingPeriod, metric: RankingMetric) -> &'static str {
    if period == RankingPeriod::PreviousMonth
        && matches!(
            metric,
            RankingMetric::Overall | RankingMetric::PeakTPM | RankingMetric::PeakRPM
        )
    {
        "This metric is not available for the previous month. Try Total tokens or Requests."
    } else {
        "No usage data is available for this selection."
    }
}
