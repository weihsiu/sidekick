use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

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

        Ok(Self { db })
    }

    /// Append an entry to the memory history.
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
}

