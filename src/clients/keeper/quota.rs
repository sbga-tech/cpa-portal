use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use super::KeeperClient;
use crate::error::{AppError, AppResult};

const QUOTA_CACHE_PATH: &str = "quota/cache";
const QUOTA_HISTORY_PATH: &str = "quota/history/";
const RESET_CREDITS_PATH: &str = "quota/reset-credits/";
const AUTO_REFRESH_SETTINGS_PATH: &str = "quota/auto-refresh/settings";
const VERSION_PATH: &str = "version";

#[derive(Debug, Serialize)]
struct QuotaCacheRequest<'a> {
    auth_indexes: &'a [String],
}

impl KeeperClient {
    pub async fn quota_reset_credits(&self, auth_index: &str) -> AppResult<Value> {
        let url = self.quota_auth_url(RESET_CREDITS_PATH, auth_index)?;
        self.get_json(url.as_str(), &[], "quota reset credits request")
            .await
    }

    pub async fn quota_history(
        &self,
        auth_index: &str,
        window_role: Option<&str>,
    ) -> AppResult<Value> {
        if window_role.is_some_and(|role| !matches!(role, "primary" | "secondary")) {
            return Err(AppError::BadRequest("window_role is invalid".into()));
        }
        let url = self.quota_auth_url(QUOTA_HISTORY_PATH, auth_index)?;
        let query = window_role.map(|role| ("window_role", role));
        self.get_json(url.as_str(), query.as_slice(), "quota history request")
            .await
    }

    fn quota_auth_url(&self, path: &str, auth_index: &str) -> AppResult<Url> {
        let auth_index = auth_index.trim();
        if auth_index.is_empty() || matches!(auth_index, "." | "..") {
            return Err(AppError::BadRequest("auth_index is invalid".into()));
        }
        let mut url = self.url(path)?;
        url.path_segments_mut()
            .map_err(|()| AppError::Config("Keeper URL cannot contain path segments".into()))?
            .pop_if_empty()
            .push(auth_index);
        Ok(url)
    }

    pub async fn quota_cache(&self, auth_indexes: &[String]) -> AppResult<Value> {
        self.send_json(
            Method::POST,
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
