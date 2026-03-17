use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use crate::CookieKey;

type Jar = PrivateCookieJar<CookieKey>;
use axum_login::AuthSession;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::user;

use super::AuthBackend;

const REMEMBER_COOKIE: &str = "sidekick_remember";

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
    jar: Jar,
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
        &user_info.first_name,
        &user_info.last_name,
        &user_info.picture,
        &user_info.locale,
    )
    .await?;

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
) -> Result<impl IntoResponse, ApiError> {
    // If no active session, try restoring from the remember cookie.
    if auth_session.user.is_none() {
        if let Some(cookie) = jar.get(REMEMBER_COOKIE) {
            let user_id = cookie.value();
            if let Ok(Some(u)) = user::find_by_id(&auth_session.backend.db, user_id).await {
                let _ = auth_session.login(&u).await;
            }
        }
    }

    let user = auth_session
        .user
        .ok_or(ApiError::Unauthorized)?;

    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM user_providers WHERE user_id = ?",
    )
    .bind(&user.id)
    .fetch_all(&auth_session.backend.db)
    .await
    .unwrap_or_default();

    Ok(Json(MeResponse {
        id: user.id,
        name: user.name,
        email: user.email,
        first_name: user.first_name,
        last_name: user.last_name,
        picture: user.picture,
        locale: user.locale,
        providers,
    }))
}
