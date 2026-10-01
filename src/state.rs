use std::{collections::HashMap, sync::Arc};

use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::{
    admin_api::AdminApiAuth,
    clients::{cpa::CPAClient, github::GitHubOAuthClient, keeper::KeeperClient},
    config::AppConfig,
    crypto::Crypto,
};

#[derive(Clone, Debug, Default)]
pub struct TokenEstimateCache {
    pub values: HashMap<String, f64>,
    pub refreshed_at: Option<time::OffsetDateTime>,
}

impl TokenEstimateCache {
    pub fn fresh_values(&self, now: time::OffsetDateTime) -> HashMap<String, f64> {
        const MAX_AGE_SECONDS: i64 = 30 * 60;
        match self.refreshed_at {
            Some(refreshed_at)
                if now >= refreshed_at
                    && now - refreshed_at <= time::Duration::seconds(MAX_AGE_SECONDS) =>
            {
                self.values.clone()
            },
            _ => HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub admin_api_auth: Arc<AdminApiAuth>,
    pub crypto: Arc<Crypto>,
    pub db: SqlitePool,
    pub github: Arc<GitHubOAuthClient>,
    pub cpa: Arc<CPAClient>,
    pub keeper: Arc<KeeperClient>,
    pub token_estimates: Arc<RwLock<TokenEstimateCache>>,
}
