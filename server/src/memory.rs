use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lru::LruCache;
use serde::Deserialize;
use synaptic::core::Embeddings;
use synaptic::memory::{ChatMessageHistory, ConversationWindowMemory};
use synaptic::store::InMemoryStore;
use tokio::sync::Mutex;

use crate::config::MemoryConfig;
use crate::history::{HistoryEntry, MemoryHistory};
use crate::migrations;
use crate::rerank::Reranker;

/// A record in a JSONL import file.
#[derive(Debug, Deserialize)]
pub struct ImportRecord {
    pub category: String,
    pub content: String,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default = "default_importance")]
    pub importance: f32,
}

fn default_role() -> String {
    "system".to_string()
}

fn default_importance() -> f32 {
    5.0
}

/// Format retrieved memory entries into a context string for the system prompt.
pub fn format_context(entries: &[HistoryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut ctx = String::from("Relevant context from memory:\n\n");
    for entry in entries {
        let role_label = match entry.role.as_str() {
            "human" => "User",
            "ai" => "Assistant",
            "system" => "Knowledge",
            other => other,
        };
        ctx.push_str(&format!(
            "[{}] [{}] {}: {}\n",
            entry.timestamp, entry.category, role_label, entry.content
        ));
    }
    ctx
}

// ---------------------------------------------------------------------------
// SemanticMemory — pure vector index backed by LanceDB
// ---------------------------------------------------------------------------

/// Semantic memory backed by LanceDB.
///
/// Stores only the embedding vector plus enough metadata for filtering.
/// All plaintext lives in SQLite (`MemoryHistory`).
///
/// Also holds the short-term chat window (last N messages passed to the LLM).
pub struct SemanticMemory {
    table: lancedb::Table,
    embeddings: Arc<dyn Embeddings>,
    reranker: Arc<dyn Reranker>,
    dim: usize,
    top_k: usize,
    rerank_top_n: usize,
    category_weights: HashMap<String, f32>,
    /// Short-term chat window backed by synaptic's conversation window.
    pub chat_memory: Arc<ConversationWindowMemory>,
}

impl SemanticMemory {
    /// Connect to (or create) the LanceDB table for semantic memory.
    ///
    /// If `needs_reset` is true the existing table is dropped and recreated.
    /// The caller is responsible for determining whether a reset is needed
    /// (via `migrations::lancedb_needs_reset`) and for recording the new
    /// version afterwards (via `migrations::mark_lancedb_current`).
    pub async fn new(
        db_path: &str,
        table_name: &str,
        needs_reset: bool,
        embeddings: Arc<dyn Embeddings>,
        reranker: Arc<dyn Reranker>,
        dim: usize,
        top_k: usize,
        rerank_top_n: usize,
        category_weights: HashMap<String, f32>,
        chat_window_size: usize,
    ) -> Result<Self> {
        std::fs::create_dir_all(db_path)
            .with_context(|| format!("failed to create db directory: {db_path}"))?;

        let db = lancedb::connect(db_path)
            .execute()
            .await
            .with_context(|| format!("failed to connect to LanceDB at {db_path}"))?;

        let schema = Self::schema(dim);
        let existing = db.table_names().execute().await?;

        if needs_reset && existing.contains(&table_name.to_string()) {
            tracing::info!("LanceDB schema version changed — dropping table '{table_name}'");
            db.drop_table(table_name, &[]).await?;
        }

        let existing = db.table_names().execute().await?;
        let table = if existing.contains(&table_name.to_string()) {
            db.open_table(table_name).execute().await?
        } else {
            db.create_empty_table(table_name, schema).execute().await?
        };

        Ok(Self {
            table,
            embeddings,
            reranker,
            dim,
            top_k,
            rerank_top_n,
            category_weights,
            chat_memory: Arc::new(ConversationWindowMemory::new(
                Arc::new(ChatMessageHistory::new(Arc::new(InMemoryStore::new()))),
                chat_window_size,
            )),
        })
    }

    fn schema(dim: usize) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("category", DataType::Utf8, false),
            Field::new("importance", DataType::Float32, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                false,
            ),
        ]))
    }

    /// Store a vector for a SQLite memory entry.
    ///
    /// The `sqlite_id` is the primary key from the memory table and is used
    /// to join back to the content when retrieving.
    pub async fn store_vector(
        &self,
        sqlite_id: i64,
        category: &str,
        importance: f32,
        embedding: Vec<f32>,
    ) -> Result<()> {
        let schema = Self::schema(self.dim);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![sqlite_id.to_string()])),
                Arc::new(StringArray::from(vec![category])),
                Arc::new(Float32Array::from(vec![importance])),
                Arc::new(FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                    vec![Some(embedding.into_iter().map(Some).collect::<Vec<_>>())],
                    self.dim as i32,
                )),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.table.add(batches).execute().await?;
        Ok(())
    }

    /// Vector similarity search.
    ///
    /// Returns SQLite row ids ordered by cosine similarity (most similar first).
    pub async fn vector_search(
        &self,
        embedding: Vec<f32>,
        categories: Option<&[&str]>,
        limit: usize,
    ) -> Result<Vec<i64>> {
        if self.table.count_rows(None).await? == 0 {
            return Ok(vec![]);
        }

        let mut builder = self.table.query().nearest_to(embedding)?;

        if let Some(cats) = categories {
            let cat_list: Vec<String> = cats
                .iter()
                .map(|c| format!("'{}'", c.replace('\'', "''")))
                .collect();
            builder = builder.only_if(format!("category IN ({})", cat_list.join(", ")));
        }

        let results: Vec<RecordBatch> = builder
            .limit(limit)
            .distance_type(lancedb::DistanceType::Cosine)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut ids = Vec::new();
        for batch in &results {
            let ids_col = batch
                .column_by_name("id")
                .context("missing id column in LanceDB result")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("id column is not StringArray")?;
            for i in 0..batch.num_rows() {
                if let Ok(id) = ids_col.value(i).parse::<i64>() {
                    ids.push(id);
                }
            }
        }

        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// UserStore — coordinates SQLite + LanceDB for a single user
// ---------------------------------------------------------------------------

/// Per-user memory bundle: LanceDB (vector index) + SQLite (content + FTS).
pub struct UserStore {
    pub semantic: SemanticMemory,
    pub history: MemoryHistory,
}

impl UserStore {
    /// Store a memory entry.
    ///
    /// Writes to SQLite first (authoritative). Then embeds the content and
    /// writes the vector to LanceDB. If LanceDB fails the entry is still
    /// persisted in SQLite and FTS — it just won't appear in vector search.
    ///
    /// `session_id` links the entry to a coordinator session (`None` for normal
    /// chat). `source` is `"human"` for ordinary messages and `"coordinator"`
    /// for agent-to-agent coordination messages.
    pub async fn store(
        &self,
        category: &str,
        role: &str,
        content: &str,
        timestamp: &str,
        importance: f32,
        session_id: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        let id = self
            .history
            .append(category, role, content, timestamp, importance, session_id, source)
            .await?;

        match self.semantic.embeddings.embed_query(content).await {
            Ok(embedding) => {
                if let Err(e) = self
                    .semantic
                    .store_vector(id, category, importance, embedding)
                    .await
                {
                    tracing::warn!("failed to store vector for entry {id}: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("failed to embed entry {id}: {e}");
            }
        }

        Ok(id)
    }

    /// Retrieve the most relevant memory entries for a query.
    ///
    /// Runs vector search (LanceDB) and FTS (SQLite) in parallel, merges
    /// results via RRF, fetches content from SQLite, then reranks.
    pub async fn retrieve(
        &self,
        query: &str,
        categories: Option<&[&str]>,
    ) -> Result<Vec<HistoryEntry>> {
        let top_k = self.semantic.top_k;

        let embedding = self
            .semantic
            .embeddings
            .embed_query(query)
            .await
            .context("failed to embed query")?;

        let (vector_ids, fts_ids) = tokio::join!(
            self.semantic.vector_search(embedding, categories, top_k),
            self.history.fts_search(query, categories, top_k),
        );

        let vector_ids = vector_ids.unwrap_or_default();
        let fts_ids = fts_ids.unwrap_or_default();
        tracing::debug!(
            query,
            vector_hits = vector_ids.len(),
            fts_hits = fts_ids.len(),
            "retrieve: search results"
        );

        if vector_ids.is_empty() && fts_ids.is_empty() {
            return Ok(vec![]);
        }

        let merged = rrf_merge(&vector_ids, &fts_ids, 60.0);
        let merged = &merged[..merged.len().min(top_k)];

        let entries = self.history.fetch_by_ids(merged).await?;
        if entries.is_empty() {
            return Ok(vec![]);
        }

        // Rerank using the cross-encoder.
        let docs: Vec<&str> = entries.iter().map(|e| e.content.as_str()).collect();
        let mut ranked = self
            .semantic
            .reranker
            .rerank(query, &docs, entries.len())
            .await?;

        // Apply category and importance weights on top of reranker scores.
        for r in &mut ranked {
            let cat_weight = self
                .semantic
                .category_weights
                .get(&entries[r.index].category)
                .copied()
                .unwrap_or(1.0);
            let imp = entries[r.index].importance.clamp(1.0, 10.0);
            let imp_weight = 0.5 + (imp - 1.0) / 9.0;
            r.score *= cat_weight * imp_weight;
        }
        ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(self.semantic.rerank_top_n);

        let mut result: Vec<HistoryEntry> =
            ranked.into_iter().map(|r| entries[r.index].clone()).collect();

        // Sort chronologically so the LLM sees context in order.
        result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(result)
    }

    /// Re-embed all SQLite memory entries into LanceDB.
    ///
    /// Called after a LanceDB schema reset to restore search coverage.
    /// Failures on individual rows are logged and skipped so a single bad
    /// entry does not abort the whole reindex.
    pub async fn reindex(&self) -> Result<()> {
        let entries = self.history.fetch_all().await?;
        let total = entries.len();
        tracing::info!("reindexing {total} entries into LanceDB");

        for entry in entries {
            match self.semantic.embeddings.embed_query(&entry.content).await {
                Ok(embedding) => {
                    if let Err(e) = self
                        .semantic
                        .store_vector(entry.id, &entry.category, entry.importance, embedding)
                        .await
                    {
                        tracing::warn!("reindex: failed to store vector for entry {}: {e}", entry.id);
                    }
                }
                Err(e) => {
                    tracing::warn!("reindex: failed to embed entry {}: {e}", entry.id);
                }
            }
        }

        tracing::info!("reindex complete");
        Ok(())
    }

    /// Import entries from a JSONL file into both SQLite and LanceDB.
    ///
    /// Each line must be a JSON object with `category`, `content`, and
    /// optional `role` (default "system") and `importance` (default 5.0).
    pub async fn import_jsonl(&self, path: &std::path::Path) -> Result<usize> {
        let file_content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read import file: {}", path.display()))?;
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut count = 0;

        for (line_num, line) in file_content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let record: ImportRecord = serde_json::from_str(line)
                .with_context(|| format!("invalid JSON on line {}", line_num + 1))?;

            self.store(
                &record.category,
                &record.role,
                &record.content,
                &timestamp,
                record.importance,
                None,
                "human",
            )
            .await
            .with_context(|| format!("failed to store entry on line {}", line_num + 1))?;

            count += 1;
        }

        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Reciprocal Rank Fusion
// ---------------------------------------------------------------------------

/// Merge two ranked id lists into a single ranking via RRF.
///
/// k=60 is the standard constant from the original RRF paper.
fn rrf_merge(list_a: &[i64], list_b: &[i64], k: f64) -> Vec<i64> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (rank, id) in list_a.iter().enumerate() {
        *scores.entry(*id).or_default() += 1.0 / (k + rank as f64 + 1.0);
    }
    for (rank, id) in list_b.iter().enumerate() {
        *scores.entry(*id).or_default() += 1.0 / (k + rank as f64 + 1.0);
    }
    let mut ids: Vec<(i64, f64)> = scores.into_iter().collect();
    ids.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ids.into_iter().map(|(id, _)| id).collect()
}

// ---------------------------------------------------------------------------
// UserStorePool — LRU pool of per-user UserStore instances
// ---------------------------------------------------------------------------

/// LRU pool of per-user `UserStore` instances.
///
/// Opening databases holds file descriptors, so we cap the number of
/// concurrently-open users and evict the least-recently-used ones.
pub struct UserStorePool {
    cache: Mutex<LruCache<String, Arc<UserStore>>>,
    base_path: String,
    table_name: String,
    top_k: usize,
    dim: usize,
    rerank_top_n: usize,
    category_weights: HashMap<String, f32>,
    chat_window_size: usize,
    embeddings: Arc<dyn Embeddings>,
    reranker: Arc<dyn Reranker>,
}

impl UserStorePool {
    pub fn new(
        config: &MemoryConfig,
        embeddings: Arc<dyn Embeddings>,
        reranker: Arc<dyn Reranker>,
        dim: usize,
        rerank_top_n: usize,
        category_weights: HashMap<String, f32>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.base_path).with_context(|| {
            format!(
                "failed to create memory base directory: {}",
                config.base_path
            )
        })?;

        let cap = std::num::NonZeroUsize::new(config.pool_size)
            .context("pool_size must be > 0")?;

        Ok(Self {
            cache: Mutex::new(LruCache::new(cap)),
            base_path: config.base_path.clone(),
            table_name: config.table_name.clone(),
            top_k: config.top_k,
            dim,
            rerank_top_n,
            category_weights,
            chat_window_size: config.chat_window,
            embeddings,
            reranker,
        })
    }

    /// Get or create a `UserStore` for the given user.
    pub async fn get(&self, user_id: &str) -> Result<Arc<UserStore>> {
        {
            let mut cache = self.cache.lock().await;
            if let Some(mem) = cache.get(user_id) {
                return Ok(Arc::clone(mem));
            }
        }

        let lance_path = format!("{}/{}.lancedb", self.base_path, user_id);
        let sqlite_path = format!("{}/{}.db", self.base_path, user_id);

        // Open SQLite first — migrations run here, and the LanceDB version
        // is stored in the meta table which migrations ensure exists.
        let history = MemoryHistory::new(&sqlite_path)
            .await
            .with_context(|| format!("failed to open memory history for user '{user_id}'"))?;

        let needs_reset = migrations::lancedb_needs_reset(history.pool())
            .await
            .with_context(|| format!("failed to check lancedb version for user '{user_id}'"))?;

        let semantic = SemanticMemory::new(
            &lance_path,
            &self.table_name,
            needs_reset,
            Arc::clone(&self.embeddings),
            Arc::clone(&self.reranker),
            self.dim,
            self.top_k,
            self.rerank_top_n,
            self.category_weights.clone(),
            self.chat_window_size,
        )
        .await
        .with_context(|| format!("failed to open semantic memory for user '{user_id}'"))?;

        let user_mem = Arc::new(UserStore { semantic, history });

        if needs_reset {
            migrations::mark_lancedb_current(user_mem.history.pool())
                .await
                .with_context(|| format!("failed to mark lancedb current for user '{user_id}'"))?;
            if let Err(e) = user_mem.reindex().await {
                tracing::warn!("reindex failed for user '{user_id}': {e}");
            }
        }

        let mut cache = self.cache.lock().await;
        if let Some(existing) = cache.get(user_id) {
            return Ok(Arc::clone(existing));
        }
        cache.put(user_id.to_string(), Arc::clone(&user_mem));

        Ok(user_mem)
    }
}
