use std::collections::HashSet;

use reqwest::{
    Client as HttpClient, StatusCode,
    header::{AUTHORIZATION, ETAG, HeaderMap, HeaderValue, IF_MATCH},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use url::Url;

use crate::{
    clients::http,
    config::CPAConfig,
    error::{AppError, AppResult},
};

const MANAGEMENT_BASE_PATH: &str = "/v8/management/";
const API_KEYS_PATH: &str = "config/access/api-keys";
const CREDENTIALS_PATH: &str = "credentials";
/// v8 replaces the whole key list on write; a concurrent writer surfaces as
/// 412.
const API_KEY_WRITE_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct CPAClient {
    http: HttpClient,
    base_url: Url,
}

impl CPAClient {
    pub fn new(config: &CPAConfig) -> AppResult<Self> {
        let mut headers = HeaderMap::new();
        let auth = format!("Bearer {}", config.management_key.expose_secret());
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).map_err(|err| {
                AppError::Config(format!("invalid CPA management key header: {err}"))
            })?,
        );

        let http = http::client_builder().default_headers(headers).build()?;
        let base_url = config
            .internal_base_url
            .as_ref()
            .join(MANAGEMENT_BASE_PATH)?;

        Ok(Self { http, base_url })
    }

    /// Makes the Portal keys in CPA's client key list exactly `active`: drops
    /// keys in `issued` that are not active and appends missing active keys.
    /// Keys Portal did not issue are kept as they are.
    pub async fn sync_api_keys(
        &self,
        issued: &HashSet<String>,
        active: &HashSet<String>,
    ) -> AppResult<()> {
        for _ in 0..API_KEY_WRITE_ATTEMPTS {
            let (current, etag) = self.api_keys().await?;
            let mut keys = current
                .iter()
                .filter(|key| !issued.contains(*key) || active.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            let mut missing = active
                .iter()
                .filter(|key| !current.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            missing.sort_unstable();
            keys.extend(missing);
            if keys == current {
                return Ok(());
            }
            // CPA serves requests without authentication when the list is
            // empty.
            if keys.is_empty() {
                return Err(AppError::Upstream(
                    "refusing to clear CPA's client API key list".to_owned(),
                ));
            }

            let mut request = self.http.put(self.url(API_KEYS_PATH)).json(&keys);
            if let Some(etag) = etag {
                request = request.header(IF_MATCH, etag);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            if status != StatusCode::PRECONDITION_FAILED {
                let text = response.text().await.unwrap_or_default();
                return Err(AppError::Upstream(format!(
                    "CPA API key update failed with {status}: {text}"
                )));
            }
        }
        Err(AppError::Upstream(
            "CPA API key update kept conflicting with concurrent config writes".to_owned(),
        ))
    }

    /// Returns the client keys and the config version they were read from.
    async fn api_keys(&self) -> AppResult<(Vec<String>, Option<HeaderValue>)> {
        let response = self.http.get(self.url(API_KEYS_PATH)).send().await?;
        // An absent list has no version to match; the first key is written
        // unconditionally.
        if response.status() == StatusCode::NOT_FOUND {
            return Ok((Vec::new(), None));
        }
        if !response.status().is_success() {
            return Err(AppError::Upstream(format!(
                "CPA API key lookup failed with {}",
                response.status()
            )));
        }
        let etag = response.headers().get(ETAG).cloned();
        Ok((response.json().await?, etag))
    }

    pub async fn auth_file_statuses(&self) -> AppResult<Vec<AuthFileStatus>> {
        let response = self.http.get(self.url(CREDENTIALS_PATH)).send().await?;
        if !response.status().is_success() {
            return Err(AppError::Upstream(format!(
                "CPA credentials lookup failed with {}",
                response.status()
            )));
        }
        let response = response.json::<AuthFileStatusesResponse>().await?;
        Ok(response.files)
    }

    fn url(&self, path: &str) -> Url {
        self.base_url
            .join(path)
            .expect("management API path must be relative")
    }
}

#[derive(Deserialize)]
struct AuthFileStatusesResponse {
    files: Vec<AuthFileStatus>,
}

/// Only scheduling metadata may leave the management API boundary.
#[derive(Debug, Deserialize, Serialize)]
pub struct AuthFileStatus {
    #[serde(rename(deserialize = "auth_index", serialize = "credential_id"))]
    pub credential_id: String,
    pub provider: String,
    pub disabled: bool,
    pub status: String,
    pub unavailable: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub next_retry_after: Option<OffsetDateTime>,
    pub weight: Option<i64>,
}
