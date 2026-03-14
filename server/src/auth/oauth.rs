use anyhow::{Context, Result};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenUrl,
};

use oauth2::TokenResponse;

use crate::config::OAuthProviderConfig;

/// User info returned from an OAuth provider.
pub struct UserInfo {
    pub id: String,
    pub name: String,
    pub email: String,
}

/// Type alias for a BasicClient with auth_url and token_url set.
type ConfiguredClient = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// A configured OAuth2 provider (Google, Facebook, etc.).
pub struct OAuthProvider {
    pub name: String,
    client: ConfiguredClient,
    scopes: Vec<String>,
    userinfo_url: String,
}

impl OAuthProvider {
    pub fn new(name: &str, config: &OAuthProviderConfig, redirect_url: &str) -> Result<Self> {
        let client = BasicClient::new(ClientId::new(config.client_id.clone()))
            .set_client_secret(ClientSecret::new(config.client_secret()?))
            .set_auth_uri(AuthUrl::new(config.auth_url.clone())?)
            .set_token_uri(TokenUrl::new(config.token_url.clone())?)
            .set_redirect_uri(RedirectUrl::new(redirect_url.to_string())?);

        Ok(Self {
            name: name.to_string(),
            client,
            scopes: config.scopes.clone(),
            userinfo_url: config.userinfo_url.clone(),
        })
    }

    /// Generate the authorization URL and PKCE verifier.
    pub fn authorize_url(&self) -> (String, CsrfToken, PkceCodeVerifier) {
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let mut builder = self
            .client
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(pkce_challenge);

        for scope in &self.scopes {
            builder = builder.add_scope(Scope::new(scope.clone()));
        }

        let (url, csrf_token) = builder.url();
        (url.to_string(), csrf_token, pkce_verifier)
    }

    /// Exchange an authorization code for tokens and fetch user info.
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: Option<PkceCodeVerifier>,
    ) -> Result<UserInfo> {
        let mut exchange = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_string()));

        if let Some(verifier) = pkce_verifier {
            exchange = exchange.set_pkce_verifier(verifier);
        }

        let http_client = reqwest::Client::new();
        let token_response = exchange
            .request_async(&http_client)
            .await
            .context("failed to exchange OAuth code for token")?;

        let access_token = token_response.access_token().secret();
        self.fetch_userinfo(access_token).await
    }

    /// Fetch user info from the provider's userinfo endpoint.
    async fn fetch_userinfo(&self, access_token: &str) -> Result<UserInfo> {
        let client = reqwest::Client::new();
        let response = client
            .get(&self.userinfo_url)
            .bearer_auth(access_token)
            .send()
            .await
            .context("failed to fetch userinfo")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("userinfo request failed (HTTP {status}): {body}");
        }

        let resp: serde_json::Value = response
            .json()
            .await
            .context("failed to parse userinfo response")?;

        // Google: { "sub": "...", "name": "...", "email": "..." }
        // Facebook: { "id": "...", "name": "...", "email": "..." }
        let id = resp["sub"]
            .as_str()
            .or_else(|| resp["id"].as_str())
            .ok_or_else(|| anyhow::anyhow!("userinfo response missing 'sub' or 'id' field"))?
            .to_string();

        let name = resp["name"].as_str().unwrap_or("Unknown").to_string();
        let email = resp["email"].as_str().unwrap_or("").to_string();

        Ok(UserInfo { id, name, email })
    }
}
