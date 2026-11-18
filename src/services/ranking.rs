//! Usage rankings and per-user usage profiles, built from Keeper's per-key,
//! per-model usage analysis over calendar days in Keeper's timezone. Usage of
//! revoked keys still counts toward their owner.

use std::{collections::HashMap, fmt, str::FromStr};

use serde::{Serialize, Serializer};
use time::{
    Date, Duration, Month, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339,
};

use crate::{
    clients::keeper::{UsageAnalysis, UsageBucket, UsageHeatmapCell},
    db::{self, ApiKey, User},
    error::{AppError, AppResult},
    services::api_keys,
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
    output_tokens: i64,
    reasoning_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    cost_usd: f64,
}

impl Usage {
    fn add(&mut self, cell: &UsageHeatmapCell) {
        self.total_tokens += cell.total_tokens;
        self.requests += cell.requests;
        self.input_tokens += cell.input_tokens;
        self.output_tokens += cell.output_tokens;
        self.reasoning_tokens += cell.reasoning_tokens;
        self.cache_read_tokens += cell.cache_read_tokens;
        self.cache_creation_tokens += cell.cache_creation_tokens;
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
    /// By Portal key id.
    keys: HashMap<i64, Usage>,
}

/// A Portal key as Keeper knows it.
struct KeyOwner {
    user_id: i64,
    key: ApiKey,
    secret: String,
}

/// Keeper's analysis of a period with every Keeper key traced back to the
/// Portal key, revoked or not, it belongs to.
struct PeriodUsage {
    /// Keeper's current date.
    today: Date,
    start: Date,
    end: Date,
    /// Keeper's UTC offset, for day and hour boundaries.
    offset: UtcOffset,
    analysis: UsageAnalysis,
    users: HashMap<i64, User>,
    owners: HashMap<String, KeyOwner>,
}

impl PeriodUsage {
    async fn load(state: &AppState, period: RankingPeriod) -> AppResult<Self> {
        // Keeper's "today" fixes the calendar and its timezone.
        let (today_analysis, key_settings) = tokio::try_join!(
            state
                .keeper
                .usage_analysis(&[("range", "today"), ("include_deleted", "true")]),
            state.keeper.cpa_api_key_settings(true),
        )?;
        let today_start = today_analysis
            .range_start
            .ok_or_else(|| AppError::Upstream("Keeper omitted today's analysis range".into()))?;
        let today = today_start.date();
        let (start, end) = period
            .dates(today)
            .filter(|(start, _)| (today - *start).whole_days() <= MAX_LOOKBACK_DAYS)
            .ok_or_else(|| AppError::BadRequest("ranking period is out of range".into()))?;
        let mut usage = Self {
            today,
            start,
            end,
            offset: today_start.offset(),
            analysis: today_analysis,
            users: HashMap::new(),
            owners: HashMap::new(),
        };
        if start != today {
            let mut query = usage.range_query();
            query.push(("include_deleted", "true".into()));
            usage.analysis = state.keeper.usage_analysis(&as_query(&query)).await?;
        }

        usage.users = db::list_users(&state.db)
            .await?
            .into_iter()
            .map(|user| (user.id, user))
            .collect();
        let mut keys_by_secret = HashMap::new();
        for key in db::list_api_keys(&state.db).await? {
            keys_by_secret.insert(api_keys::decrypt(state, &key)?, key);
        }
        usage.owners = key_settings
            .items
            .into_iter()
            .filter_map(|item| {
                let secret = item.api_key?;
                let key = keys_by_secret.remove(&secret)?;
                Some((
                    item.id,
                    KeyOwner {
                        user_id: key.user_id,
                        key,
                        secret,
                    },
                ))
            })
            .collect();
        Ok(usage)
    }

    /// Whether the period is today or yesterday, which Keeper reports hour by
    /// hour under its named ranges.
    fn hourly(&self) -> bool {
        self.start == self.end
            && (self.start == self.today || Some(self.start) == self.today.previous_day())
    }

    /// Keeper range parameters for the period.
    fn range_query(&self) -> Vec<(&'static str, String)> {
        if self.hourly() {
            let range = if self.start == self.today {
                "today"
            } else {
                "yesterday"
            };
            return vec![("range", range.into())];
        }
        vec![
            ("range", "custom".into()),
            ("unit", "day".into()),
            ("start", date_string(self.start)),
            ("end", date_string(self.end)),
        ]
    }

    /// Usage per user over the models matching `matcher`.
    fn by_user(&self, matcher: Option<&str>) -> HashMap<i64, UserUsage> {
        let needle = matcher.map(str::to_lowercase);
        let mut by_user = HashMap::<i64, UserUsage>::new();
        for cell in &self.analysis.heatmap.cells {
            if needle
                .as_ref()
                .is_some_and(|needle| !cell.model.to_lowercase().contains(needle))
            {
                continue;
            }
            let Some(owner) = self.owners.get(&cell.api_key) else {
                continue;
            };
            let usage = by_user.entry(owner.user_id).or_default();
            usage.total.add(cell);
            usage
                .models
                .entry(cell.model.clone())
                .or_default()
                .add(cell);
            usage.keys.entry(owner.key.id).or_default().add(cell);
        }
        by_user
    }
}

/// Users with a positive value, best first.
fn rank<'a>(
    by_user: &'a HashMap<i64, UserUsage>,
    users: &'a HashMap<i64, User>,
    metric: RankingMetric,
) -> Vec<(&'a User, i64, &'a UserUsage)> {
    let mut ranked = by_user
        .iter()
        .filter_map(|(user_id, usage)| {
            let value = usage.total.value(metric);
            (value > 0).then_some((users.get(user_id)?, value, usage))
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
    ranked
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

    let usage = PeriodUsage::load(state, period).await?;
    let by_user = usage.by_user(matcher);
    let entries = rank(&by_user, &usage.users, metric)
        .into_iter()
        .enumerate()
        .map(|(index, (user, value, user_usage))| LeaderboardEntry {
            rank: index + 1,
            user: ranking_user(user),
            value,
            models: if include_models {
                ranking_models(user_usage, metric)
            } else {
                Vec::new()
            },
        })
        .collect();

    Ok(Leaderboard {
        period,
        metric,
        start_date: date_string(usage.start),
        end_date: date_string(usage.end),
        generated_at: OffsetDateTime::now_utc(),
        matcher: matcher.map(ToOwned::to_owned),
        entries,
    })
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

/// One user's usage over a period, through all keys they were ever issued.
#[derive(Debug, Serialize)]
pub struct UsageProfile {
    pub period: RankingPeriod,
    /// Inclusive `YYYY-MM-DD` dates in Keeper's timezone.
    pub start_date: String,
    pub end_date: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub user: RankingUser,
    /// Places in the rankings by tokens, cost and requests; absent without
    /// usage.
    pub ranks: ProfileRanks,
    pub totals: ProfileTotals,
    pub series: ProfileSeries,
    /// Largest first by tokens.
    pub models: Vec<ProfileModel>,
    /// Keys used in the period, largest first by tokens.
    pub keys: Vec<ProfileKey>,
}

#[derive(Debug, Serialize)]
pub struct ProfileRanks {
    pub tokens: Option<usize>,
    pub cost: Option<usize>,
    pub req: Option<usize>,
    /// Users with usage in the period.
    pub users: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct ProfileTotals {
    pub requests: i64,
    pub failed_requests: i64,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ProfileSeries {
    pub unit: SeriesUnit,
    /// Every hour of the day or every day of the period, in order.
    pub buckets: Vec<ProfileBucket>,
}

#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SeriesUnit {
    Hour,
    Day,
}

#[derive(Debug, Default, Serialize)]
pub struct ProfileBucket {
    /// `YYYY-MM-DD` for days, RFC 3339 for hours.
    pub start: String,
    pub total_tokens: i64,
    pub requests: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ProfileModel {
    pub model: String,
    pub total_tokens: i64,
    pub requests: i64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ProfileKey {
    pub label: String,
    /// Last characters of the key, enough to tell keys apart.
    pub suffix: String,
    pub revoked: bool,
    pub total_tokens: i64,
    pub requests: i64,
    pub cost_usd: f64,
}

const KEY_SUFFIX_CHARS: usize = 4;

/// Returns `None` when no Portal user has this GitHub id.
pub async fn usage_profile(
    state: &AppState,
    github_id: i64,
    period: RankingPeriod,
) -> AppResult<Option<UsageProfile>> {
    let Some(user) = db::get_user_by_github_id(&state.db, github_id).await? else {
        return Ok(None);
    };
    let usage = PeriodUsage::load(state, period).await?;
    let by_user = usage.by_user(None);
    let empty = UserUsage::default();
    let user_usage = by_user.get(&user.id).unwrap_or(&empty);
    let place = |metric| {
        rank(&by_user, &usage.users, metric)
            .iter()
            .position(|(ranked, ..)| ranked.id == user.id)
            .map(|index| index + 1)
    };
    let ranks = ProfileRanks {
        tokens: place(RankingMetric::Tokens),
        cost: place(RankingMetric::Cost),
        req: place(RankingMetric::Requests),
        users: rank(&by_user, &usage.users, RankingMetric::Requests).len(),
    };

    let total = &user_usage.total;
    let mut totals = ProfileTotals {
        requests: total.requests,
        failed_requests: 0,
        total_tokens: total.total_tokens,
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        reasoning_tokens: total.reasoning_tokens,
        cache_read_tokens: total.cache_read_tokens,
        cache_creation_tokens: total.cache_creation_tokens,
        cost_usd: total.cost_usd,
    };
    let mut series = empty_series(&usage);
    let range = usage.range_query();
    // Failures and the time series need one Keeper query per used key.
    for (keeper_id, owner) in &usage.owners {
        if owner.user_id != user.id || !user_usage.keys.contains_key(&owner.key.id) {
            continue;
        }
        let mut query = range.clone();
        query.push(("api_key_id", keeper_id.clone()));
        query.push(("include_deleted", "true".into()));
        let query = as_query(&query);
        let (overview, analysis) = tokio::try_join!(
            state.keeper.usage_overview(&query),
            state.keeper.usage_analysis(&query),
        )?;
        totals.failed_requests += overview.usage.failure_count;
        add_to_series(&mut series, &usage, &analysis.token_usage);
    }

    let mut models = user_usage
        .models
        .iter()
        .map(|(model, model_usage)| ProfileModel {
            model: model.clone(),
            total_tokens: model_usage.total_tokens,
            requests: model_usage.requests,
            cost_usd: model_usage.cost_usd,
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| {
        right
            .total_tokens
            .cmp(&left.total_tokens)
            .then_with(|| left.model.cmp(&right.model))
    });
    let mut keys = usage
        .owners
        .values()
        .filter_map(|owner| {
            let key_usage = user_usage.keys.get(&owner.key.id)?;
            let suffix_start = owner.secret.len().saturating_sub(KEY_SUFFIX_CHARS);
            Some(ProfileKey {
                label: owner.key.label.clone(),
                suffix: owner.secret[suffix_start..].to_owned(),
                revoked: owner.key.revoked_at.is_some(),
                total_tokens: key_usage.total_tokens,
                requests: key_usage.requests,
                cost_usd: key_usage.cost_usd,
            })
        })
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| std::cmp::Reverse(key.total_tokens));

    Ok(Some(UsageProfile {
        period,
        start_date: date_string(usage.start),
        end_date: date_string(usage.end),
        generated_at: OffsetDateTime::now_utc(),
        user: ranking_user(&user),
        ranks,
        totals,
        series,
        models,
        keys,
    }))
}

fn empty_series(usage: &PeriodUsage) -> ProfileSeries {
    if usage.hourly() {
        let midnight = usage.start.midnight().assume_offset(usage.offset);
        return ProfileSeries {
            unit: SeriesUnit::Hour,
            buckets: (0..24)
                .map(|hour| ProfileBucket {
                    start: (midnight + Duration::hours(hour))
                        .format(&Rfc3339)
                        .expect("RFC 3339 formats any offset date time"),
                    ..ProfileBucket::default()
                })
                .collect(),
        };
    }
    let days = (usage.end - usage.start).whole_days();
    ProfileSeries {
        unit: SeriesUnit::Day,
        buckets: (0..=days)
            .map(|day| ProfileBucket {
                start: date_string(usage.start + Duration::days(day)),
                ..ProfileBucket::default()
            })
            .collect(),
    }
}

fn add_to_series(series: &mut ProfileSeries, usage: &PeriodUsage, buckets: &[UsageBucket]) {
    for bucket in buckets {
        let local = bucket.bucket.to_offset(usage.offset);
        let index = match series.unit {
            SeriesUnit::Hour if local.date() == usage.start => usize::from(local.hour()),
            SeriesUnit::Day => match usize::try_from((local.date() - usage.start).whole_days()) {
                Ok(index) => index,
                Err(_) => continue,
            },
            SeriesUnit::Hour => continue,
        };
        let Some(target) = series.buckets.get_mut(index) else {
            continue;
        };
        target.total_tokens += bucket.total_tokens;
        target.requests += bucket.requests;
        target.cost_usd += bucket.cost_usd;
    }
}

fn as_query<'a>(query: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    query
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
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
