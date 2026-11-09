//! Usage rankings of Portal users, built from Keeper's per-key, per-model
//! usage analysis over calendar days in Keeper's timezone.

use std::{collections::HashMap, fmt, str::FromStr};

use serde::{Serialize, Serializer};
use time::{Date, Duration, Month, OffsetDateTime};

use crate::{
    clients::keeper::{UsageAnalysis, UsageHeatmapCell},
    db::{self, User},
    error::{AppError, AppResult},
    services::user::decrypt_api_key,
    state::AppState,
};

/// Keeper only serves custom day ranges starting within its most recent 365
/// calendar days, today included.
const MAX_LOOKBACK_DAYS: i64 = 364;
const MAX_MATCHER_CHARS: usize = 64;
const MAX_PERIOD_NUMBER_DIGITS: usize = 4;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PeriodUnit {
    Day,
    Week,
    Month,
}

impl PeriodUnit {
    fn symbol(self) -> char {
        match self {
            Self::Day => 'd',
            Self::Week => 'w',
            Self::Month => 'm',
        }
    }

    fn from_symbol(symbol: char) -> Option<Self> {
        match symbol {
            'd' => Some(Self::Day),
            'w' => Some(Self::Week),
            'm' => Some(Self::Month),
            _ => None,
        }
    }

    /// First day of the unit `back` units before the one containing `today`.
    /// Weeks start on Monday.
    fn start(self, today: Date, back: u32) -> Option<Date> {
        match self {
            Self::Day => today.checked_sub(Duration::days(back.into())),
            Self::Week => today.checked_sub(Duration::days(
                i64::from(today.weekday().number_days_from_monday()) + 7 * i64::from(back),
            )),
            Self::Month => {
                let index = today.year() * 12 + i32::from(today.month() as u8)
                    - 1
                    - i32::try_from(back).ok()?;
                let month = Month::try_from(u8::try_from(index.rem_euclid(12) + 1).ok()?).ok()?;
                Date::from_calendar_date(index.div_euclid(12), month, 1).ok()
            },
        }
    }

    /// Last day of the unit starting at `start`.
    fn end(self, start: Date) -> Option<Date> {
        match self {
            Self::Day => Some(start),
            Self::Week => start.checked_add(Duration::days(6)),
            Self::Month => start.replace_day(start.month().length(start.year())).ok(),
        }
    }
}

/// `X` is the current day, week or month, `X-N` the one N units before it,
/// and `NX` the last N units including the current one. `1X` and `X-0` are
/// written as `X`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RankingPeriod {
    Single { unit: PeriodUnit, offset: u32 },
    Last { unit: PeriodUnit, count: u32 },
}

impl RankingPeriod {
    pub const TODAY: Self = Self::Single {
        unit: PeriodUnit::Day,
        offset: 0,
    };

    /// Inclusive local dates, never past `today`.
    fn dates(self, today: Date) -> Option<(Date, Date)> {
        match self {
            Self::Single { unit, offset } => {
                let start = unit.start(today, offset)?;
                Some((start, unit.end(start)?.min(today)))
            },
            Self::Last { unit, count } => Some((unit.start(today, count.checked_sub(1)?)?, today)),
        }
    }
}

impl fmt::Display for RankingPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Single { unit, offset: 0 } => write!(f, "{}", unit.symbol()),
            Self::Single { unit, offset } => write!(f, "{}-{offset}", unit.symbol()),
            Self::Last { unit, count } => write!(f, "{count}{}", unit.symbol()),
        }
    }
}

impl FromStr for RankingPeriod {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim().to_ascii_lowercase();
        let mut chars = value.chars();
        let first = chars.next().ok_or(())?;
        if let Some(unit) = PeriodUnit::from_symbol(first) {
            let rest = chars.as_str();
            let offset = if rest.is_empty() {
                0
            } else {
                parse_period_number(rest.strip_prefix('-').ok_or(())?)?
            };
            return Ok(Self::Single { unit, offset });
        }
        let unit = value
            .chars()
            .next_back()
            .and_then(PeriodUnit::from_symbol)
            .ok_or(())?;
        match parse_period_number(&value[..value.len() - 1])? {
            0 => Err(()),
            1 => Ok(Self::Single { unit, offset: 0 }),
            count => Ok(Self::Last { unit, count }),
        }
    }
}

impl Serialize for RankingPeriod {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

fn parse_period_number(digits: &str) -> Result<u32, ()> {
    if digits.is_empty()
        || digits.len() > MAX_PERIOD_NUMBER_DIGITS
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(());
    }
    digits.parse().map_err(|_| ())
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize)]
pub enum RankingMetric {
    #[serde(rename = "tokens")]
    Tokens,
    /// Values are micro-USD so they stay integers without rounding light
    /// users down to zero.
    #[serde(rename = "cost")]
    Cost,
    #[serde(rename = "req")]
    Requests,
    /// Cache-read share of input tokens, in parts per million.
    #[serde(rename = "cache")]
    Cache,
}

impl RankingMetric {
    pub const ALL: [Self; 4] = [Self::Tokens, Self::Cost, Self::Requests, Self::Cache];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Cost => "cost",
            Self::Requests => "req",
            Self::Cache => "cache",
        }
    }
}

impl FromStr for RankingMetric {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|metric| metric.as_str().eq_ignore_ascii_case(value))
            .ok_or(())
    }
}

#[derive(Debug, Serialize)]
pub struct Leaderboard {
    pub period: RankingPeriod,
    pub metric: RankingMetric,
    /// Inclusive `YYYY-MM-DD` dates in Keeper's timezone.
    pub start_date: String,
    pub end_date: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub entries: Vec<LeaderboardEntry>,
}

#[derive(Debug, Serialize)]
pub struct LeaderboardEntry {
    pub rank: usize,
    pub user: RankingUser,
    pub value: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<RankingModel>,
}

#[derive(Debug, Serialize)]
pub struct RankingUser {
    pub id: i64,
    pub github_id: i64,
    pub github_login: String,
    pub github_name: String,
    pub avatar_url: String,
}

#[derive(Debug, Serialize)]
pub struct RankingModel {
    pub model: String,
    /// The entry's unit, except for `cache`: rates do not add up, so models
    /// are weighed by total tokens there.
    pub value: i64,
}

#[derive(Debug, Default)]
struct Usage {
    total_tokens: i64,
    requests: i64,
    input_tokens: i64,
    cache_read_tokens: i64,
    cost_usd: f64,
}

impl Usage {
    fn add(&mut self, cell: &UsageHeatmapCell) {
        self.total_tokens += cell.total_tokens;
        self.requests += cell.requests;
        self.input_tokens += cell.input_tokens;
        self.cache_read_tokens += cell.cache_read_tokens;
        self.cost_usd += cell.cost_usd;
    }

    fn value(&self, metric: RankingMetric) -> i64 {
        match metric {
            RankingMetric::Tokens => self.total_tokens,
            RankingMetric::Cost => (self.cost_usd * 1_000_000.0).round() as i64,
            RankingMetric::Requests => self.requests,
            RankingMetric::Cache if self.input_tokens > 0 => {
                let rate =
                    i128::from(self.cache_read_tokens) * 1_000_000 / i128::from(self.input_tokens);
                rate.clamp(0, 1_000_000) as i64
            },
            RankingMetric::Cache => 0,
        }
    }

    fn model_value(&self, metric: RankingMetric) -> i64 {
        match metric {
            RankingMetric::Cache => self.total_tokens,
            _ => self.value(metric),
        }
    }

    /// Orders equal values by the volume behind them.
    fn tiebreak(&self, metric: RankingMetric) -> i64 {
        match metric {
            RankingMetric::Tokens => self.requests,
            RankingMetric::Cost | RankingMetric::Requests => self.total_tokens,
            RankingMetric::Cache => self.input_tokens,
        }
    }
}

#[derive(Debug, Default)]
struct UserUsage {
    total: Usage,
    models: HashMap<String, Usage>,
}

pub async fn leaderboard(
    state: &AppState,
    period: RankingPeriod,
    metric: RankingMetric,
    matcher: Option<&str>,
    include_models: bool,
) -> AppResult<Leaderboard> {
    let matcher = matcher.map(str::trim).filter(|value| !value.is_empty());
    if matcher.is_some_and(|value| value.chars().count() > MAX_MATCHER_CHARS) {
        return Err(AppError::BadRequest("model matcher is too long".into()));
    }

    // Keeper's "today" fixes the calendar; it is also the data for `d`.
    let (today_analysis, key_settings) = tokio::try_join!(
        state.keeper.usage_analysis(&[("range", "today")]),
        state.keeper.cpa_api_key_settings(),
    )?;
    let today = today_analysis
        .range_start
        .ok_or_else(|| AppError::Upstream("Keeper omitted today's analysis range".into()))?
        .date();
    let (start, end) = period
        .dates(today)
        .filter(|(start, _)| (today - *start).whole_days() <= MAX_LOOKBACK_DAYS)
        .ok_or_else(|| AppError::BadRequest("ranking period is out of range".into()))?;
    let (start_date, end_date) = (date_string(start), date_string(end));
    let analysis = if start == today {
        today_analysis
    } else {
        state
            .keeper
            .usage_analysis(&[
                ("range", "custom"),
                ("unit", "day"),
                ("start", &start_date),
                ("end", &end_date),
            ])
            .await?
    };

    let users_by_api_key = users_by_api_key(state).await?;
    let users_by_key_id = key_settings
        .items
        .into_iter()
        .filter_map(|key| {
            let user = users_by_api_key.get(key.api_key.as_deref()?)?;
            Some((key.id, user))
        })
        .collect::<HashMap<_, _>>();

    let mut ranked = usage_by_user(&analysis, &users_by_key_id, matcher)
        .into_values()
        .filter_map(|(user, usage)| {
            let value = usage.total.value(metric);
            (value > 0).then_some((user, value, usage))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(
        |(left_user, left_value, left), (right_user, right_value, right)| {
            right_value
                .cmp(left_value)
                .then_with(|| {
                    right
                        .total
                        .tiebreak(metric)
                        .cmp(&left.total.tiebreak(metric))
                })
                .then_with(|| left_user.github_login.cmp(&right_user.github_login))
        },
    );
    let entries = ranked
        .into_iter()
        .enumerate()
        .map(|(index, (user, value, usage))| LeaderboardEntry {
            rank: index + 1,
            user: ranking_user(user),
            value,
            models: if include_models {
                ranking_models(&usage, metric)
            } else {
                Vec::new()
            },
        })
        .collect();

    Ok(Leaderboard {
        period,
        metric,
        start_date,
        end_date,
        generated_at: OffsetDateTime::now_utc(),
        matcher: matcher.map(ToOwned::to_owned),
        entries,
    })
}

async fn users_by_api_key(state: &AppState) -> AppResult<HashMap<String, User>> {
    let mut users_by_api_key = HashMap::new();
    for user in db::list_users(&state.db).await? {
        users_by_api_key.insert(decrypt_api_key(state, &user)?, user);
    }
    Ok(users_by_api_key)
}

fn usage_by_user<'a>(
    analysis: &UsageAnalysis,
    users_by_key_id: &HashMap<String, &'a User>,
    matcher: Option<&str>,
) -> HashMap<i64, (&'a User, UserUsage)> {
    let needle = matcher.map(str::to_lowercase);
    let mut usage_by_user = HashMap::<i64, (&User, UserUsage)>::new();
    for cell in &analysis.heatmap.cells {
        if needle
            .as_ref()
            .is_some_and(|needle| !cell.model.to_lowercase().contains(needle))
        {
            continue;
        }
        let Some(&user) = users_by_key_id.get(&cell.api_key) else {
            continue;
        };
        let (_, usage) = usage_by_user
            .entry(user.id)
            .or_insert_with(|| (user, UserUsage::default()));
        usage.total.add(cell);
        usage
            .models
            .entry(cell.model.clone())
            .or_default()
            .add(cell);
    }
    usage_by_user
}

/// Every model with a positive value, largest first; consumers decide how
/// many to show.
fn ranking_models(usage: &UserUsage, metric: RankingMetric) -> Vec<RankingModel> {
    let mut models = usage
        .models
        .iter()
        .map(|(model, model_usage)| RankingModel {
            model: model.clone(),
            value: model_usage.model_value(metric),
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

fn ranking_user(user: &User) -> RankingUser {
    RankingUser {
        id: user.id,
        github_id: user.github_id,
        github_login: user.github_login.clone(),
        github_name: user.github_name.clone(),
        avatar_url: user.avatar_url.clone(),
    }
}

fn date_string(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}
