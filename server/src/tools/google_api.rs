use std::sync::Arc;

use anyhow::Result;
use reqwest::{Client, Method};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::config::OAuthProviderConfig;
use crate::context::CURRENT_USER_ID;

/// Shared Google API client that resolves per-user tokens from the database.
#[derive(Clone)]
pub struct GoogleApiClient {
    pub(crate) db: SqlitePool,
    pub(crate) http: Client,
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
    pub(crate) token_url: String,
}

impl GoogleApiClient {
    pub fn new(db: SqlitePool, google_config: &OAuthProviderConfig) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            db,
            http: Client::new(),
            client_id: google_config.client_id.clone(),
            client_secret: google_config.client_secret()?,
            token_url: google_config.token_url.clone(),
        }))
    }

    /// Get a valid access token for the current user (from task-local context).
    pub async fn get_user_token(&self) -> Result<String, synaptic::core::SynapticError> {
        let user_id = CURRENT_USER_ID
            .try_with(|id| id.clone())
            .map_err(|_| {
                tracing::error!("CURRENT_USER_ID task-local not set — tool called outside user scope");
                synaptic::core::SynapticError::Tool("no user context available".into())
            })?;

        tracing::debug!(user_id = %user_id, "looking up Google token for user");

        let token = crate::auth::tokens::get_valid_token(
            &self.db,
            &user_id,
            "google",
            &self.client_id,
            &self.client_secret,
            &self.token_url,
        )
        .await
        .map_err(|e| {
            tracing::error!(user_id = %user_id, error = %e, "failed to get/refresh Google token");
            synaptic::core::SynapticError::Tool(format!("token error: {e}"))
        })?;

        match token {
            Some(t) => Ok(t),
            None => {
                tracing::warn!(user_id = %user_id, "no Google token row found in user_tokens");
                Err(synaptic::core::SynapticError::Tool(
                    "no Google token found — user needs to re-authenticate with Google".into(),
                ))
            }
        }
    }

    /// Make an authenticated Google API call.
    pub async fn call(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, synaptic::core::SynapticError> {
        let token = self.get_user_token().await?;

        let mut req = self.http.request(method, url).bearer_auth(&token);
        if let Some(body) = body {
            req = req.json(body);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| synaptic::core::SynapticError::Tool(format!("API request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(synaptic::core::SynapticError::Tool(format!(
                "Google API error (HTTP {status}): {body}"
            )));
        }

        // Some endpoints (DELETE) return no body.
        let text = resp.text().await.unwrap_or_default();
        if text.is_empty() {
            return Ok(serde_json::json!({"status": "ok"}));
        }

        serde_json::from_str(&text)
            .map_err(|e| synaptic::core::SynapticError::Tool(format!("failed to parse response: {e}")))
    }
}
