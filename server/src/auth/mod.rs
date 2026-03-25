pub mod oauth;
pub mod routes;
pub mod tokens;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum_login::{AuthnBackend, UserId};
use sqlx::SqlitePool;

use crate::config::OAuthProviderConfig;
use crate::user::{self, User};

use self::oauth::OAuthProvider;

/// Credentials passed from the OAuth callback.
#[derive(Clone)]
pub struct OAuthCredentials {
    pub provider: String,
    pub code: String,
}

/// Wrapper so we can use anyhow internally while satisfying `std::error::Error`.
#[derive(Debug, thiserror::Error)]
#[error("{0:#}")]
pub struct AuthError(#[from] anyhow::Error);

/// Auth backend for axum-login.
///
/// Holds all configured OAuth providers and the user database.
#[derive(Clone)]
pub struct AuthBackend {
    pub(crate) db: SqlitePool,
    providers: Arc<HashMap<String, OAuthProvider>>,
    pub frontend_url: String,
}

impl AuthBackend {
    pub fn new(
        db: SqlitePool,
        base_url: &str,
        frontend_url: &str,
        provider_configs: &HashMap<String, OAuthProviderConfig>,
    ) -> Result<Self> {
        let mut providers = HashMap::new();
        for (name, cfg) in provider_configs {
            let redirect_url = format!("{}/auth/{}/callback", base_url, name);
            let provider = OAuthProvider::new(name, cfg, &redirect_url)
                .with_context(|| format!("failed to configure OAuth provider '{name}'"))?;
            providers.insert(name.clone(), provider);
        }

        Ok(Self {
            db,
            providers: Arc::new(providers),
            frontend_url: frontend_url.to_string(),
        })
    }

    pub fn get_provider(&self, name: &str) -> Option<&OAuthProvider> {
        self.providers.get(name)
    }

    /// Persist a PKCE verifier for an in-flight OAuth flow.
    pub async fn store_pkce(&self, csrf_token: &str, verifier: &oauth2::PkceCodeVerifier) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        // Expire any stale entries (older than 10 minutes) while we're here.
        sqlx::query("DELETE FROM oauth_state WHERE created_at < ?")
            .bind(now - 600)
            .execute(&self.db)
            .await
            .ok();
        sqlx::query(
            "INSERT OR REPLACE INTO oauth_state (csrf_token, pkce_verifier, created_at) VALUES (?, ?, ?)",
        )
        .bind(csrf_token)
        .bind(verifier.secret())
        .bind(now)
        .execute(&self.db)
        .await
        .context("failed to store PKCE verifier")?;
        Ok(())
    }

    /// Retrieve and delete a PKCE verifier by CSRF token.
    pub async fn take_pkce(&self, csrf_token: &str) -> Result<Option<oauth2::PkceCodeVerifier>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT pkce_verifier FROM oauth_state WHERE csrf_token = ?")
                .bind(csrf_token)
                .fetch_optional(&self.db)
                .await
                .context("failed to look up PKCE verifier")?;

        if let Some((secret,)) = row {
            sqlx::query("DELETE FROM oauth_state WHERE csrf_token = ?")
                .bind(csrf_token)
                .execute(&self.db)
                .await
                .ok();
            Ok(Some(oauth2::PkceCodeVerifier::new(secret)))
        } else {
            Ok(None)
        }
    }
}

impl AuthnBackend for AuthBackend {
    type User = User;
    type Credentials = OAuthCredentials;
    type Error = AuthError;

    fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> impl std::future::Future<Output = Result<Option<Self::User>, Self::Error>> + Send {
        let providers = self.providers.clone();
        let db = self.db.clone();

        async move {
            let provider = providers
                .get(&creds.provider)
                .with_context(|| format!("unknown OAuth provider: {}", creds.provider))?;

            // Exchange the auth code for tokens and fetch user info.
            let oauth_result = provider.exchange_code(&creds.code, None).await?;

            // Find or create the user in sidekick.db (identity only).
            // Profile and tokens are written in routes::callback which has UserStorePool access.
            let user = user::find_or_create(
                &db,
                &creds.provider,
                &oauth_result.user_info.id,
                &oauth_result.user_info.email,
            )
            .await?;

            Ok(Some(user))
        }
    }

    fn get_user(
        &self,
        user_id: &UserId<Self>,
    ) -> impl std::future::Future<Output = Result<Option<Self::User>, Self::Error>> + Send {
        let db = self.db.clone();
        let user_id = user_id.clone();

        async move {
            let user = user::find_by_id(&db, &user_id).await?;
            Ok(user)
        }
    }
}
