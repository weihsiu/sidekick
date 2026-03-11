use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::Index;
use lancedb::query::{ExecutableQuery, QueryBase};
use serde::Deserialize;
use synaptic::core::Embeddings;

use crate::config::MemoryConfig;

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
/// Uses hybrid search (dense vector + full-text) via Reciprocal Rank Fusion
/// so that retrieval benefits from both semantic similarity and keyword matching.
pub struct ConversationMemory {
    table: lancedb::Table,
    embeddings: Arc<dyn Embeddings>,
    dim: usize,
    top_k: usize,
}

impl ConversationMemory {
    /// Connect to (or create) the LanceDB table for conversation memory.
    pub async fn new(
        config: &MemoryConfig,
        embeddings: Arc<dyn Embeddings>,
        dim: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(&config.db_path)?;

        let db = lancedb::connect(&config.db_path).execute().await?;

        let schema = Self::schema(dim);
        let existing = db.table_names().execute().await?;
        let table = if existing.contains(&config.table_name) {
            db.open_table(&config.table_name).execute().await?
        } else {
            db.create_empty_table(&config.table_name, schema)
                .execute()
                .await?
        };

        let mem = Self {
            table,
            embeddings,
            dim,
            top_k: config.top_k,
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
            Field::new("user_id", DataType::Utf8, false),
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
    async fn rebuild_fts_index(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.table
            .create_index(&["content"], Index::FTS(Default::default()))
            .replace(true)
            .execute()
            .await?;
        Ok(())
    }

    /// Store a single entry in memory.
    pub async fn store(
        &self,
        user_id: &str,
        category: &str,
        role: &str,
        content: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        self.store_raw(user_id, category, role, content, &id, &timestamp)
            .await?;

        // Rebuild the FTS index so newly added content is searchable.
        self.rebuild_fts_index().await?;

        Ok(())
    }

    /// Store with explicit id and timestamp (used by batch import).
    async fn store_raw(
        &self,
        user_id: &str,
        category: &str,
        role: &str,
        content: &str,
        id: &str,
        timestamp: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let embedding = self.embeddings.embed_query(content).await?;

        let schema = Self::schema(self.dim);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![id])),
                Arc::new(StringArray::from(vec![user_id])),
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
    pub async fn import_jsonl(
        &self,
        path: &std::path::Path,
        user_id: &str,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path)?;
        let timestamp = chrono::Utc::now().to_rfc3339();
        let mut count = 0;

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let record: ImportRecord = serde_json::from_str(line).map_err(|e| {
                format!("line {}: {e}", line_num + 1)
            })?;

            let id = uuid::Uuid::new_v4().to_string();
            self.store_raw(
                user_id,
                &record.category,
                &record.role,
                &record.content,
                &id,
                &timestamp,
            )
            .await?;

            count += 1;
        }

        if count > 0 {
            self.rebuild_fts_index().await?;
        }

        Ok(count)
    }

    /// Retrieve the most relevant past entries for a query, filtered by user_id
    /// and optionally by categories.
    ///
    /// Uses hybrid search: dense vector (cosine similarity) + sparse full-text
    /// search on the content column, fused via Reciprocal Rank Fusion (RRF).
    pub async fn retrieve(
        &self,
        user_id: &str,
        query: &str,
        categories: Option<&[&str]>,
    ) -> Result<Vec<MemoryEntry>, Box<dyn std::error::Error + Send + Sync>> {
        // On an empty table, vector search would fail — return early.
        let row_count = self.table.count_rows(None).await?;
        if row_count == 0 {
            return Ok(Vec::new());
        }

        let query_vec = self.embeddings.embed_query(query).await?;
        let fts_query = FullTextSearchQuery::new(query.to_string());

        // Build filter: always filter by user_id, optionally by categories.
        let mut filter = format!("user_id = '{}'", user_id.replace('\'', "''"));
        if let Some(cats) = categories {
            let cat_list: Vec<String> = cats
                .iter()
                .map(|c| format!("'{}'", c.replace('\'', "''")))
                .collect();
            filter.push_str(&format!(" AND category IN ({})", cat_list.join(", ")));
        }

        let results: Vec<RecordBatch> = self
            .table
            .query()
            .full_text_search(fts_query)
            .only_if(filter)
            .nearest_to(query_vec)?
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

        // Sort by timestamp so context reads chronologically.
        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok(entries)
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
