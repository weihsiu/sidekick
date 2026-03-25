use anyhow::{Context, Result};
use axum_login::AuthUser;
use serde::Serialize;
use sqlx::SqlitePool;

/// A user stored in the SQLite database (minimal — just enough for login routing).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    /// Internal user ID (UUID).
    pub id: String,
    /// Email address (used as the merge key across providers).
    pub email: String,
    /// When the user was created.
    pub created_at: String,
}

/// Full user profile stored in the per-user database.
#[derive(Debug, Clone, Serialize)]
pub struct UserProfile {
    pub name: String,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub picture: String,
    pub locale: String,
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.id.as_bytes()
    }
}

/// Initialise the shared authentication tables in sidekick.db.
pub async fn init_db(pool: &SqlitePool) -> Result<()> {
    // Minimal user table — just identity and merge key.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id         TEXT PRIMARY KEY,
            email      TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("failed to create users table")?;

    // Maps OAuth provider identities to internal user IDs.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_providers (
            user_id          TEXT NOT NULL REFERENCES users(id),
            provider         TEXT NOT NULL,
            provider_user_id TEXT NOT NULL,
            PRIMARY KEY (provider, provider_user_id)
        )",
    )
    .execute(pool)
    .await
    .context("failed to create user_providers table")?;

    // Short-lived PKCE verifiers for in-flight OAuth flows.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS oauth_state (
            csrf_token    TEXT PRIMARY KEY,
            pkce_verifier TEXT NOT NULL,
            created_at    INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("failed to create oauth_state table")?;

    // Public directory for discoverability (future agent-to-agent chat).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS directory (
            user_id      TEXT PRIMARY KEY REFERENCES users(id),
            display_name TEXT NOT NULL,
            handle       TEXT UNIQUE,
            picture      TEXT NOT NULL DEFAULT '',
            visible      INTEGER NOT NULL DEFAULT 1
        )",
    )
    .execute(pool)
    .await
    .context("failed to create directory table")?;

    Ok(())
}

/// Find or create a user from OAuth login.
///
/// Users are matched by email: if a user with the same email already exists
/// (e.g. from a different OAuth provider), the existing user is returned and
/// the new provider is linked.
///
/// Profile fields (name, picture, etc.) are stored in the per-user database,
/// not here — call `MemoryHistory::upsert_profile` after this.
pub async fn find_or_create(
    pool: &SqlitePool,
    provider: &str,
    provider_user_id: &str,
    email: &str,
) -> Result<User> {
    // First check if this exact provider link already exists.
    let existing: Option<User> = sqlx::query_as(
        "SELECT u.* FROM users u
         JOIN user_providers up ON u.id = up.user_id
         WHERE up.provider = ? AND up.provider_user_id = ?",
    )
    .bind(provider)
    .bind(provider_user_id)
    .fetch_optional(pool)
    .await
    .context("failed to query user by provider")?;

    if let Some(user) = existing {
        return Ok(user);
    }

    // Check if a user with the same email exists (merge across providers).
    let by_email: Option<User> = sqlx::query_as("SELECT * FROM users WHERE email = ?")
        .bind(email)
        .fetch_optional(pool)
        .await
        .context("failed to query user by email")?;

    let user_id = if let Some(existing_user) = by_email {
        existing_user.id
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        sqlx::query("INSERT INTO users (id, email, created_at) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(email)
            .bind(&created_at)
            .execute(pool)
            .await
            .context("failed to insert user")?;

        id
    };

    // Link this provider to the user.
    sqlx::query(
        "INSERT OR IGNORE INTO user_providers (user_id, provider, provider_user_id)
         VALUES (?, ?, ?)",
    )
    .bind(&user_id)
    .bind(provider)
    .bind(provider_user_id)
    .execute(pool)
    .await
    .context("failed to link provider to user")?;

    find_by_id(pool, &user_id)
        .await?
        .context("user not found after creation")
}

/// Find a user by internal ID.
pub async fn find_by_id(pool: &SqlitePool, id: &str) -> Result<Option<User>> {
    sqlx::query_as("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to query user by id")
}
