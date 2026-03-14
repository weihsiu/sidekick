use anyhow::{Context, Result};
use axum_login::AuthUser;
use serde::Serialize;
use sqlx::SqlitePool;

/// A user stored in the SQLite database.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    /// Internal user ID (UUID).
    pub id: String,
    /// OAuth provider name (e.g. "google", "facebook").
    pub provider: String,
    /// Provider-specific user ID.
    pub provider_user_id: String,
    /// Display name.
    pub name: String,
    /// Email address.
    pub email: String,
    /// When the user was created.
    pub created_at: String,
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        // We use provider + provider_user_id as the session hash.
        // If the user re-links with a different provider account, sessions invalidate.
        self.provider_user_id.as_bytes()
    }
}

/// Initialise the users table.
pub async fn init_db(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            provider_user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            email TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(provider, provider_user_id)
        )",
    )
    .execute(pool)
    .await
    .context("failed to create users table")?;
    Ok(())
}

/// Find or create a user from OAuth login.
pub async fn find_or_create(
    pool: &SqlitePool,
    provider: &str,
    provider_user_id: &str,
    name: &str,
    email: &str,
) -> Result<User> {
    // Try to find existing user.
    let existing: Option<User> = sqlx::query_as(
        "SELECT * FROM users WHERE provider = ? AND provider_user_id = ?",
    )
    .bind(provider)
    .bind(provider_user_id)
    .fetch_optional(pool)
    .await
    .context("failed to query user")?;

    if let Some(user) = existing {
        return Ok(user);
    }

    // Create new user.
    let id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO users (id, provider, provider_user_id, name, email, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(provider)
    .bind(provider_user_id)
    .bind(name)
    .bind(email)
    .bind(&created_at)
    .execute(pool)
    .await
    .context("failed to insert user")?;

    Ok(User {
        id,
        provider: provider.to_string(),
        provider_user_id: provider_user_id.to_string(),
        name: name.to_string(),
        email: email.to_string(),
        created_at,
    })
}

/// Find a user by internal ID.
pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<User>> {
    sqlx::query_as("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to query user by id")
}
