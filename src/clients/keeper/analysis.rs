use serde::Deserialize;
use time::OffsetDateTime;

use super::KeeperClient;
use crate::error::AppResult;

const USAGE_ANALYSIS_PATH: &str = "usage/analysis";

#[derive(Debug, Deserialize)]
pub struct UsageAnalysis {
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub range_start: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub range_end: Option<OffsetDateTime>,
    #[serde(default)]
    pub model_composition: Vec<UsageCompositionItem>,
    #[serde(default)]
    pub heatmap: UsageHeatmap,
}

#[derive(Debug, Deserialize)]
pub struct UsageCompositionItem {
    pub key: String,
    #[serde(default)]
    pub label: String,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct UsageHeatmap {
    #[serde(default)]
    pub cells: Vec<UsageHeatmapCell>,
}

#[derive(Debug, Deserialize)]
pub struct UsageHeatmapCell {
    pub api_key: String,
    pub model: String,
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
