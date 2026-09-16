use serde::Deserialize;
use time::OffsetDateTime;

use super::KeeperClient;
use crate::error::AppResult;

const USAGE_IDENTITIES_PATH: &str = "usage/identities";

#[derive(Debug, Deserialize)]
pub struct UsageIdentitiesResponse {
    pub identities: Vec<UsageIdentity>,
}

#[derive(Debug, Deserialize)]
pub struct UsageIdentity {
    pub id: String,
    pub name: String,
    pub alias: Option<String>,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub auth_type: i32,
    pub identity: String,
    #[serde(rename = "type")]
    pub identity_type: String,
    pub provider: String,
    pub file_name: Option<String>,
    pub disabled: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub active_until: Option<OffsetDateTime>,
}

impl KeeperClient {
    pub async fn usage_identities(&self) -> AppResult<UsageIdentitiesResponse> {
        self.get_json(USAGE_IDENTITIES_PATH, &[], "usage identities request")
            .await
    }
}
