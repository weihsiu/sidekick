use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::query::{ExecutableQuery, QueryBase};
use lru::LruCache;
use serde::Deserialize;
use synaptic::core::Embeddings;
use synaptic::memory::{ChatMessageHistory, ConversationWindowMemory};
use synaptic::store::InMemoryStore;
use tokio::sync::Mutex;

use crate::config::MemoryConfig;
use crate::rerank::Reranker;

/// A single entry stored in memory.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub category: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

/// A record in a JSONL import file.
#[derive(Debug, Deserialize)]
pub struct ImportRecord {
    pub category: String,
    pub content: String,
    /// Optional role — defaults to "system" for imported knowledge.
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "system".to_string()
}

/// Persistent conversation memory backed by LanceDB.
///
/// Thread-safe: a write lock serialises `store` and FTS index rebuilds,
/// while reads proceed concurrently without contention.
///
/// Also holds a `ConversationWindowMemory` for short-term chat context
/// (last N messages sent to the LLM as message history).
pub struct ConversationMemory {
    table: lancedb::Table,
    embeddings: Arc<dyn Embeddings>,
    reranker: Arc<dyn Reranker>,
    dim: usize,
    top_k: usize,
    rerank_top_n: usize,
    category_weights: HashMap<String, f32>,
    /// Serialises writes so concurrent `store` calls don't race on FTS rebuilds.
    write_lock: Mutex<()>,
    /// Short-term chat window backed by synaptic's ConversationWindowMemory.
    pub chat_memory: Arc<ConversationWindowMemory>,
}

impl ConversationMemory {
    /// Connect to (or create) the LanceDB table for conversation memory.
    pub async fn new(
        db_path: &str,
        table_name: &str,
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
        let table = if existing.contains(&table_name.to_string()) {
            db.open_table(table_name).execute().await?
        } else {
            db.create_empty_table(table_name, schema)
                .execute()
                .await?
        };

        let mem = Self {
            table,
            embeddings,
            reranker,
            dim,
            top_k,
            rerank_top_n,
            category_weights,
            write_lock: Mutex::new(()),
            chat_memory: Arc::new(ConversationWindowMemory::new(
                Arc::new(ChatMessageHistory::new(Arc::new(InMemoryStore::new()))),
                chat_window_size,
            )),
        };

        // Ensure the FTS index exists so hybrid search works on existing data.
        if mem.table.count_rows(None).await? > 0 {
            mem.rebuild_fts_index().await?;
        }

        Ok(mem)
    }

    fn schema(dim: usize) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("category", DataType::Utf8, false),
            Field::new("role", DataType::Utf8, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("timestamp", DataType::Utf8, false),
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

    /// Rebuild the full-text search index on the content column.
    async fn rebuild_fts_index(&self) -> Result<()> {
        self.table
            .create_index(&["content"], Index::FTS(Default::default()))
            .replace(true)
            .execute()
            .await
            .context("failed to rebuild FTS index")?;
        Ok(())
    }

    /// Store a single entry in memory (thread-safe).
    pub async fn store(&self, category: &str, role: &str, content: &str) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        // Compute embedding outside the lock — this is the slow part.
        let embedding = self
            .embeddings
            .embed_query(content)
            .await
            .context("failed to compute embedding")?;

        let _guard = self.write_lock.lock().await;
        self.insert_row(category, role, content, &id, &timestamp, embedding)
            .await?;
        self.rebuild_fts_index().await?;

        Ok(())
    }

    /// Insert a row with a precomputed embedding.
    async fn insert_row(
        &self,
        category: &str,
        role: &str,
        content: &str,
        id: &str,
        timestamp: &str,
        embedding: Vec<f32>,
    ) -> Result<()> {
        let schema = Self::schema(self.dim);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![id])),
                Arc::new(StringArray::from(vec![category])),
                Arc::new(StringArray::from(vec![role])),
                Arc::new(StringArray::from(vec![content])),
                Arc::new(StringArray::from(vec![timestamp])),
                Arc::new(
                    FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                        vec![Some(embedding.into_iter().map(Some).collect::<Vec<_>>())],
                        self.dim as i32,
                    ),
                ),
            ],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.table.add(batches).execute().await?;

        Ok(())
    }

    /// Import entries from a JSONL file. Rebuilds the FTS index once at the end.
    pub async fn import_jsonl(&self, path: &std::path::Path) -> Result<usize> {
        let file_content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read import file: {}", path.display()))?;
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut count = 0;

        let _guard = self.write_lock.lock().await;

        for (line_num, line) in file_content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let record: ImportRecord = serde_json::from_str(line)
                .with_context(|| format!("invalid JSON on line {}", line_num + 1))?;

            let id = uuid::Uuid::new_v4().to_string();
            let embedding = self
                .embeddings
                .embed_query(&record.content)
                .await
                .with_context(|| format!("embedding failed on line {}", line_num + 1))?;
            self.insert_row(
                &record.category,
                &record.role,
                &record.content,
                &id,
                &timestamp,
                embedding,
            )
            .await?;

            count += 1;
        }

        if count > 0 {
            self.rebuild_fts_index().await?;
        }

        Ok(count)
    }

    /// Retrieve the most relevant past entries for a query, optionally filtered
    /// by categories.
    ///
    /// Uses hybrid search: dense vector (cosine similarity) + sparse full-text
    /// search on the content column, fused via Reciprocal Rank Fusion (RRF).
    pub async fn retrieve(
        &self,
        query: &str,
        categories: Option<&[&str]>,
    ) -> Result<Vec<MemoryEntry>> {
        // On an empty table, vector search would fail — return early.
        let row_count = self.table.count_rows(None).await?;
        if row_count == 0 {
            return Ok(Vec::new());
        }

        let query_vec = self
            .embeddings
            .embed_query(query)
            .await
            .context("failed to embed query")?;
        let fts_query = FullTextSearchQuery::new(query.to_string());

        let mut builder = self
            .table
            .query()
            .full_text_search(fts_query)
            .nearest_to(query_vec)?;

        // Optionally filter by categories.
        if let Some(cats) = categories {
            let cat_list: Vec<String> = cats
                .iter()
                .map(|c| format!("'{}'", c.replace('\'', "''")))
                .collect();
            builder = builder.only_if(format!("category IN ({})", cat_list.join(", ")));
        }

        let results: Vec<RecordBatch> = builder
            .limit(self.top_k)
            .distance_type(lancedb::DistanceType::Cosine)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;

        let mut entries = Vec::new();
        for batch in &results {
            let categories = batch
                .column_by_name("category")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let roles = batch
                .column_by_name("role")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let contents = batch
                .column_by_name("content")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let timestamps = batch
                .column_by_name("timestamp")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            for i in 0..batch.num_rows() {
                entries.push(MemoryEntry {
                    category: categories.value(i).to_string(),
                    role: roles.value(i).to_string(),
                    content: contents.value(i).to_string(),
                    timestamp: timestamps.value(i).to_string(),
                });
            }
        }

        // Rerank to surface the most relevant entries, then apply category weights.
        let docs: Vec<&str> = entries.iter().map(|e| e.content.as_str()).collect();
        let mut ranked = self.reranker.rerank(query, &docs, entries.len()).await?;
        for r in &mut ranked {
            let weight = self
                .category_weights
                .get(&entries[r.index].category)
                .copied()
                .unwrap_or(1.0);
            r.score *= weight;
        }
        ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(self.rerank_top_n);
        let entries: Vec<MemoryEntry> =
            ranked.into_iter().map(|r| entries[r.index].clone()).collect();

        // Sort by timestamp so context reads chronologically.
        let mut entries = entries;
        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(entries)
    }
}

/// LRU pool of per-user `ConversationMemory` instances.
///
/// Opening a LanceDB is relatively cheap but holds file descriptors, so we
/// cap the number of concurrently-open databases and evict the least-recently-
/// used ones.
pub struct MemoryPool {
    cache: Mutex<LruCache<String, Arc<ConversationMemory>>>,
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

impl MemoryPool {
    pub fn new(
        config: &MemoryConfig,
        embeddings: Arc<dyn Embeddings>,
        reranker: Arc<dyn Reranker>,
        dim: usize,
        rerank_top_n: usize,
        category_weights: HashMap<String, f32>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.base_path)
            .with_context(|| format!("failed to create memory base directory: {}", config.base_path))?;

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

    /// Get or create a `ConversationMemory` for the given user.
    pub async fn get(&self, user_id: &str) -> Result<Arc<ConversationMemory>> {
        // Fast path: check the cache.
        {
            let mut cache = self.cache.lock().await;
            if let Some(mem) = cache.get(user_id) {
                return Ok(Arc::clone(mem));
            }
        }

        // Slow path: open/create the DB outside the cache lock.
        let db_path = format!("{}/{}.lancedb", self.base_path, user_id);
        let mem = Arc::new(
            ConversationMemory::new(
                &db_path,
                &self.table_name,
                Arc::clone(&self.embeddings),
                Arc::clone(&self.reranker),
                self.dim,
                self.top_k,
                self.rerank_top_n,
                self.category_weights.clone(),
                self.chat_window_size,
            )
            .await
            .with_context(|| format!("failed to open memory for user '{user_id}'"))?,
        );

        let mut cache = self.cache.lock().await;
        // Another task may have inserted while we were creating — use their
        // instance if so.
        if let Some(existing) = cache.get(user_id) {
            return Ok(Arc::clone(existing));
        }
        cache.put(user_id.to_string(), Arc::clone(&mem));

        Ok(mem)
    }
}

/// Format retrieved memory entries into a context string for the system prompt.
pub fn format_context(entries: &[MemoryEntry]) -> String {
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
