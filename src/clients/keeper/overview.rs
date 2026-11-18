use serde::Deserialize;

use super::KeeperClient;
use crate::error::AppResult;

const USAGE_OVERVIEW_PATH: &str = "usage/overview";

#[derive(Debug, Deserialize)]
pub struct UsageOverview {
    pub usage: UsageTotals,
}

#[derive(Debug, Deserialize)]
pub struct UsageTotals {
    pub failure_count: i64,
}

impl KeeperClient {
    pub async fn usage_overview(&self, query: &[(&str, &str)]) -> AppResult<UsageOverview> {
        self.get_json(USAGE_OVERVIEW_PATH, query, "usage overview request")
            .await
    }
}
