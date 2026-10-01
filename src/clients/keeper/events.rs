use serde::Deserialize;

use super::KeeperClient;
use crate::error::AppResult;

const USAGE_EVENTS_EXPORT_PATH: &str = "usage/events/export";

#[derive(Debug, Deserialize)]
pub struct UsageEventsExport {
    #[serde(default)]
    pub events: Vec<UsageEventExportItem>,
}

#[derive(Debug, Deserialize)]
pub struct UsageEventExportItem {
    pub timestamp: String,
    pub auth_index: String,
    pub total_tokens: i64,
}

impl KeeperClient {
    pub async fn usage_events_export(
        &self,
        range: &str,
        auth_index: &str,
    ) -> AppResult<UsageEventsExport> {
        self.get_json(
            USAGE_EVENTS_EXPORT_PATH,
            &[
                ("range", range),
                ("format", "json"),
                ("auth_index", auth_index),
            ],
            "usage events export request",
        )
        .await
    }
}
