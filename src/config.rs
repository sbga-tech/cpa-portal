use std::{env, fmt, net::SocketAddr, num::NonZeroU64};

use anyhow::{Context, Result};
use base64ct::{Base64, Base64UrlUnpadded, Encoding};
use config::{Config, Environment, File};
use nutype::nutype;
use secrecy::SecretString;
use serde::{Deserialize, Deserializer};
use url::Url;

#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, AsRef, Deserialize)
)]
pub struct NonEmptyString(String);

impl NonEmptyString {
    fn new(raw: impl Into<String>) -> Self {
        Self::try_new(raw).expect("hard-coded default string must not be empty")
    }
}

#[nutype(validate(predicate = is_http_url), derive(Debug, Clone, AsRef, Deserialize))]
pub struct HttpUrl(Url);

fn is_http_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

impl HttpUrl {
    pub fn as_str(&self) -> &str {
        self.as_ref().as_str().trim_end_matches('/')
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub github: GitHubConfig,
    pub cpa: CPAConfig,
    pub keeper: KeeperConfig,
    pub admin_api: AdminApiConfig,
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_server_listen")]
    pub listen: SocketAddr,
    pub public_base_url: HttpUrl,
    #[serde(default = "default_site_name")]
    pub site_name: NonEmptyString,
    #[serde(default)]
    pub session: SessionConfig,
    /// Whether GitHub users without an existing account may sign up. Existing
    /// users can always sign in.
    #[serde(default = "default_allow_registration")]
    pub allow_registration: bool,
}

fn default_server_listen() -> SocketAddr {
    "0.0.0.0:8080"
        .parse()
        .expect("hard-coded default listen address must parse")
}

fn default_site_name() -> NonEmptyString {
    NonEmptyString::new("CPA Portal")
}

fn default_allow_registration() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    pub cookie_name: NonEmptyString,
    pub secure: bool,
    pub ttl_seconds: NonZeroU64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cookie_name: NonEmptyString::new("cpa_portal"),
            secure: true,
            ttl_seconds: NonZeroU64::new(2_592_000).unwrap(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubConfig {
    pub client_id: NonEmptyString,
    pub client_secret: SecretString,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CPAConfig {
    pub public_base_url: HttpUrl,
    pub internal_base_url: HttpUrl,
    pub management_key: SecretString,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeeperConfig {
    pub internal_base_url: HttpUrl,
    pub login_password: SecretString,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminApiConfig {
    pub token: SecretString,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://./data/cpa-portal.db".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    pub api_key_encryption_key: EncryptionKey,
    #[serde(default = "default_api_key_prefix")]
    pub api_key_prefix: NonEmptyString,
}

fn default_api_key_prefix() -> NonEmptyString {
    NonEmptyString::new("sk-ghu-")
}

#[derive(Clone)]
pub struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<'de> Deserialize<'de> for EncryptionKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let raw = value.trim().strip_prefix("base64:").unwrap_or(value.trim());
        let decoded = Base64::decode_vec(raw)
            .or_else(|_| Base64UrlUnpadded::decode_vec(raw))
            .map_err(|_| serde::de::Error::custom("key must be base64 or base64url"))?;
        let key: [u8; 32] = decoded
            .try_into()
            .map_err(|_| serde::de::Error::custom("key must decode to exactly 32 bytes"))?;
        Ok(Self(key))
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let mut builder = Config::builder().add_source(File::with_name("config").required(false));

        if let Ok(path) = env::var("CPA_PORTAL_CONFIG")
            && !path.trim().is_empty()
        {
            builder = builder.add_source(File::with_name(&path).required(true));
        }

        builder
            .add_source(Environment::with_prefix("CPA_PORTAL").separator("__"))
            .build()
            .context("build configuration")?
            .try_deserialize()
            .context("deserialize configuration")
    }

    pub fn callback_url(&self) -> String {
        self.server
            .public_base_url
            .as_ref()
            .join("/auth/github/callback")
            .expect("callback path must be a valid URL path")
            .to_string()
    }
}
