use std::sync::Arc;

use anyhow::{Context, Result};
use cpa_portal::{
    admin_api::AdminApiAuth,
    clients::{cpa::CPAClient, github::GitHubOAuthClient, keeper::KeeperClient},
    config::AppConfig,
    crypto::Crypto,
    db, routes,
    services::key_alias,
    state::AppState,
};
use time::Duration;
use tower_http::trace::TraceLayer;
use tower_sessions::{
    Expiry, SessionManagerLayer, cookie::SameSite, session_store::ExpiredDeletion,
};
use tower_sessions_sqlx_store::SqliteStore;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Arc::new(AppConfig::load()?);
    let crypto = Arc::new(Crypto::new(&config.security.api_key_encryption_key));
    let db = db::connect(&config.database.url).await?;
    let session_config = &config.server.session;
    let session_ttl_seconds = i64::try_from(session_config.ttl_seconds.get())
        .context("session TTL exceeds supported range")?;

    let session_store = SqliteStore::new(db.clone());
    session_store
        .migrate()
        .await
        .context("migrate session store")?;
    let cleanup_task = tokio::task::spawn(
        session_store
            .clone()
            .continuously_delete_expired(std::time::Duration::from_secs(60)),
    );

    let session_layer = SessionManagerLayer::new(session_store)
        .with_name(session_config.cookie_name.as_ref().to_owned())
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_secure(session_config.secure)
        .with_path("/")
        .with_expiry(Expiry::OnInactivity(Duration::seconds(session_ttl_seconds)));

    let state = AppState {
        config: Arc::clone(&config),
        admin_api_auth: Arc::new(AdminApiAuth::new(&config.admin_api)?),
        crypto,
        db,
        github: Arc::new(GitHubOAuthClient::new(
            &config.github,
            config.callback_url(),
        )?),
        cpa: Arc::new(CPAClient::new(&config.cpa)?),
        keeper: Arc::new(KeeperClient::new(&config.keeper)?),
    };
    let key_alias_task = tokio::task::spawn(key_alias::run(state.clone()));

    let app = routes::router(state)
        .layer(session_layer)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(config.server.listen)
        .await
        .with_context(|| format!("bind {}", config.server.listen))?;

    tracing::info!(listen = %config.server.listen, "cpa-portal listening");
    let serve_result = axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await;
    cleanup_task.abort();
    key_alias_task.abort();
    serve_result.context("serve HTTP")?;

    match cleanup_task.await {
        Ok(Ok(())) => {},
        Ok(Err(err)) => return Err(err).context("delete expired sessions"),
        Err(err) if err.is_cancelled() => {},
        Err(err) => return Err(err).context("join session cleanup task"),
    }
    match key_alias_task.await {
        Ok(()) => {},
        Err(err) if err.is_cancelled() => {},
        Err(err) => return Err(err).context("join Keeper API key alias sync task"),
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
