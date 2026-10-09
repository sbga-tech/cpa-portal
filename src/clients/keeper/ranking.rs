use std::{collections::HashMap, str::FromStr};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::KeeperClient;
use crate::error::AppResult;

const LOCAL_LEADERBOARD_PATH: &str = "ranking/local/leaderboards";

#[derive(Debug, Copy, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub enum RankingPeriod {
    #[serde(rename = "today")]
    Today,
    #[serde(rename = "yesterday")]
    Yesterday,
    #[serde(rename = "current_month")]
    CurrentMonth,
    #[serde(rename = "previous_month")]
    PreviousMonth,
}

impl AsRef<str> for RankingPeriod {
    fn as_ref(&self) -> &str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::CurrentMonth => "current_month",
            Self::PreviousMonth => "previous_month",
        }
    }
}

impl FromStr for RankingPeriod {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "today" => Ok(Self::Today),
            "yesterday" => Ok(Self::Yesterday),
            "current_month" => Ok(Self::CurrentMonth),
            "previous_month" => Ok(Self::PreviousMonth),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Copy, Clone, Deserialize, Serialize, Eq, PartialEq)]
pub enum RankingMetric {
    #[serde(rename = "overall")]
    Overall,
    #[serde(rename = "total_tokens")]
    TotalTokens,
    #[serde(rename = "request_count")]
    RequestCount,
    #[serde(rename = "cache_read_rate")]
    CacheReadRate,
    #[serde(rename = "ttft_average")]
    TTFTAverage,
    #[serde(rename = "latency_average")]
    LatencyAverage,
    #[serde(rename = "peak_tpm")]
    PeakTPM,
    #[serde(rename = "peak_rpm")]
    PeakRPM,
    /// Computed by Portal from Keeper's usage analysis; Keeper's leaderboard
    /// does not know this metric and must never receive it.
    #[serde(rename = "cost")]
    Cost,
}

impl AsRef<str> for RankingMetric {
    fn as_ref(&self) -> &str {
        match self {
            Self::Overall => "overall",
            Self::TotalTokens => "total_tokens",
            Self::RequestCount => "request_count",
            Self::CacheReadRate => "cache_read_rate",
            Self::TTFTAverage => "ttft_average",
            Self::LatencyAverage => "latency_average",
            Self::PeakTPM => "peak_tpm",
            Self::PeakRPM => "peak_rpm",
            Self::Cost => "cost",
        }
    }
}

impl FromStr for RankingMetric {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "overall" => Ok(Self::Overall),
            "total_tokens" => Ok(Self::TotalTokens),
            "request_count" => Ok(Self::RequestCount),
            "cache_read_rate" => Ok(Self::CacheReadRate),
            "ttft_average" => Ok(Self::TTFTAverage),
            "latency_average" => Ok(Self::LatencyAverage),
            "peak_tpm" => Ok(Self::PeakTPM),
            "peak_rpm" => Ok(Self::PeakRPM),
            "cost" => Ok(Self::Cost),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LocalLeaderboard {
    pub period: RankingPeriod,
    pub period_key: String,
    pub metric: RankingMetric,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub stale: bool,
    pub entries: Vec<LocalLeaderboardEntry>,
    pub score_explanation: Option<LocalScoreExplanation>,
}

#[derive(Debug, Deserialize)]
pub struct LocalLeaderboardEntry {
    pub rank: u16,
    pub participant_id: String,
    pub display_name: String,
    pub key_alias: Option<String>,
    pub avatar_id: u8,
    pub value: i64,
    pub rate_numerator: Option<i64>,
    pub rate_denominator: Option<i64>,
    pub metrics: Option<LocalLeaderboardMetrics>,
}

#[derive(Debug, Deserialize)]
pub struct LocalLeaderboardMetrics {
    pub total_tokens: i64,
    pub request_count: i64,
    pub cache_read_rate: i64,
    pub ttft_average: i64,
    pub latency_average: i64,
    pub peak_tpm: i64,
    pub peak_rpm: i64,
}

#[derive(Debug, Deserialize)]
pub struct LocalScoreExplanation {
    pub version: i64,
    pub texts: Option<HashMap<String, String>>,
}

impl KeeperClient {
    pub async fn local_leaderboard(
        &self,
        period: RankingPeriod,
        metric: RankingMetric,
    ) -> AppResult<LocalLeaderboard> {
        self.get_json(
            LOCAL_LEADERBOARD_PATH,
            &[("period", period.as_ref()), ("metric", metric.as_ref())],
            "local leaderboard request",
        )
        .await
    }
}
