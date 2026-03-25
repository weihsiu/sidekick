use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Save or update OAuth tokens for a provider in the per-user database.
pub async fn save_tokens(
    pool: &SqlitePool,
    provider: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_at: Option<&str>,
    scopes: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO tokens (provider, access_token, refresh_token, expires_at, scopes, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (provider) DO UPDATE SET
             access_token  = excluded.access_token,
             refresh_token = COALESCE(excluded.refresh_token, tokens.refresh_token),
             expires_at    = excluded.expires_at,
             scopes        = excluded.scopes,
             updated_at    = excluded.updated_at",
    )
    .bind(provider)
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at)
    .bind(scopes)
    .bind(&now)
    .execute(pool)
    .await
    .context("failed to upsert tokens")?;

    Ok(())
}

/// Stored token row from the per-user database.
#[derive(sqlx::FromRow)]
struct TokenRow {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<String>,
}

/// Get a valid access token for a provider, refreshing if expired.
///
/// Returns `None` if no tokens are stored for this provider.
pub async fn get_valid_token(
    pool: &SqlitePool,
    provider: &str,
    client_id: &str,
    client_secret: &str,
    token_url: &str,
) -> Result<Option<String>> {
    let row: Option<TokenRow> = sqlx::query_as(
        "SELECT access_token, refresh_token, expires_at FROM tokens WHERE provider = ?",
    )
    .bind(provider)
    .fetch_optional(pool)
    .await
    .context("failed to query tokens")?;

    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    // Check if the token is still valid (with 5-minute buffer).
    if let Some(ref expires_at) = row.expires_at {
        if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(expires_at) {
            let buffer = chrono::Duration::minutes(5);
            if chrono::Utc::now() + buffer < exp {
                return Ok(Some(row.access_token));
            }
        }
    }

    // Token expired or no expiry info — try to refresh.
    let refresh_token = match row.refresh_token {
        Some(ref rt) if !rt.is_empty() => rt.clone(),
        _ => {
            tracing::warn!(provider, "no refresh token available, using existing access token");
            return Ok(Some(row.access_token));
        }
    };

    let new_token = refresh_access_token(
        pool,
        provider,
        client_id,
        client_secret,
        token_url,
        &refresh_token,
    )
    .await?;

    Ok(Some(new_token))
}

/// Use a refresh token to obtain a new access token from the OAuth provider.
async fn refresh_access_token(
    pool: &SqlitePool,
    provider: &str,
    client_id: &str,
    client_secret: &str,
    token_url: &str,
    refresh_token: &str,
) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(token_url)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .context("token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token refresh failed (HTTP {status}): {body}");
    }

    let data: serde_json::Value = resp.json().await.context("failed to parse token response")?;

    let access_token = data["access_token"]
        .as_str()
        .context("no access_token in refresh response")?
        .to_string();

    let expires_at = data["expires_in"].as_u64().map(|secs| {
        (chrono::Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339()
    });

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE tokens SET access_token = ?, expires_at = ?, updated_at = ? WHERE provider = ?",
    )
    .bind(&access_token)
    .bind(&expires_at)
    .bind(&now)
    .bind(provider)
    .execute(pool)
    .await
    .context("failed to update refreshed token")?;

    tracing::info!(provider, "refreshed OAuth access token");

    Ok(access_token)
}
