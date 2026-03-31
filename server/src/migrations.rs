use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Current LanceDB schema version. Increment when the LanceDB table schema
/// changes to trigger an automatic drop-and-recreate on next startup.
pub const LANCEDB_VERSION: i32 = 1;

/// Run all pending SQLite schema migrations.
///
/// Uses `PRAGMA user_version` as the version counter. Each migration block
/// is guarded by a `version < N` check and ends by setting `user_version = N`.
pub async fn run_sqlite_migrations(db: &SqlitePool) -> Result<()> {
    // meta table is used to store non-SQLite version numbers (e.g. LanceDB).
    // Create it unconditionally so it is always available to migrations.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(db)
    .await
    .context("failed to create meta table")?;

    let (version,): (i32,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(db)
        .await
        .context("failed to read schema version")?;

    if version < 1 {
        // v1: FTS5 full-text search over memory content.
        //
        // Uses an external content table (content='memory') so the text is
        // not duplicated. A trigger keeps the FTS index in sync with every
        // INSERT into memory. A one-time backfill covers any rows that
        // existed before this migration.
        let mut tx = db.begin().await.context("v1: failed to begin transaction")?;

        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
             USING fts5(content, category, content='memory', content_rowid='id')",
        )
        .execute(&mut *tx)
        .await
        .context("v1: failed to create FTS table")?;

        sqlx::query(
            "CREATE TRIGGER IF NOT EXISTS memory_fts_sync
             AFTER INSERT ON memory BEGIN
               INSERT INTO memory_fts(rowid, content, category)
               VALUES (new.id, new.content, new.category);
             END",
        )
        .execute(&mut *tx)
        .await
        .context("v1: failed to create FTS trigger")?;

        sqlx::query(
            "INSERT INTO memory_fts(rowid, content, category)
             SELECT id, content, category FROM memory",
        )
        .execute(&mut *tx)
        .await
        .context("v1: failed to backfill FTS index")?;

        sqlx::query("PRAGMA user_version = 1")
            .execute(&mut *tx)
            .await
            .context("v1: failed to set schema version")?;

        tx.commit().await.context("v1: failed to commit migration")?;
    }

    if version < 2 {
        // v2: Add session_id and source to memory; add agent_url to contacts.
        //
        // session_id links all messages within a coordinator session together.
        // source distinguishes human-originated messages from coordinator ones.
        // agent_url stores the remote endpoint for a contact's sidekick agent.
        let mut tx = db.begin().await.context("v2: failed to begin transaction")?;

        sqlx::query("ALTER TABLE memory ADD COLUMN session_id TEXT")
            .execute(&mut *tx)
            .await
            .context("v2: failed to add session_id to memory")?;

        sqlx::query(
            "ALTER TABLE memory ADD COLUMN source TEXT NOT NULL DEFAULT 'human'",
        )
        .execute(&mut *tx)
        .await
        .context("v2: failed to add source to memory")?;

        sqlx::query(
            "ALTER TABLE contacts ADD COLUMN agent_url TEXT NOT NULL DEFAULT 'http://localhost:3000'",
        )
        .execute(&mut *tx)
        .await
        .context("v2: failed to add agent_url to contacts")?;

        sqlx::query("PRAGMA user_version = 2")
            .execute(&mut *tx)
            .await
            .context("v2: failed to set schema version")?;

        tx.commit().await.context("v2: failed to commit migration")?;
    }

    // To add a future migration: add an `if version < N { ... }` block here
    // and set `PRAGMA user_version = N` at the end of it. The current SQLite
    // version is whatever the highest N is across all blocks.

    Ok(())
}

/// Returns true if the stored LanceDB schema version does not match the
/// current expected version, meaning the table should be dropped and recreated.
pub async fn lancedb_needs_reset(db: &SqlitePool) -> Result<bool> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT value FROM meta WHERE key = 'lancedb_version'",
    )
    .fetch_optional(db)
    .await
    .context("failed to read lancedb_version from meta")?;

    let stored = row.map(|(v,)| v).unwrap_or(0);
    Ok(stored != LANCEDB_VERSION)
}

/// Record that the LanceDB table has been reset to the current schema version.
pub async fn mark_lancedb_current(db: &SqlitePool) -> Result<()> {
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES ('lancedb_version', ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(LANCEDB_VERSION)
    .execute(db)
    .await
    .context("failed to update lancedb_version in meta")?;

    Ok(())
}
