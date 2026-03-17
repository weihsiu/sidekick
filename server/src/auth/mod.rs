pub mod oauth;
pub mod routes;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum_login::{AuthnBackend, UserId};
use sqlx::SqlitePool;
use tokio::sync::Mutex;

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
    /// PKCE verifiers keyed by CSRF token, consumed during callback.
    pub csrf_verifiers: Arc<Mutex<HashMap<String, oauth2::PkceCodeVerifier>>>,
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
            csrf_verifiers: Arc::new(Mutex::new(HashMap::new())),
            frontend_url: frontend_url.to_string(),
        })
    }

    pub fn get_provider(&self, name: &str) -> Option<&OAuthProvider> {
        self.providers.get(name)
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
            let user_info = provider.exchange_code(&creds.code, None).await?;

            // Find or create the user in our database.
            let user = user::find_or_create(
                &db,
                &creds.provider,
                &user_info.id,
                &user_info.name,
                &user_info.email,
                &user_info.first_name,
                &user_info.last_name,
                &user_info.picture,
                &user_info.locale,
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
