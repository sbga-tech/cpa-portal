use serde::Serialize;
use serde_json::Value;

use super::KeeperClient;
use crate::error::{AppError, AppResult};

const QUOTA_CACHE_PATH: &str = "quota/cache";
const RESET_CREDITS_PATH: &str = "quota/reset-credits/";
const AUTO_REFRESH_SETTINGS_PATH: &str = "quota/auto-refresh/settings";
const VERSION_PATH: &str = "version";

#[derive(Debug, Serialize)]
struct QuotaCacheRequest<'a> {
    auth_indexes: &'a [String],
}

impl KeeperClient {
    pub async fn quota_reset_credits(&self, auth_index: &str) -> AppResult<Value> {
        let auth_index = auth_index.trim();
        if auth_index.is_empty() || matches!(auth_index, "." | "..") {
            return Err(AppError::BadRequest("auth_index is invalid".into()));
        }

        let mut url = self.url(RESET_CREDITS_PATH)?;
        url.path_segments_mut()
            .map_err(|()| AppError::Config("Keeper URL cannot contain path segments".into()))?
            .pop_if_empty()
            .push(auth_index);
        self.get_json(url.as_str(), &[], "quota reset credits request")
            .await
    }

    pub async fn quota_cache(&self, auth_indexes: &[String]) -> AppResult<Value> {
        self.post_json(
            QUOTA_CACHE_PATH,
            &QuotaCacheRequest { auth_indexes },
            "quota cache request",
        )
        .await
    }

    pub async fn quota_auto_refresh_settings(&self) -> AppResult<Value> {
        self.get_json(
            AUTO_REFRESH_SETTINGS_PATH,
            &[],
            "quota auto-refresh settings request",
        )
        .await
    }

    pub async fn version(&self) -> AppResult<Value> {
        self.get_json(VERSION_PATH, &[], "version request").await
    }
}
