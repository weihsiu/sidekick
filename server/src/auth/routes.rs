use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect};
use axum::Json;
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

use super::AuthBackend;

#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: String,
    pub state: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub id: String,
    pub name: String,
    pub email: String,
    pub provider: String,
}

/// GET /auth/:provider — redirect to the OAuth provider's login page.
pub async fn login(
    Path(provider_name): Path<String>,
    auth_session: AuthSession<AuthBackend>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = auth_session.backend;
    let provider = backend
        .get_provider(&provider_name)
        .ok_or_else(|| anyhow::anyhow!("unknown OAuth provider: {provider_name}"))?;

    let (auth_url, csrf_token, pkce_verifier) = provider.authorize_url();

    // Store the PKCE verifier keyed by CSRF token so the callback can retrieve it.
    {
        let mut map = backend.csrf_verifiers.lock().await;
        map.insert(csrf_token.secret().clone(), pkce_verifier);
    }

    Ok(Redirect::temporary(&auth_url))
}

/// GET /auth/:provider/callback — handle the OAuth callback.
pub async fn callback(
    Path(provider_name): Path<String>,
    Query(params): Query<CallbackParams>,
    mut auth_session: AuthSession<AuthBackend>,
) -> Result<impl IntoResponse, ApiError> {
    // Retrieve and consume the PKCE verifier.
    let pkce_verifier = {
        let mut map = auth_session.backend.csrf_verifiers.lock().await;
        map.remove(&params.state)
    };

    let provider = auth_session
        .backend
        .get_provider(&provider_name)
        .ok_or_else(|| anyhow::anyhow!("unknown OAuth provider: {provider_name}"))?;

    let user_info = provider
        .exchange_code(&params.code, pkce_verifier)
        .await?;

    // Find or create the user.
    let user = crate::user::find_or_create(
        &auth_session.backend.db,
        &provider_name,
        &user_info.id,
        &user_info.name,
        &user_info.email,
    )
    .await?;

    // Log the user in (creates a session).
    auth_session
        .login(&user)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create session: {e}"))?;

    // Redirect to the frontend app.
    Ok(Redirect::temporary(&auth_session.backend.frontend_url))
}

/// POST /auth/logout — log the user out.
pub async fn logout(
    mut auth_session: AuthSession<AuthBackend>,
) -> Result<impl IntoResponse, ApiError> {
    let frontend_url = auth_session.backend.frontend_url.clone();
    auth_session
        .logout()
        .await
        .map_err(|e| anyhow::anyhow!("failed to logout: {e}"))?;
    Ok(Redirect::temporary(&frontend_url))
}

/// GET /auth/me — return the current user's info, or 401.
pub async fn me(
    auth_session: AuthSession<AuthBackend>,
) -> Result<impl IntoResponse, ApiError> {
    let user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("not authenticated"))?;

    Ok(Json(MeResponse {
        id: user.id,
        name: user.name,
        email: user.email,
        provider: user.provider,
    }))
}
