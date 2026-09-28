use std::{collections::HashMap, time::Duration};

use tokio::time::MissedTickBehavior;

use crate::{db, error::AppResult, services::user::decrypt_api_key, state::AppState};

const SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Keeps Keeper's alias for each Portal-issued API key equal to the owner's
/// GitHub login. Keeper discovers new CPA API keys on its own metadata sync
/// interval, so a key provisioned at registration is aliased on a later pass.
/// Keys that do not belong to a Portal user are left untouched.
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(SYNC_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if let Err(error) = sync_once(&state).await {
            tracing::warn!(%error, "Keeper API key alias sync failed");
        }
    }
}

async fn sync_once(state: &AppState) -> AppResult<()> {
    let key_settings = state.keeper.cpa_api_key_settings().await?;
    let mut logins_by_api_key = HashMap::new();
    for user in db::list_users(&state.db).await? {
        logins_by_api_key.insert(decrypt_api_key(state, &user)?, user.github_login);
    }

    for key in key_settings.items {
        let Some(login) = logins_by_api_key.remove(&key.api_key) else {
            continue;
        };
        if key.key_alias == login {
            continue;
        }
        match state.keeper.update_cpa_api_key_alias(&key.id, &login).await {
            Ok(()) => {
                tracing::info!(keeper_api_key_id = %key.id, alias = %login, "updated Keeper API key alias");
            },
            Err(error) => {
                tracing::warn!(%error, keeper_api_key_id = %key.id, alias = %login, "Keeper API key alias update failed");
            },
        }
    }
    Ok(())
}
