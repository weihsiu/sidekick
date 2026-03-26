use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use crate::CookieKey;

type Jar = PrivateCookieJar<CookieKey>;
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::ApiError;
use crate::user::{self, User};
use crate::AppState;

use super::AuthBackend;

const REMEMBER_COOKIE: &str = "sidekick_remember";

/// Extract the authenticated user from session or remember cookie.
///
/// If the in-memory session is gone (e.g. server restart), falls back to the
/// encrypted remember cookie to restore the session transparently.
pub async fn require_user(
    auth_session: &mut AuthSession<AuthBackend>,
    jar: &Jar,
) -> Result<User, ApiError> {
    if auth_session.user.is_none() {
        if let Some(cookie) = jar.get(REMEMBER_COOKIE) {
            let user_id = cookie.value();
            if let Ok(Some(u)) = user::find_by_id(&auth_session.backend.db, user_id).await {
                let _ = auth_session.login(&u).await;
            }
        }
    }
    auth_session.user.clone().ok_or(ApiError::Unauthorized)
}

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
    pub first_name: String,
    pub last_name: String,
    pub picture: String,
    pub locale: String,
    pub providers: Vec<String>,
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

    // Persist the PKCE verifier to SQLite so it survives restarts and multi-instance deployments.
    backend.store_pkce(csrf_token.secret(), &pkce_verifier).await
        .map_err(|e| anyhow::anyhow!("failed to store PKCE verifier: {e}"))?;

    Ok(Redirect::temporary(&auth_url))
}

/// GET /auth/:provider/callback — handle the OAuth callback.
pub async fn callback(
    Path(provider_name): Path<String>,
    Query(params): Query<CallbackParams>,
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    // Retrieve and consume the PKCE verifier from SQLite.
    let pkce_verifier = auth_session.backend.take_pkce(&params.state).await
        .map_err(|e| anyhow::anyhow!("failed to retrieve PKCE verifier: {e}"))?;

    let provider = auth_session
        .backend
        .get_provider(&provider_name)
        .ok_or_else(|| anyhow::anyhow!("unknown OAuth provider: {provider_name}"))?;

    let oauth_result = provider
        .exchange_code(&params.code, pkce_verifier)
        .await?;

    // Find or create the user in sidekick.db (identity only).
    let user = crate::user::find_or_create(
        &auth_session.backend.db,
        &provider_name,
        &oauth_result.user_info.id,
        &oauth_result.user_info.email,
    )
    .await?;

    // Open the per-user database and write profile + tokens.
    let user_mem = state.chat_service.pool().get(&user.id).await?;

    user_mem.history.upsert_profile(
        &oauth_result.user_info.name,
        &oauth_result.user_info.email,
        &oauth_result.user_info.first_name,
        &oauth_result.user_info.last_name,
        &oauth_result.user_info.picture,
        &oauth_result.user_info.locale,
    ).await?;

    super::tokens::save_tokens(
        user_mem.history.pool(),
        &provider_name,
        &oauth_result.access_token,
        oauth_result.refresh_token.as_deref(),
        oauth_result.expires_at.as_deref(),
        &oauth_result.scopes,
    )
    .await?;

    // Update public directory in sidekick.db.
    sqlx::query(
        "INSERT INTO directory (user_id, display_name, picture)
         VALUES (?, ?, ?)
         ON CONFLICT (user_id) DO UPDATE SET
             display_name = excluded.display_name,
             picture      = excluded.picture",
    )
    .bind(&user.id)
    .bind(&oauth_result.user_info.name)
    .bind(&oauth_result.user_info.picture)
    .execute(&auth_session.backend.db)
    .await
    .map_err(|e| anyhow::anyhow!("failed to upsert directory: {e}"))?;

    // Log the user in (creates a session).
    let user_id = user.id.clone();
    auth_session
        .login(&user)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create session: {e}"))?;

    // Set a long-lived encrypted "remember me" cookie.
    let cookie = Cookie::build((REMEMBER_COOKIE, user_id))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(tower_sessions::cookie::time::Duration::days(30))
        .build();
    let jar = jar.add(cookie);

    // Redirect to the frontend app.
    let frontend_url = auth_session.backend.frontend_url.clone();
    Ok((jar, Redirect::temporary(&frontend_url)))
}

/// POST /auth/logout — log the user out and clear the remember cookie.
pub async fn logout(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
) -> Result<impl IntoResponse, ApiError> {
    let frontend_url = auth_session.backend.frontend_url.clone();
    auth_session
        .logout()
        .await
        .map_err(|e| anyhow::anyhow!("failed to logout: {e}"))?;
    let jar = jar.remove(Cookie::from(REMEMBER_COOKIE));
    Ok((jar, Redirect::temporary(&frontend_url)))
}

/// GET /auth/me — return the current user's info, or 401.
///
/// If the in-memory session has expired (e.g. after a server restart) but a
/// valid "remember me" cookie exists, the session is silently restored.
pub async fn me(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;

    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM user_providers WHERE user_id = ?",
    )
    .bind(&user.id)
    .fetch_all(&auth_session.backend.db)
    .await
    .unwrap_or_default();

    let user_mem = state.chat_service.pool().get(&user.id).await?;
    let profile = user_mem.history.get_profile().await?;

    let (name, email, first_name, last_name, picture, locale) = match profile {
        Some(p) => (p.name, p.email, p.first_name, p.last_name, p.picture, p.locale),
        None => (String::new(), user.email, String::new(), String::new(), String::new(), String::new()),
    };

    Ok(Json(MeResponse {
        id: user.id,
        name,
        email,
        first_name,
        last_name,
        picture,
        locale,
        providers,
    }))
}
