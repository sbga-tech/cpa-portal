use serde::Deserialize;
use time::OffsetDateTime;

use super::KeeperClient;
use crate::error::AppResult;

const USAGE_ANALYSIS_PATH: &str = "usage/analysis";

#[derive(Debug, Deserialize)]
pub struct UsageAnalysis {
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub range_start: Option<OffsetDateTime>,
    #[serde(default)]
    pub token_usage: Vec<UsageBucket>,
    #[serde(default)]
    pub heatmap: UsageHeatmap,
}

/// Usage in one hour or day, depending on the requested range. Keeper omits
/// buckets without usage.
#[derive(Debug, Deserialize)]
pub struct UsageBucket {
    #[serde(with = "time::serde::rfc3339")]
    pub bucket: OffsetDateTime,
    pub total_tokens: i64,
    pub requests: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Debug, Default, Deserialize)]
pub struct UsageHeatmap {
    #[serde(default)]
    pub cells: Vec<UsageHeatmapCell>,
}

/// Usage of one Keeper API key (identified by its Keeper id) on one model.
#[derive(Debug, Deserialize)]
pub struct UsageHeatmapCell {
    pub api_key: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
    pub requests: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl KeeperClient {
    pub async fn usage_analysis(&self, query: &[(&str, &str)]) -> AppResult<UsageAnalysis> {
        self.get_json(USAGE_ANALYSIS_PATH, query, "usage analysis request")
            .await
    }
}
