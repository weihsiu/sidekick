use anyhow::{Context, Result};
use axum_login::AuthUser;
use serde::Serialize;
use sqlx::SqlitePool;

/// A user stored in the SQLite database.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    /// Internal user ID (UUID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Email address (used as the merge key across providers).
    pub email: String,
    /// First name.
    pub first_name: String,
    /// Last name.
    pub last_name: String,
    /// Profile picture URL.
    pub picture: String,
    /// Locale (e.g. "en", "zh-TW").
    pub locale: String,
    /// When the user was created.
    pub created_at: String,
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

/// Initialise the users and user_providers tables.
pub async fn init_db(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            first_name TEXT NOT NULL DEFAULT '',
            last_name TEXT NOT NULL DEFAULT '',
            picture TEXT NOT NULL DEFAULT '',
            locale TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await
    .context("failed to create users table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_providers (
            user_id TEXT NOT NULL REFERENCES users(id),
            provider TEXT NOT NULL,
            provider_user_id TEXT NOT NULL,
            PRIMARY KEY (provider, provider_user_id)
        )",
    )
    .execute(pool)
    .await
    .context("failed to create user_providers table")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_tokens (
            user_id TEXT NOT NULL REFERENCES users(id),
            provider TEXT NOT NULL,
            access_token TEXT NOT NULL,
            refresh_token TEXT,
            expires_at TEXT,
            scopes TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL,
            PRIMARY KEY (user_id, provider)
        )",
    )
    .execute(pool)
    .await
    .context("failed to create user_tokens table")?;

    Ok(())
}

/// Find or create a user from OAuth login.
///
/// Users are matched by email: if a user with the same email already exists
/// (e.g. from a different OAuth provider), the existing user is returned and
/// the new provider is linked.
pub async fn find_or_create(
    pool: &SqlitePool,
    provider: &str,
    provider_user_id: &str,
    name: &str,
    email: &str,
    first_name: &str,
    last_name: &str,
    picture: &str,
    locale: &str,
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
        // Update profile fields from latest OAuth info.
        sqlx::query(
            "UPDATE users SET name = ?, first_name = ?, last_name = ?, picture = ?, locale = ?
             WHERE id = ?",
        )
        .bind(name)
        .bind(first_name)
        .bind(last_name)
        .bind(picture)
        .bind(locale)
        .bind(&user.id)
        .execute(pool)
        .await
        .context("failed to update user profile")?;

        return find_by_id(pool, &user.id)
            .await?
            .context("user disappeared after update");
    }

    // Check if a user with the same email exists (merge across providers).
    let by_email: Option<User> = sqlx::query_as(
        "SELECT * FROM users WHERE email = ?",
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .context("failed to query user by email")?;

    let user_id = if let Some(existing_user) = by_email {
        // Link the new provider to the existing user.
        existing_user.id
    } else {
        // Create a new user.
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO users (id, name, email, first_name, last_name, picture, locale, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(email)
        .bind(first_name)
        .bind(last_name)
        .bind(picture)
        .bind(locale)
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
