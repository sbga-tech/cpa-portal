use tower_sessions::Session;

use crate::{
    clients::github,
    crypto,
    db::{self, GitHubProfileUpdate, NewUser, User},
    error::{AppError, AppResult},
    state::AppState,
};

const SESSION_USER_ID: &str = "user_id";

pub async fn current_user(state: &AppState, session: &Session) -> AppResult<Option<User>> {
    let Some(user_id) = session.get::<i64>(SESSION_USER_ID).await? else {
        return Ok(None);
    };

    let Some(user) = db::get_user_by_id(&state.db, user_id).await? else {
        session.flush().await?;
        return Ok(None);
    };

    Ok(Some(user))
}

pub async fn remember_current_user(session: &Session, user_id: i64) -> AppResult<()> {
    session.insert(SESSION_USER_ID, user_id).await?;
    Ok(())
}

pub fn decrypt_api_key(state: &AppState, user: &User) -> AppResult<String> {
    state
        .crypto
        .decrypt(&user.api_key_ciphertext, &user.api_key_nonce)
        .map_err(AppError::from)
}

pub async fn find_or_create_user(
    state: &AppState,
    identity: github::GitHubIdentity,
) -> AppResult<User> {
    if find_existing_user(state, identity.id).await?.is_some() {
        return update_existing_user(state, identity).await;
    }

    if !state.config.server.allow_registration {
        tracing::info!(
            github_id = identity.id,
            github_login = %identity.login,
            "rejected sign-up while registration is closed"
        );
        return Err(AppError::RegistrationClosed);
    }

    create_user(state, identity).await
}

async fn find_existing_user(state: &AppState, github_id: i64) -> AppResult<Option<User>> {
    Ok(db::get_user_by_github_id(&state.db, github_id).await?)
}

async fn update_existing_user(
    state: &AppState,
    identity: github::GitHubIdentity,
) -> AppResult<User> {
    let update = GitHubProfileUpdate {
        github_id: identity.id,
        github_login: identity.login,
        github_name: identity.name,
        github_email: identity.email,
        avatar_url: identity.avatar_url,
    };

    Ok(db::update_existing_user(&state.db, &update).await?)
}

async fn create_user(state: &AppState, identity: github::GitHubIdentity) -> AppResult<User> {
    let api_key = crypto::generate_api_key(state.config.security.api_key_prefix.as_ref())?;

    state.cpa.provision_api_key(&api_key).await?;

    let encrypted = state.crypto.encrypt(&api_key)?;
    let github_id = identity.id;
    let new_user = NewUser {
        github_id,
        github_login: identity.login,
        github_name: identity.name,
        github_email: identity.email,
        avatar_url: identity.avatar_url,
        api_key_ciphertext: encrypted.ciphertext,
        api_key_nonce: encrypted.nonce,
    };

    insert_user_with_race_retry(state, new_user, github_id).await
}

async fn insert_user_with_race_retry(
    state: &AppState,
    new_user: NewUser,
    github_id: i64,
) -> AppResult<User> {
    match db::insert_user(&state.db, new_user).await {
        Ok(user) => Ok(user),
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
            db::get_user_by_github_id(&state.db, github_id)
                .await?
                .ok_or(sqlx::Error::RowNotFound)
                .map_err(AppError::from)
        },
        Err(err) => Err(AppError::from(err)),
    }
}
