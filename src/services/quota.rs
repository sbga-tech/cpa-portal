use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::{
    clients::{cpa::AuthFileStatus, keeper::UsageIdentity},
    error::AppResult,
    state::AppState,
};

const AUTH_TYPE_AUTH_FILE: i32 = 1;

#[derive(Debug, Serialize)]
pub struct QuotaSnapshot {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub credentials: Vec<QuotaCredential>,
    pub keeper: KeeperQuotaPayload,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub token_estimates: HashMap<String, f64>,
}

#[derive(Debug, Serialize)]
pub struct QuotaCredential {
    pub credential_id: String,
    pub display_name: String,
    pub provider: String,
    #[serde(rename = "type")]
    pub credential_type: String,
    pub disabled: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub subscription_active_until: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize)]
pub struct KeeperQuotaPayload {
    pub version: Value,
    pub auto_refresh: Value,
    pub quota_cache: Value,
}

#[derive(Debug, Serialize)]
pub struct QuotaRoutingSnapshot {
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub credentials: Vec<AuthFileStatus>,
}

pub async fn routing_snapshot(state: &AppState) -> AppResult<QuotaRoutingSnapshot> {
    let credentials = state.cpa.auth_file_statuses().await?;
    Ok(QuotaRoutingSnapshot {
        generated_at: OffsetDateTime::now_utc(),
        credentials,
    })
}

pub async fn snapshot(state: &AppState) -> AppResult<QuotaSnapshot> {
    let (version, auto_refresh, identities) = tokio::try_join!(
        state.keeper.version(),
        state.keeper.quota_auto_refresh_settings(),
        state.keeper.usage_identities(),
    )?;
    let identities = identities
        .identities
        .into_iter()
        .filter(|identity| identity.auth_type == AUTH_TYPE_AUTH_FILE)
        .collect::<Vec<_>>();
    let auth_indexes = identities
        .iter()
        .filter(|identity| !identity.disabled)
        .map(|identity| identity.identity.clone())
        .collect::<Vec<_>>();
    let quota_cache = if auth_indexes.is_empty() {
        json!({ "items": [] })
    } else {
        state.keeper.quota_cache(&auth_indexes).await?
    };
    let token_estimates = state
        .token_estimates
        .read()
        .await
        .fresh_values(OffsetDateTime::now_utc());

    Ok(QuotaSnapshot {
        generated_at: OffsetDateTime::now_utc(),
        credentials: identities.into_iter().map(map_credential).collect(),
        keeper: KeeperQuotaPayload {
            version,
            auto_refresh,
            quota_cache,
        },
        token_estimates,
    })
}

fn map_credential(identity: UsageIdentity) -> QuotaCredential {
    let display_name = identity
        .alias
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if identity.display_name.trim().is_empty() {
                identity.name.as_str()
            } else {
                identity.display_name.as_str()
            }
        })
        .to_owned();
    QuotaCredential {
        credential_id: identity.identity,
        display_name,
        provider: identity.provider,
        credential_type: identity.identity_type,
        disabled: identity.disabled,
        subscription_active_until: identity.active_until,
    }
}
