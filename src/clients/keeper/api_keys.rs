use reqwest::Method;
use serde::{Deserialize, Serialize, de::IgnoredAny};

use super::KeeperClient;
use crate::error::{AppError, AppResult};

const CPA_API_KEY_SETTINGS_PATH: &str = "usage/api-keys/settings";
const CPA_API_KEYS_PATH: &str = "usage/api-keys";

#[derive(Debug, Deserialize)]
pub struct CPAAPIKeySettingsResponse {
    pub items: Vec<CPAAPIKeySettingsItem>,
}

#[derive(Debug, Deserialize)]
pub struct CPAAPIKeySettingsItem {
    pub id: String,
    /// Keeper omits the raw key for plugin-provided keys.
    #[serde(rename = "apiKey", default)]
    pub api_key: Option<String>,
    #[serde(rename = "keyAlias", default)]
    pub key_alias: String,
}

#[derive(Debug, Serialize)]
struct UpdateCPAAPIKeyAliasRequest<'a> {
    #[serde(rename = "keyAlias")]
    key_alias: &'a str,
}

impl KeeperClient {
    /// With `include_deleted`, also lists keys CPA no longer has, so usage of
    /// revoked keys can still be attributed.
    pub async fn cpa_api_key_settings(
        &self,
        include_deleted: bool,
    ) -> AppResult<CPAAPIKeySettingsResponse> {
        let query: &[(&str, &str)] = if include_deleted {
            &[("include_deleted", "true")]
        } else {
            &[]
        };
        self.get_json(
            CPA_API_KEY_SETTINGS_PATH,
            query,
            "CPA API key settings request",
        )
        .await
    }

    pub async fn update_cpa_api_key_alias(&self, id: &str, key_alias: &str) -> AppResult<()> {
        let id = id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or_else(|| AppError::Upstream("Keeper API key id is invalid".into()))?;
        let mut url = self.url(CPA_API_KEYS_PATH)?;
        url.path_segments_mut()
            .map_err(|()| AppError::Config("Keeper URL cannot contain path segments".into()))?
            .pop_if_empty()
            .push(&id.to_string());

        let _: IgnoredAny = self
            .send_json(
                Method::PATCH,
                url.as_str(),
                &UpdateCPAAPIKeyAliasRequest { key_alias },
                "CPA API key alias update",
            )
            .await?;
        Ok(())
    }
}
