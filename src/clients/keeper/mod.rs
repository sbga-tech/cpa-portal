mod api_keys;
mod identities;
mod quota;
mod ranking;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub use api_keys::{CPAAPIKeySettingsItem, CPAAPIKeySettingsResponse};
pub use identities::{UsageIdentitiesResponse, UsageIdentity};
pub use ranking::{
    LocalLeaderboard, LocalLeaderboardEntry, LocalLeaderboardMetrics, LocalScoreExplanation,
    RankingMetric, RankingPeriod,
};
use reqwest::{
    Client as HttpClient, Method, Response, StatusCode, cookie::Jar,
    redirect::Policy as RedirectPolicy,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;
use url::Url;

use crate::{
    clients::http,
    config::KeeperConfig,
    error::{AppError, AppResult},
};

const API_PATH: &str = "api/v1/";
const LOGIN_PATH: &str = "auth/login";
const REQUEST_INTENT_HEADER: &str = "X-CPA-Usage-Keeper-Request";
const REQUEST_INTENT_FETCH: &str = "fetch";
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct KeeperClient {
    inner: Arc<KeeperClientInner>,
}

struct KeeperClientInner {
    http: HttpClient,
    api_base_url: Url,
    login_password: SecretString,
    auth_lock: RwLock<()>,
    auth_generation: AtomicU64,
}

impl KeeperClient {
    pub fn new(config: &KeeperConfig) -> AppResult<Self> {
        let api_base_url = api_base_url(config.internal_base_url.as_ref())?;
        let cookie_jar = Arc::new(Jar::default());
        let http = http::client_builder()
            .cookie_provider(cookie_jar)
            .redirect(RedirectPolicy::none())
            .retry(reqwest::retry::never())
            .build()?;

        Ok(Self {
            inner: Arc::new(KeeperClientInner {
                http,
                api_base_url,
                login_password: config.login_password.clone(),
                auth_lock: RwLock::new(()),
                auth_generation: AtomicU64::new(0),
            }),
        })
    }

    pub(super) async fn get_json<T>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        operation: &'static str,
    ) -> AppResult<T>
    where
        T: DeserializeOwned,
    {
        let (observed_generation, response) = {
            let _guard = self.inner.auth_lock.read().await;
            let observed_generation = self.inner.auth_generation.load(Ordering::Acquire);
            let response = self.send_get(path, query, operation).await?;
            (observed_generation, response)
        };
        if response.status() != StatusCode::UNAUTHORIZED {
            return decode_json_response(response, operation).await;
        }
        drain_response(response).await;

        self.refresh_auth(observed_generation).await?;
        let response = {
            let _guard = self.inner.auth_lock.read().await;
            self.send_get(path, query, operation).await?
        };
        decode_json_response(response, operation).await
    }

    pub(super) async fn send_json<T, B>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        operation: &'static str,
    ) -> AppResult<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let (observed_generation, response) = {
            let _guard = self.inner.auth_lock.read().await;
            let observed_generation = self.inner.auth_generation.load(Ordering::Acquire);
            let response = self
                .send_body(method.clone(), path, body, operation)
                .await?;
            (observed_generation, response)
        };
        if response.status() != StatusCode::UNAUTHORIZED {
            return decode_json_response(response, operation).await;
        }
        drain_response(response).await;

        self.refresh_auth(observed_generation).await?;
        let response = {
            let _guard = self.inner.auth_lock.read().await;
            self.send_body(method, path, body, operation).await?
        };
        decode_json_response(response, operation).await
    }

    async fn send_body<B>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        operation: &'static str,
    ) -> AppResult<Response>
    where
        B: Serialize + ?Sized,
    {
        self.inner
            .http
            .request(method, self.url(path)?)
            .header(REQUEST_INTENT_HEADER, REQUEST_INTENT_FETCH)
            .json(body)
            .send()
            .await
            .map_err(|error| keeper_request_error(operation, error))
    }

    async fn send_get(
        &self,
        path: &str,
        query: &[(&str, &str)],
        operation: &'static str,
    ) -> AppResult<Response> {
        self.inner
            .http
            .get(self.url(path)?)
            .query(query)
            .send()
            .await
            .map_err(|error| keeper_request_error(operation, error))
    }

    async fn refresh_auth(&self, observed_generation: u64) -> AppResult<()> {
        let _guard = self.inner.auth_lock.write().await;
        if self.inner.auth_generation.load(Ordering::Acquire) != observed_generation {
            return Ok(());
        }

        let result = self.login().await;
        self.inner.auth_generation.fetch_add(1, Ordering::Release);
        result
    }

    async fn login(&self) -> AppResult<()> {
        let password = self.inner.login_password.expose_secret();
        let response = self
            .inner
            .http
            .post(self.url(LOGIN_PATH)?)
            .header(REQUEST_INTENT_HEADER, REQUEST_INTENT_FETCH)
            .json(&LoginRequest { password })
            .send()
            .await
            .map_err(|error| keeper_request_error("admin login", error))?;

        if response.status() == StatusCode::NO_CONTENT {
            drain_response(response).await;
            return Ok(());
        }

        Err(upstream_response_error(response, "admin login").await)
    }

    fn url(&self, path: &str) -> AppResult<Url> {
        self.inner.api_base_url.join(path).map_err(AppError::from)
    }
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    password: &'a str,
}

fn api_base_url(configured: &Url) -> AppResult<Url> {
    if configured.host_str().is_none()
        || !configured.username().is_empty()
        || configured.password().is_some()
        || configured.query().is_some()
        || configured.fragment().is_some()
    {
        return Err(AppError::Config(
            "Keeper internal base URL must be an HTTP URL without credentials, query, or fragment"
                .into(),
        ));
    }

    let mut normalized = configured.clone();
    let path = normalized.path().trim_end_matches('/');
    normalized.set_path(&format!("{path}/{API_PATH}"));
    Ok(normalized)
}

async fn decode_json_response<T>(response: Response, operation: &'static str) -> AppResult<T>
where
    T: DeserializeOwned,
{
    if !response.status().is_success() {
        return Err(upstream_response_error(response, operation).await);
    }

    response.json().await.map_err(|error| {
        AppError::Upstream(format!(
            "Keeper {operation} returned an invalid JSON response: {error}"
        ))
    })
}

fn keeper_request_error(operation: &'static str, error: reqwest::Error) -> AppError {
    AppError::Upstream(format!("Keeper {operation} failed: {error}"))
}

async fn upstream_response_error(response: Response, operation: &'static str) -> AppError {
    let status = response.status();
    let body = bounded_response_body(response).await;

    AppError::Upstream(format!("Keeper {operation} failed with {status}: {body}"))
}

async fn bounded_response_body(mut response: Response) -> String {
    let content_length = response.content_length();
    let mut body = Vec::with_capacity(
        content_length
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(MAX_ERROR_BODY_BYTES),
    );
    let mut truncated = false;

    while let Some(chunk) = match response.chunk().await {
        Ok(chunk) => chunk,
        Err(error) => return format!("failed to read response body: {error}"),
    } {
        if body.len() < MAX_ERROR_BODY_BYTES {
            let remaining = MAX_ERROR_BODY_BYTES - body.len();
            let retained = chunk.len().min(remaining);
            body.extend_from_slice(&chunk[..retained]);
            truncated |= retained < chunk.len();
        } else if !chunk.is_empty() {
            truncated = true;
        }
    }

    if body.len() == MAX_ERROR_BODY_BYTES
        && content_length.is_none_or(|length| length > MAX_ERROR_BODY_BYTES as u64)
    {
        truncated = true;
    }

    let mut text = String::from_utf8_lossy(&body).trim().to_owned();
    if text.is_empty() {
        text.push_str("empty response body");
    }
    if truncated {
        text.push('…');
    }
    text
}

async fn drain_response(mut response: Response) {
    while matches!(response.chunk().await, Ok(Some(_))) {}
}
