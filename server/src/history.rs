use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use crate::migrations;
use crate::user::UserProfile;

/// A single memory entry stored in the history database.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub category: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub importance: f32,
}

/// Per-user memory history backed by SQLite.
///
/// Provides ordered, cursor-based pagination for infinite scroll.
/// Also owns the FTS5 index over memory content.
pub struct MemoryHistory {
    db: SqlitePool,
}

impl MemoryHistory {
    /// Open (or create) the SQLite database for a user's memory history.
    pub async fn new(db_path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory for {db_path}"))?;
        }

        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect(&format!("sqlite:{db_path}?mode=rwc"))
            .await
            .with_context(|| format!("failed to open memory history db at {db_path}"))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS profile (
                name       TEXT NOT NULL DEFAULT '',
                email      TEXT NOT NULL DEFAULT '',
                first_name TEXT NOT NULL DEFAULT '',
                last_name  TEXT NOT NULL DEFAULT '',
                picture    TEXT NOT NULL DEFAULT '',
                locale     TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await
        .context("failed to create profile table")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tokens (
                provider      TEXT PRIMARY KEY,
                access_token  TEXT NOT NULL,
                refresh_token TEXT,
                expires_at    TEXT,
                scopes        TEXT NOT NULL DEFAULT '',
                updated_at    TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await
        .context("failed to create tokens table")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS contacts (
                user_id  TEXT PRIMARY KEY,
                nickname TEXT,
                blocked  INTEGER NOT NULL DEFAULT 0,
                added_at TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await
        .context("failed to create contacts table")?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                category   TEXT NOT NULL,
                role       TEXT NOT NULL,
                content    TEXT NOT NULL,
                timestamp  TEXT NOT NULL,
                importance REAL NOT NULL DEFAULT 5.0
            )",
        )
        .execute(&db)
        .await
        .context("failed to create memory table")?;

        migrations::run_sqlite_migrations(&db).await?;

        Ok(Self { db })
    }

    /// Append an entry to the memory history and update the FTS index.
    pub async fn append(
        &self,
        category: &str,
        role: &str,
        content: &str,
        timestamp: &str,
        importance: f32,
    ) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO memory (category, role, content, timestamp, importance)
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(category)
        .bind(role)
        .bind(content)
        .bind(timestamp)
        .bind(importance)
        .fetch_one(&self.db)
        .await
        .context("failed to insert memory history entry")?;

        Ok(row.0)
    }

    /// Full-text search over memory content.
    ///
    /// Returns SQLite row ids ordered by BM25 relevance (best match first).
    /// Returns an empty list if the query is empty or FTS fails to parse it.
    pub async fn fts_search(
        &self,
        query: &str,
        categories: Option<&[&str]>,
        limit: usize,
    ) -> Result<Vec<i64>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }

        let rows: Vec<(i64,)> = if let Some(cats) = categories {
            let placeholders = cats.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT rowid FROM memory_fts
                 WHERE memory_fts MATCH ? AND category IN ({placeholders})
                 ORDER BY rank LIMIT ?",
            );
            let mut q = sqlx::query_as::<_, (i64,)>(&sql).bind(query);
            for cat in cats {
                q = q.bind(*cat);
            }
            q.bind(limit as i64).fetch_all(&self.db).await
        } else {
            sqlx::query_as(
                "SELECT rowid FROM memory_fts WHERE memory_fts MATCH ? ORDER BY rank LIMIT ?",
            )
            .bind(query)
            .bind(limit as i64)
            .fetch_all(&self.db)
            .await
        }
        .unwrap_or_default(); // silently return empty on FTS parse errors

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Fetch memory entries by their SQLite ids.
    ///
    /// The returned order is undefined — callers should sort as needed.
    pub async fn fetch_by_ids(&self, ids: &[i64]) -> Result<Vec<HistoryEntry>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, category, role, content, timestamp, importance
             FROM memory WHERE id IN ({placeholders})",
        );
        let mut q = sqlx::query_as::<_, (i64, String, String, String, String, f32)>(&sql);
        for id in ids {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(&self.db).await.context("failed to fetch entries by id")?;
        Ok(rows
            .into_iter()
            .map(|(id, category, role, content, timestamp, importance)| HistoryEntry {
                id,
                category,
                role,
                content,
                timestamp,
                importance,
            })
            .collect())
    }

    /// Fetch every row in the memory table, ordered by id ascending.
    pub async fn fetch_all(&self) -> Result<Vec<HistoryEntry>> {
        let rows: Vec<(i64, String, String, String, String, f32)> = sqlx::query_as(
            "SELECT id, category, role, content, timestamp, importance FROM memory ORDER BY id ASC",
        )
        .fetch_all(&self.db)
        .await
        .context("failed to fetch all memory entries")?;

        Ok(rows
            .into_iter()
            .map(|(id, category, role, content, timestamp, importance)| HistoryEntry {
                id,
                category,
                role,
                content,
                timestamp,
                importance,
            })
            .collect())
    }

    /// Returns true if the memory table has no entries.
    pub async fn is_empty(&self) -> Result<bool> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM memory")
            .fetch_one(&self.db)
            .await
            .context("failed to count memory entries")?;
        Ok(count.0 == 0)
    }

    /// Fetch entries for infinite scroll, optionally filtered by category.
    ///
    /// - `before`: cursor — only return entries with `id < before`. Pass `None`
    ///   to start from the latest.
    /// - `limit`: max number of entries to return.
    /// - `category`: optional filter (e.g. `"conversation"`).
    ///
    /// Returns entries in **ascending** order (oldest first within the page)
    /// so the client can prepend them directly.
    pub async fn fetch(
        &self,
        before: Option<i64>,
        limit: i64,
        category: Option<&str>,
    ) -> Result<Vec<HistoryEntry>> {
        let rows: Vec<(i64, String, String, String, String, f32)> = match (before, category) {
            (Some(cursor), Some(cat)) => {
                sqlx::query_as(
                    "SELECT id, category, role, content, timestamp, importance FROM memory
                     WHERE id < ? AND category = ? ORDER BY id DESC LIMIT ?",
                )
                .bind(cursor)
                .bind(cat)
                .bind(limit)
                .fetch_all(&self.db)
                .await?
            }
            (Some(cursor), None) => {
                sqlx::query_as(
                    "SELECT id, category, role, content, timestamp, importance FROM memory
                     WHERE id < ? ORDER BY id DESC LIMIT ?",
                )
                .bind(cursor)
                .bind(limit)
                .fetch_all(&self.db)
                .await?
            }
            (None, Some(cat)) => {
                sqlx::query_as(
                    "SELECT id, category, role, content, timestamp, importance FROM memory
                     WHERE category = ? ORDER BY id DESC LIMIT ?",
                )
                .bind(cat)
                .bind(limit)
                .fetch_all(&self.db)
                .await?
            }
            (None, None) => {
                sqlx::query_as(
                    "SELECT id, category, role, content, timestamp, importance FROM memory
                     ORDER BY id DESC LIMIT ?",
                )
                .bind(limit)
                .fetch_all(&self.db)
                .await?
            }
        };

        let mut entries: Vec<HistoryEntry> = rows
            .into_iter()
            .map(|(id, category, role, content, timestamp, importance)| HistoryEntry {
                id,
                category,
                role,
                content,
                timestamp,
                importance,
            })
            .collect();
        entries.reverse();

        Ok(entries)
    }

    /// Fetch entries newer than a cursor, for catch-up after reconnect.
    ///
    /// Returns entries in **ascending** order (oldest first).
    pub async fn fetch_after(
        &self,
        after: i64,
        limit: i64,
        category: Option<&str>,
    ) -> Result<Vec<HistoryEntry>> {
        let rows: Vec<(i64, String, String, String, String, f32)> = match category {
            Some(cat) => sqlx::query_as(
                "SELECT id, category, role, content, timestamp, importance FROM memory
                 WHERE id > ? AND category = ? ORDER BY id ASC LIMIT ?",
            )
            .bind(after)
            .bind(cat)
            .bind(limit)
            .fetch_all(&self.db)
            .await?,
            None => sqlx::query_as(
                "SELECT id, category, role, content, timestamp, importance FROM memory
                 WHERE id > ? ORDER BY id ASC LIMIT ?",
            )
            .bind(after)
            .bind(limit)
            .fetch_all(&self.db)
            .await?,
        };

        Ok(rows
            .into_iter()
            .map(|(id, category, role, content, timestamp, importance)| HistoryEntry {
                id,
                category,
                role,
                content,
                timestamp,
                importance,
            })
            .collect())
    }

    /// Expose the underlying pool for token operations.
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// Insert or replace the user's profile (single-row table).
    pub async fn upsert_profile(
        &self,
        name: &str,
        email: &str,
        first_name: &str,
        last_name: &str,
        picture: &str,
        locale: &str,
    ) -> Result<()> {
        sqlx::query("DELETE FROM profile")
            .execute(&self.db)
            .await
            .context("failed to clear profile")?;

        sqlx::query(
            "INSERT INTO profile (name, email, first_name, last_name, picture, locale)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(name)
        .bind(email)
        .bind(first_name)
        .bind(last_name)
        .bind(picture)
        .bind(locale)
        .execute(&self.db)
        .await
        .context("failed to upsert profile")?;

        Ok(())
    }

    /// Fetch the user's profile, if it exists.
    pub async fn get_profile(&self) -> Result<Option<UserProfile>> {
        let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT name, email, first_name, last_name, picture, locale FROM profile LIMIT 1",
        )
        .fetch_optional(&self.db)
        .await
        .context("failed to fetch profile")?;

        Ok(row.map(|(name, email, first_name, last_name, picture, locale)| UserProfile {
            name,
            email,
            first_name,
            last_name,
            picture,
            locale,
        }))
    }
}
