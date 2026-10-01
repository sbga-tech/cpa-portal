use std::collections::HashMap;

use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::warn;

use crate::{
    clients::keeper::{UsageEventExportItem, UsageIdentity},
    error::{AppError, AppResult},
    state::AppState,
};

const AUTH_TYPE_AUTH_FILE: i32 = 1;
const WEEKLY_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
const MIN_OBSERVED_BURN_PERCENT: i64 = 5;
const REFRESH_INTERVAL_SECONDS: u64 = 10 * 60;
const CURRENT_HISTORY_MAX_AGE_SECONDS: i64 = 30 * 60;
const MAX_EVENT_RANGE_SECONDS: i64 = 30 * 24 * 60 * 60;
const SHORT_EVENT_RANGE_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Deserialize)]
struct QuotaHistory {
    cycles: Vec<QuotaHistoryCycle>,
}

#[derive(Debug, Deserialize)]
struct QuotaHistoryCycle {
    status: String,
    window_seconds: i64,
    #[serde(with = "time::serde::rfc3339")]
    first_observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_observed_at: OffsetDateTime,
    transitions: Vec<QuotaHistoryTransition>,
}

#[derive(Debug, Deserialize)]
struct QuotaHistoryTransition {
    from_remaining_percent: i64,
    to_remaining_percent: i64,
    #[serde(with = "time::serde::rfc3339")]
    interval_started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    interval_ended_at: OffsetDateTime,
}

#[derive(Debug)]
struct ParsedUsageEvent {
    timestamp: OffsetDateTime,
    auth_index: String,
    total_tokens: i64,
}

#[derive(Debug)]
struct CycleCandidate {
    status: String,
    burn_percent: i64,
    intervals: Vec<(OffsetDateTime, OffsetDateTime)>,
}

pub async fn run(state: AppState) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(REFRESH_INTERVAL_SECONDS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match refresh(&state).await {
            Ok(estimates) => {
                let mut cache = state.token_estimates.write().await;
                cache.values = estimates;
                cache.refreshed_at = Some(OffsetDateTime::now_utc());
            },
            Err(error) => warn!(error = %error, "quota token estimate refresh failed"),
        }
    }
}

async fn refresh(state: &AppState) -> AppResult<HashMap<String, f64>> {
    let identities = state
        .keeper
        .usage_identities()
        .await?
        .identities
        .into_iter()
        .filter(|identity| {
            identity.auth_type == AUTH_TYPE_AUTH_FILE
                && !identity.disabled
                && matches!(
                    identity.identity_type.to_ascii_lowercase().as_str(),
                    "codex" | "claude"
                )
        })
        .collect::<Vec<_>>();

    let mut estimates = HashMap::new();
    for identity in identities {
        if let Some(estimate) = estimate_identity(state, &identity).await? {
            estimates.insert(identity.identity, estimate);
        }
    }
    Ok(estimates)
}

async fn estimate_identity(state: &AppState, identity: &UsageIdentity) -> AppResult<Option<f64>> {
    let now = OffsetDateTime::now_utc();
    let mut cycles = Vec::new();
    for role in ["primary", "secondary"] {
        let response = state
            .keeper
            .quota_history(&identity.identity, Some(role))
            .await
            .map_err(|_| AppError::Upstream("quota history request failed".into()))?;
        let history: QuotaHistory = serde_json::from_value(response)
            .map_err(|_| AppError::Upstream("invalid quota history response".into()))?;
        for cycle in history.cycles {
            if let Some(candidate) = parse_cycle(cycle, now) {
                cycles.push(candidate);
            }
        }
    }
    if cycles.is_empty() {
        return Ok(None);
    }

    let earliest = cycles
        .iter()
        .flat_map(|cycle| cycle.intervals.iter().map(|(start, _)| *start))
        .min()
        .ok_or_else(|| AppError::Upstream("invalid quota history intervals".into()))?;
    let age = now - earliest;
    let range = if age <= time::Duration::seconds(SHORT_EVENT_RANGE_SECONDS) {
        "7d"
    } else if age <= time::Duration::seconds(MAX_EVENT_RANGE_SECONDS) {
        "30d"
    } else {
        return Ok(None);
    };
    let export = state
        .keeper
        .usage_events_export(range, &identity.identity)
        .await?;
    let events = parse_events(export.events)?;
    let mut samples = Vec::new();
    let mut current = None;
    for cycle in cycles {
        let tokens = cycle_tokens(&events, &identity.identity, &cycle)?;
        if tokens <= 0 {
            continue;
        }
        let sample = (
            tokens as f64 / cycle.burn_percent as f64,
            cycle.burn_percent,
        );
        if cycle.status == "current" && cycle.burn_percent >= MIN_OBSERVED_BURN_PERCENT {
            if current
                .as_ref()
                .is_none_or(|(_, burn)| *burn < cycle.burn_percent)
            {
                current = Some(sample);
            }
        } else if cycle.status == "completed" {
            samples.push(sample);
        }
    }
    if let Some((tokens_per_percent, _)) = current {
        return Ok(tokens_per_percent.is_finite().then_some(tokens_per_percent));
    }
    if samples.is_empty() {
        return Ok(None);
    }
    samples.sort_by(|left, right| left.0.total_cmp(&right.0));
    let total_weight = samples.iter().map(|(_, burn)| *burn).sum::<i64>();
    let midpoint = (total_weight + 1) / 2;
    let mut weight = 0;
    for (tokens_per_percent, burn) in samples {
        weight += burn;
        if weight >= midpoint {
            return Ok(tokens_per_percent.is_finite().then_some(tokens_per_percent));
        }
    }
    Ok(None)
}

fn parse_cycle(cycle: QuotaHistoryCycle, now: OffsetDateTime) -> Option<CycleCandidate> {
    if cycle.window_seconds != WEEKLY_WINDOW_SECONDS
        || cycle.first_observed_at > cycle.last_observed_at
        || !matches!(cycle.status.as_str(), "current" | "completed")
    {
        return None;
    }
    if cycle.status == "current"
        && (now < cycle.last_observed_at
            || now - cycle.last_observed_at
                > time::Duration::seconds(CURRENT_HISTORY_MAX_AGE_SECONDS))
    {
        return None;
    }
    let mut burn_percent = 0;
    let mut intervals = Vec::new();
    for transition in cycle.transitions {
        if !(0..=100).contains(&transition.from_remaining_percent)
            || !(0..=100).contains(&transition.to_remaining_percent)
            || transition.interval_started_at >= transition.interval_ended_at
            || transition.interval_ended_at > now
            || transition.interval_started_at
                < now - time::Duration::seconds(MAX_EVENT_RANGE_SECONDS)
        {
            return None;
        }
        if transition.to_remaining_percent >= transition.from_remaining_percent {
            continue;
        }
        burn_percent += transition.from_remaining_percent - transition.to_remaining_percent;
        intervals.push((transition.interval_started_at, transition.interval_ended_at));
    }
    (burn_percent > 0 && !intervals.is_empty()).then_some(CycleCandidate {
        status: cycle.status,
        burn_percent,
        intervals,
    })
}

fn cycle_tokens(
    events: &[ParsedUsageEvent],
    auth_index: &str,
    cycle: &CycleCandidate,
) -> AppResult<i64> {
    events
        .iter()
        .filter(|event| {
            event.auth_index == auth_index
                && cycle
                    .intervals
                    .iter()
                    .any(|(start, end)| event.timestamp > *start && event.timestamp <= *end)
        })
        .try_fold(0i64, |total, event| {
            total
                .checked_add(event.total_tokens)
                .ok_or_else(|| AppError::Upstream("usage event token total overflow".into()))
        })
}

fn parse_events(events: Vec<UsageEventExportItem>) -> AppResult<Vec<ParsedUsageEvent>> {
    events
        .into_iter()
        .map(|event| {
            let timestamp = OffsetDateTime::parse(&event.timestamp, &Rfc3339)
                .map_err(|_| AppError::Upstream("invalid usage event timestamp".into()))?;
            if event.auth_index.trim().is_empty() || event.total_tokens < 0 {
                return Err(AppError::Upstream("invalid usage event data".into()));
            }
            Ok(ParsedUsageEvent {
                timestamp,
                auth_index: event.auth_index,
                total_tokens: event.total_tokens,
            })
        })
        .collect()
}
