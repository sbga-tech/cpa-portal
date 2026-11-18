//! Portal-issued CPA API keys. The database is the source of truth: CPA's
//! client key list is synced to it after every change and once a minute, so a
//! failed sync is repaired on a later pass.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use serde::Serialize;
use time::OffsetDateTime;
use tokio::time::MissedTickBehavior;

use crate::{
    crypto::{self, EncryptedSecret},
    db::{self, ApiKey},
    error::{AppError, AppResult},
    state::AppState,
};

const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const MAX_LABEL_CHARS: usize = 64;
pub const DEFAULT_LABEL: &str = "Default";

/// An active key as its owner sees it.
#[derive(Debug, Serialize)]
pub struct OwnedKey {
    pub id: i64,
    pub label: String,
    pub key: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub fn decrypt(state: &AppState, key: &ApiKey) -> AppResult<String> {
    state
        .crypto
        .decrypt(&key.ciphertext, &key.nonce)
        .map_err(AppError::from)
}

/// Generates a key and encrypts it for storage.
pub fn generate(state: &AppState) -> AppResult<EncryptedSecret> {
    let key = crypto::generate_api_key(state.config.security.api_key_prefix.as_ref())?;
    Ok(state.crypto.encrypt(&key)?)
}

pub async fn list(state: &AppState, user_id: i64) -> AppResult<Vec<OwnedKey>> {
    db::list_active_api_keys(&state.db, user_id)
        .await?
        .iter()
        .map(|key| owned_key(state, key))
        .collect()
}

pub async fn create(state: &AppState, user_id: i64, label: &str) -> AppResult<OwnedKey> {
    let label = validate_label(label)?;
    let key = db::insert_api_key(&state.db, user_id, label, &generate(state)?).await?;
    sync_cpa(state).await?;
    owned_key(state, &key)
}

pub async fn rename(
    state: &AppState,
    user_id: i64,
    key_id: i64,
    label: &str,
) -> AppResult<OwnedKey> {
    let label = validate_label(label)?;
    let key = db::rename_api_key(&state.db, user_id, key_id, label)
        .await?
        .ok_or(AppError::NotFound)?;
    owned_key(state, &key)
}

pub async fn revoke(state: &AppState, user_id: i64, key_id: i64) -> AppResult<()> {
    if !db::revoke_api_key(&state.db, user_id, key_id).await? {
        return Err(AppError::NotFound);
    }
    sync_cpa(state).await
}

/// Replaces the key with a new one under the same label.
pub async fn rotate(state: &AppState, user_id: i64, key_id: i64) -> AppResult<OwnedKey> {
    let key = db::rotate_api_key(&state.db, user_id, key_id, &generate(state)?)
        .await?
        .ok_or(AppError::NotFound)?;
    sync_cpa(state).await?;
    owned_key(state, &key)
}

/// Makes CPA accept exactly the active Portal keys.
pub async fn sync_cpa(state: &AppState) -> AppResult<()> {
    let mut issued = HashSet::new();
    let mut active = HashSet::new();
    for key in db::list_api_keys(&state.db).await? {
        let secret = decrypt(state, &key)?;
        if key.revoked_at.is_none() {
            active.insert(secret.clone());
        }
        issued.insert(secret);
    }
    state.cpa.sync_api_keys(&issued, &active).await
}

/// Periodically repairs CPA's key list and labels the keys in Keeper.
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(SYNC_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(error) = sync_cpa(&state).await {
            tracing::warn!(%error, "CPA API key sync failed");
        }
        if let Err(error) = sync_keeper_aliases(&state).await {
            tracing::warn!(%error, "Keeper API key alias sync failed");
        }
    }
}

/// Keeps Keeper's alias for each active Portal key at `login/label`. Keeper
/// discovers new CPA keys on its own sync interval, so a new key is aliased on
/// a later pass. Keys Portal did not issue are left untouched.
async fn sync_keeper_aliases(state: &AppState) -> AppResult<()> {
    let key_settings = state.keeper.cpa_api_key_settings(false).await?;
    let logins = db::list_users(&state.db)
        .await?
        .into_iter()
        .map(|user| (user.id, user.github_login))
        .collect::<HashMap<_, _>>();
    let mut aliases = HashMap::new();
    for key in db::list_api_keys(&state.db).await? {
        if key.revoked_at.is_some() {
            continue;
        }
        if let Some(login) = logins.get(&key.user_id) {
            aliases.insert(decrypt(state, &key)?, format!("{login}/{}", key.label));
        }
    }

    for key in key_settings.items {
        let Some(alias) = key
            .api_key
            .as_deref()
            .and_then(|api_key| aliases.remove(api_key))
        else {
            continue;
        };
        if key.key_alias == alias {
            continue;
        }
        match state.keeper.update_cpa_api_key_alias(&key.id, &alias).await {
            Ok(()) => {
                tracing::info!(keeper_api_key_id = %key.id, %alias, "updated Keeper API key alias");
            },
            Err(error) => {
                tracing::warn!(%error, keeper_api_key_id = %key.id, %alias, "Keeper API key alias update failed");
            },
        }
    }
    Ok(())
}

fn validate_label(label: &str) -> AppResult<&str> {
    let label = label.trim();
    // Keeper rejects aliases with control characters, and the label is part
    // of the alias.
    if label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err(AppError::BadRequest(format!(
            "A key label needs 1 to {MAX_LABEL_CHARS} characters and no control characters."
        )));
    }
    Ok(label)
}

fn owned_key(state: &AppState, key: &ApiKey) -> AppResult<OwnedKey> {
    Ok(OwnedKey {
        id: key.id,
        label: key.label.clone(),
        key: decrypt(state, key)?,
        created_at: unix_time(key.created_at)?,
    })
}

fn unix_time(timestamp: i64) -> AppResult<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(timestamp).map_err(|error| {
        AppError::Other(anyhow::anyhow!(
            "stored key timestamp is out of range: {error}"
        ))
    })
}
