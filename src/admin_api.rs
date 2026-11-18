use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    api_error::ApiError,
    config::AdminApiConfig,
    error::{AppError, AppResult},
    state::AppState,
};

const MIN_ADMIN_TOKEN_BYTES: usize = 32;

pub struct AdminApiAuth {
    token_hash: [u8; 32],
}

impl AdminApiAuth {
    pub fn new(config: &AdminApiConfig) -> AppResult<Self> {
        let token = config.token.expose_secret();
        if token.len() < MIN_ADMIN_TOKEN_BYTES {
            return Err(AppError::Config(format!(
                "admin API token must contain at least {MIN_ADMIN_TOKEN_BYTES} bytes"
            )));
        }
        Ok(Self {
            token_hash: hash_token(token),
        })
    }

    fn verify(&self, candidate: &str) -> bool {
        self.token_hash.ct_eq(&hash_token(candidate)).into()
    }
}

pub struct AdminApiAuthenticated;

impl FromRequestParts<AppState> for AdminApiAuthenticated {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let candidate = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|value| !value.is_empty())
            .ok_or_else(ApiError::bearer_unauthorized)?;

        if !state.admin_api_auth.verify(candidate) {
            return Err(ApiError::bearer_unauthorized());
        }
        Ok(Self)
    }
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
