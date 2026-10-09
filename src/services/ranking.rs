use std::collections::HashMap;

use serde::Serialize;
use time::{Date, Duration, Month, OffsetDateTime};

use crate::{
    clients::keeper::{LocalLeaderboardEntry, RankingMetric, RankingPeriod, UsageAnalysis},
    db::{self, User},
    error::{AppError, AppResult},
    services::user::decrypt_api_key,
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct PortalLeaderboard {
    pub period: RankingPeriod,
    pub period_key: String,
    pub metric: RankingMetric,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub entries: Vec<PortalLeaderboardEntry>,
}

#[derive(Debug, Serialize)]
pub struct PortalLeaderboardEntry {
    pub rank: usize,
    pub user: PortalRankingUser,
    pub value: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_numerator: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_denominator: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<PortalLeaderboardMetrics>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<PortalRankingModel>,
}

#[derive(Debug, Serialize)]
pub struct PortalRankingUser {
    pub id: i64,
    pub github_id: i64,
    pub github_login: String,
    pub github_name: String,
    pub avatar_url: String,
}

#[derive(Debug, Serialize)]
pub struct PortalLeaderboardMetrics {
    pub total_tokens: i64,
    pub request_count: i64,
    pub cache_read_rate: i64,
    pub ttft_average: i64,
    pub latency_average: i64,
    pub peak_tpm: i64,
    pub peak_rpm: i64,
}

#[derive(Debug, Serialize)]
pub struct PortalRankingModel {
    pub model: String,
    /// Same unit as the entry value: tokens, or micro-USD for `cost`.
    pub value: i64,
}

#[derive(Debug, Default)]
struct UserModelUsage {
    total_tokens: i64,
    total_requests: i64,
    cost_usd: f64,
    models: HashMap<String, ModelUsage>,
}

#[derive(Debug, Default)]
struct ModelUsage {
    total_tokens: i64,
    cost_usd: f64,
}

impl UserModelUsage {
    fn value(&self, metric: RankingMetric) -> i64 {
        metric_value(metric, self.total_tokens, self.cost_usd)
    }
}

impl ModelUsage {
    fn value(&self, metric: RankingMetric) -> i64 {
        metric_value(metric, self.total_tokens, self.cost_usd)
    }
}

/// Cost is reported in micro-USD so it stays an integer like the other
/// ranking values without rounding light users down to zero.
fn metric_value(metric: RankingMetric, total_tokens: i64, cost_usd: f64) -> i64 {
    if metric == RankingMetric::Cost {
        (cost_usd * 1_000_000.0).round() as i64
    } else {
        total_tokens
    }
}

pub async fn local_leaderboard(
    state: &AppState,
    period: RankingPeriod,
    metric: RankingMetric,
    matcher: Option<&str>,
    include_models: bool,
) -> AppResult<PortalLeaderboard> {
    let matcher = matcher.map(str::trim).filter(|value| !value.is_empty());
    if matcher.is_some_and(|value| value.chars().count() > 64) {
        return Err(AppError::BadRequest("model matcher is too long".into()));
    }
    if matcher.is_some() && !matches!(metric, RankingMetric::TotalTokens | RankingMetric::Cost) {
        return Err(AppError::BadRequest(
            "model ranking only supports total_tokens and cost".into(),
        ));
    }
    // Model-filtered and cost rankings are built from Keeper's usage analysis;
    // the token leaderboard still supplies the period key and staleness.
    let from_analysis = matcher.is_some() || metric == RankingMetric::Cost;
    let keeper_metric = if metric == RankingMetric::Cost {
        RankingMetric::TotalTokens
    } else {
        metric
    };

    let (leaderboard, key_settings) = tokio::try_join!(
        state.keeper.local_leaderboard(period, keeper_metric),
        state.keeper.cpa_api_key_settings(),
    )?;
    let needs_analysis = from_analysis || (include_models && metric == RankingMetric::TotalTokens);
    let analysis = if needs_analysis {
        match load_analysis(state, period, &leaderboard.period_key).await {
            Ok(analysis) => Some(analysis),
            Err(_) if !from_analysis => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let users_by_api_key = users_by_api_key(state).await?;
    let users_by_key_id = key_settings
        .items
        .iter()
        .filter_map(|key| {
            users_by_api_key
                .get(&key.api_key)
                .cloned()
                .map(|user| (key.id.clone(), user))
        })
        .collect::<HashMap<_, _>>();
    let user_ids_by_key = users_by_key_id
        .iter()
        .map(|(key_id, user)| (key_id.clone(), user.id))
        .collect::<HashMap<_, _>>();
    let users_by_id = users_by_key_id
        .values()
        .map(|user| (user.id, user))
        .collect::<HashMap<_, _>>();
    let mut model_usage = analysis
        .as_ref()
        .map(|analysis| build_model_usage(analysis, &user_ids_by_key, matcher));

    if from_analysis {
        let entries = model_usage
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(user_id, usage)| {
                let user = users_by_id.get(&user_id).copied()?;
                let value = usage.value(metric);
                (value > 0).then(|| {
                    let tiebreak = if metric == RankingMetric::Cost {
                        usage.total_tokens
                    } else {
                        usage.total_requests
                    };
                    let models = if include_models {
                        ranking_models(&usage, metric)
                    } else {
                        Vec::new()
                    };
                    (analysis_entry(user, value, models), tiebreak)
                })
            })
            .collect::<Vec<_>>();
        return Ok(PortalLeaderboard {
            period: leaderboard.period,
            period_key: leaderboard.period_key,
            metric,
            generated_at: OffsetDateTime::now_utc(),
            stale: leaderboard.stale,
            matcher: matcher.map(ToOwned::to_owned),
            entries: rank_analysis_entries(entries),
        });
    }

    let entries = leaderboard
        .entries
        .into_iter()
        .filter_map(|entry| portal_entry(entry, &users_by_key_id))
        .map(|mut entry| {
            entry.models = model_usage
                .as_ref()
                .and_then(|usage| usage.get(&entry.user.id))
                .map(|usage| ranking_models(usage, metric))
                .unwrap_or_default();
            entry
        })
        .enumerate()
        .map(|(index, mut entry)| {
            entry.rank = index + 1;
            entry
        })
        .collect();

    Ok(PortalLeaderboard {
        period: leaderboard.period,
        period_key: leaderboard.period_key,
        metric: leaderboard.metric,
        generated_at: leaderboard.generated_at,
        stale: leaderboard.stale,
        matcher: None,
        entries,
    })
}

async fn load_analysis(
    state: &AppState,
    period: RankingPeriod,
    period_key: &str,
) -> AppResult<UsageAnalysis> {
    let today = if period == RankingPeriod::CurrentMonth {
        let probe = state.keeper.usage_analysis(&[("range", "today")]).await?;
        Some(
            probe
                .range_start
                .ok_or_else(|| AppError::Upstream("Keeper omitted today's analysis range".into()))?
                .date(),
        )
    } else {
        None
    };
    let query = analysis_query(period, period_key, today)?;
    let query_refs = query
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    state.keeper.usage_analysis(&query_refs).await
}
async fn users_by_api_key(state: &AppState) -> AppResult<HashMap<String, User>> {
    let mut users_by_api_key = HashMap::new();
    for user in db::list_users(&state.db).await? {
        users_by_api_key.insert(decrypt_api_key(state, &user)?, user);
    }
    Ok(users_by_api_key)
}

fn portal_entry(
    entry: LocalLeaderboardEntry,
    users: &HashMap<String, User>,
) -> Option<PortalLeaderboardEntry> {
    let user = users.get(&entry.participant_id)?;
    Some(PortalLeaderboardEntry {
        rank: 0,
        user: portal_user(user),
        value: entry.value,
        rate_numerator: entry.rate_numerator,
        rate_denominator: entry.rate_denominator,
        metrics: entry.metrics.map(|metrics| PortalLeaderboardMetrics {
            total_tokens: metrics.total_tokens,
            request_count: metrics.request_count,
            cache_read_rate: metrics.cache_read_rate,
            ttft_average: metrics.ttft_average,
            latency_average: metrics.latency_average,
            peak_tpm: metrics.peak_tpm,
            peak_rpm: metrics.peak_rpm,
        }),
        models: Vec::new(),
    })
}

fn analysis_entry(
    user: &User,
    value: i64,
    models: Vec<PortalRankingModel>,
) -> PortalLeaderboardEntry {
    PortalLeaderboardEntry {
        rank: 0,
        user: portal_user(user),
        value,
        rate_numerator: None,
        rate_denominator: None,
        metrics: None,
        models,
    }
}

fn portal_user(user: &User) -> PortalRankingUser {
    PortalRankingUser {
        id: user.id,
        github_id: user.github_id,
        github_login: user.github_login.clone(),
        github_name: user.github_name.clone(),
        avatar_url: user.avatar_url.clone(),
    }
}

fn rank_analysis_entries(
    mut entries: Vec<(PortalLeaderboardEntry, i64)>,
) -> Vec<PortalLeaderboardEntry> {
    entries.sort_by(|(left, left_tiebreak), (right, right_tiebreak)| {
        right
            .value
            .cmp(&left.value)
            .then_with(|| right_tiebreak.cmp(left_tiebreak))
            .then_with(|| left.user.github_login.cmp(&right.user.github_login))
    });
    entries
        .into_iter()
        .enumerate()
        .map(|(index, (mut entry, _))| {
            entry.rank = index + 1;
            entry
        })
        .collect()
}

fn build_model_usage(
    analysis: &UsageAnalysis,
    user_ids_by_key: &HashMap<String, i64>,
    matcher: Option<&str>,
) -> HashMap<i64, UserModelUsage> {
    let needle = matcher.map(str::to_lowercase);
    let mut usage_by_user = HashMap::<i64, UserModelUsage>::new();
    for cell in &analysis.heatmap.cells {
        if needle
            .as_ref()
            .is_some_and(|needle| !cell.model.to_lowercase().contains(needle))
        {
            continue;
        }
        let Some(&user_id) = user_ids_by_key.get(&cell.api_key) else {
            continue;
        };
        let usage = usage_by_user.entry(user_id).or_default();
        usage.total_tokens += cell.total_tokens;
        usage.total_requests += cell.requests;
        usage.cost_usd += cell.cost_usd;
        let model = usage.models.entry(cell.model.clone()).or_default();
        model.total_tokens += cell.total_tokens;
        model.cost_usd += cell.cost_usd;
    }
    usage_by_user
}

/// Every model with a positive value, largest first; consumers decide how
/// many to show.
fn ranking_models(usage: &UserModelUsage, metric: RankingMetric) -> Vec<PortalRankingModel> {
    let mut models = usage
        .models
        .iter()
        .map(|(model, model_usage)| PortalRankingModel {
            model: model.clone(),
            value: model_usage.value(metric),
        })
        .filter(|model| model.value > 0)
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then_with(|| left.model.cmp(&right.model))
    });
    models
}

fn analysis_query(
    period: RankingPeriod,
    period_key: &str,
    today: Option<Date>,
) -> AppResult<Vec<(String, String)>> {
    match period {
        RankingPeriod::Today | RankingPeriod::Yesterday => {
            Ok(vec![("range".into(), period.as_ref().into())])
        },
        RankingPeriod::CurrentMonth => custom_month_query(period_key, true, today),
        RankingPeriod::PreviousMonth => custom_month_query(period_key, false, today),
    }
}

fn custom_month_query(
    period_key: &str,
    current: bool,
    today: Option<Date>,
) -> AppResult<Vec<(String, String)>> {
    let (year, month) = period_key.split_once('-').ok_or_else(|| {
        AppError::Upstream("Keeper returned an invalid ranking period key".into())
    })?;
    let year = year
        .parse::<i32>()
        .map_err(|_| AppError::Upstream("Keeper returned an invalid ranking year".into()))?;
    let month = month
        .parse::<u8>()
        .ok()
        .and_then(|value| Month::try_from(value).ok())
        .ok_or_else(|| AppError::Upstream("Keeper returned an invalid ranking month".into()))?;
    let first = Date::from_calendar_date(year, month, 1)
        .map_err(|_| AppError::Upstream("Keeper returned an invalid ranking date".into()))?;
    let next_first = if month == Month::December {
        Date::from_calendar_date(year + 1, Month::January, 1)
    } else {
        Date::from_calendar_date(year, month.next(), 1)
    }
    .map_err(|_| AppError::Upstream("Keeper returned an invalid ranking date".into()))?;
    let last = next_first - Duration::days(1);
    let end = if current {
        today
            .ok_or_else(|| AppError::Upstream("Keeper omitted today's analysis range".into()))?
            .max(first)
            .min(last)
    } else {
        last
    };
    Ok(vec![
        ("range".into(), "custom".into()),
        ("unit".into(), "day".into()),
        ("start".into(), date_string(first)),
        ("end".into(), date_string(end)),
    ])
}

fn date_string(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}
