use std::{path::PathBuf, str::FromStr};

use anyhow::{Context, Result};
use sqlx::{
    Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use time::OffsetDateTime;

use crate::crypto::{EncryptedSecret, hash_secret};

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub github_id: i64,
    pub github_login: String,
    pub github_name: String,
    pub github_email: String,
    pub avatar_url: String,
    pub created_at: i64,
    pub last_login_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub github_id: i64,
    pub github_login: String,
    pub github_name: String,
    pub github_email: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone)]
pub struct GitHubProfileUpdate {
    pub github_id: i64,
    pub github_login: String,
    pub github_name: String,
    pub github_email: String,
    pub avatar_url: String,
}

#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: i64,
    pub user_id: i64,
    pub label: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

pub async fn connect(database_url: &str) -> Result<SqlitePool> {
    if let Some(path) = sqlite_file_path(database_url)
        && let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create database directory {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("parse database URL {database_url}"))?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("connect database {database_url}"))?;

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

fn sqlite_file_path(database_url: &str) -> Option<PathBuf> {
    let without_query = database_url
        .split_once('?')
        .map_or(database_url, |(left, _)| left);
    let path = without_query
        .strip_prefix("sqlite://")
        .or_else(|| without_query.strip_prefix("sqlite:"))?;
    if path == ":memory:" || path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

pub async fn get_user_by_id(pool: &SqlitePool, id: i64) -> Result<Option<User>, sqlx::Error> {
    let row = sqlx::query("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(row_to_user).transpose()
}

pub async fn list_users(pool: &SqlitePool) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query("SELECT * FROM users ORDER BY id")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(row_to_user)
        .collect()
}

pub async fn get_user_by_github_id(
    pool: &SqlitePool,
    github_id: i64,
) -> Result<Option<User>, sqlx::Error> {
    let row = sqlx::query("SELECT * FROM users WHERE github_id = ?")
        .bind(github_id)
        .fetch_optional(pool)
        .await?;
    row.map(row_to_user).transpose()
}

/// Creates the user together with their first API key.
pub async fn insert_user(
    pool: &SqlitePool,
    user: NewUser,
    key_label: &str,
    key: &EncryptedSecret,
) -> Result<User, sqlx::Error> {
    let now = now_unix();
    let mut tx = pool.begin().await?;
    let user_id = sqlx::query(
        r#"INSERT INTO users (
            github_id, github_login, github_name, github_email, avatar_url,
            created_at, last_login_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(user.github_id)
    .bind(user.github_login)
    .bind(user.github_name)
    .bind(user.github_email)
    .bind(user.avatar_url)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    insert_api_key_in(&mut tx, user_id, key_label, key, now).await?;
    tx.commit().await?;

    get_user_by_id(pool, user_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn update_existing_user(
    pool: &SqlitePool,
    update: &GitHubProfileUpdate,
) -> Result<User, sqlx::Error> {
    let now = now_unix();
    sqlx::query(
        r#"UPDATE users
           SET github_login = ?, github_name = ?, github_email = ?, avatar_url = ?,
               last_login_at = ?
           WHERE github_id = ?"#,
    )
    .bind(&update.github_login)
    .bind(&update.github_name)
    .bind(&update.github_email)
    .bind(&update.avatar_url)
    .bind(now)
    .bind(update.github_id)
    .execute(pool)
    .await?;

    get_user_by_github_id(pool, update.github_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn delete_expired_oauth_states(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM oauth_states WHERE expires_at <= ?")
        .bind(now_unix())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn insert_oauth_state(
    pool: &SqlitePool,
    state: &str,
    pkce_verifier: &str,
    ttl_seconds: i64,
) -> Result<(), sqlx::Error> {
    let now = now_unix();
    sqlx::query(
        "INSERT INTO oauth_states (state_hash, pkce_verifier, expires_at) VALUES (?, ?, ?)",
    )
    .bind(hash_secret(state))
    .bind(pkce_verifier)
    .bind(now + ttl_seconds)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn consume_oauth_state(
    pool: &SqlitePool,
    state: &str,
) -> Result<Option<String>, sqlx::Error> {
    let hash = hash_secret(state);
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT pkce_verifier FROM oauth_states WHERE state_hash = ? AND expires_at > ?",
    )
    .bind(&hash)
    .bind(now_unix())
    .fetch_optional(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM oauth_states WHERE state_hash = ?")
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    row.map(|row| row.try_get("pkce_verifier")).transpose()
}

fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn row_to_user(row: sqlx::sqlite::SqliteRow) -> Result<User, sqlx::Error> {
    Ok(User {
        id: row.try_get("id")?,
        github_id: row.try_get("github_id")?,
        github_login: row.try_get("github_login")?,
        github_name: row.try_get("github_name")?,
        github_email: row.try_get("github_email")?,
        avatar_url: row.try_get("avatar_url")?,
        created_at: row.try_get("created_at")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

/// Every key ever issued, revoked ones included, oldest first.
pub async fn list_api_keys(pool: &SqlitePool) -> Result<Vec<ApiKey>, sqlx::Error> {
    sqlx::query("SELECT * FROM api_keys ORDER BY id")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(row_to_api_key)
        .collect()
}

pub async fn list_active_api_keys(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<ApiKey>, sqlx::Error> {
    sqlx::query("SELECT * FROM api_keys WHERE user_id = ? AND revoked_at IS NULL ORDER BY id")
        .bind(user_id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(row_to_api_key)
        .collect()
}

pub async fn insert_api_key(
    pool: &SqlitePool,
    user_id: i64,
    label: &str,
    key: &EncryptedSecret,
) -> Result<ApiKey, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let id = insert_api_key_in(&mut conn, user_id, label, key, now_unix()).await?;
    get_api_key(&mut conn, id).await
}

/// Returns the renamed key, or `None` when the user has no such active key.
pub async fn rename_api_key(
    pool: &SqlitePool,
    user_id: i64,
    key_id: i64,
    label: &str,
) -> Result<Option<ApiKey>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let updated = sqlx::query(
        "UPDATE api_keys SET label = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(label)
    .bind(key_id)
    .bind(user_id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Ok(None);
    }
    get_api_key(&mut conn, key_id).await.map(Some)
}

/// Returns whether the user had such an active key.
pub async fn revoke_api_key(
    pool: &SqlitePool,
    user_id: i64,
    key_id: i64,
) -> Result<bool, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    revoke_api_key_in(&mut conn, user_id, key_id).await
}

/// Revokes the key and issues `replacement` under the same label. Returns the
/// new key, or `None` when the user has no such active key.
pub async fn rotate_api_key(
    pool: &SqlitePool,
    user_id: i64,
    key_id: i64,
    replacement: &EncryptedSecret,
) -> Result<Option<ApiKey>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let old = get_api_key(&mut tx, key_id).await;
    let old = match old {
        Ok(old) if old.user_id == user_id && old.revoked_at.is_none() => old,
        Ok(_) | Err(sqlx::Error::RowNotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    revoke_api_key_in(&mut tx, user_id, key_id).await?;
    let id = insert_api_key_in(&mut tx, user_id, &old.label, replacement, now_unix()).await?;
    let key = get_api_key(&mut tx, id).await?;
    tx.commit().await?;
    Ok(Some(key))
}

async fn insert_api_key_in(
    conn: &mut SqliteConnection,
    user_id: i64,
    label: &str,
    key: &EncryptedSecret,
    now: i64,
) -> Result<i64, sqlx::Error> {
    Ok(sqlx::query(
        "INSERT INTO api_keys (user_id, label, ciphertext, nonce, created_at) VALUES (?, ?, ?, ?, \
         ?)",
    )
    .bind(user_id)
    .bind(label)
    .bind(&key.ciphertext)
    .bind(&key.nonce)
    .bind(now)
    .execute(conn)
    .await?
    .last_insert_rowid())
}

async fn revoke_api_key_in(
    conn: &mut SqliteConnection,
    user_id: i64,
    key_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE api_keys SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(now_unix())
    .bind(key_id)
    .bind(user_id)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

async fn get_api_key(conn: &mut SqliteConnection, id: i64) -> Result<ApiKey, sqlx::Error> {
    sqlx::query("SELECT * FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(conn)
        .await
        .and_then(row_to_api_key)
}

fn row_to_api_key(row: sqlx::sqlite::SqliteRow) -> Result<ApiKey, sqlx::Error> {
    Ok(ApiKey {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        label: row.try_get("label")?,
        ciphertext: row.try_get("ciphertext")?,
        nonce: row.try_get("nonce")?,
        created_at: row.try_get("created_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}
