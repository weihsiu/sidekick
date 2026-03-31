use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

use crate::context::CURRENT_USER_ID;
use crate::memory::{format_context, UserStorePool};

/// Tool that lets the LLM search the user's long-term memory on demand.
pub struct RecallMemory {
    pool: Arc<UserStorePool>,
}

impl RecallMemory {
    pub fn new(pool: Arc<UserStorePool>) -> Arc<dyn Tool> {
        Arc::new(Self { pool })
    }
}

#[async_trait]
impl Tool for RecallMemory {
    fn name(&self) -> &'static str {
        "recall_memory"
    }

    fn description(&self) -> &'static str {
        "Search the user's long-term memory for past conversations, facts, and preferences. \
         Use this when you need context from previous interactions that isn't in the recent chat history."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language search query to find relevant memories."
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("query is required".into()))?;

        let user_id = CURRENT_USER_ID
            .try_with(|id| id.clone())
            .map_err(|_| SynapticError::Tool("no user context available".into()))?;

        let user_mem = self
            .pool
            .get(&user_id)
            .await
            .map_err(|e| SynapticError::Tool(format!("failed to access memory: {e}")))?;

        tracing::info!(user_id, query, "recall_memory called");

        let entries = user_mem
            .retrieve(query, None)
            .await
            .map_err(|e| SynapticError::Tool(format!("memory search failed: {e}")))?;

        if entries.is_empty() {
            tracing::info!(user_id, query, "recall_memory: no entries found");
            return Ok(serde_json::json!("No relevant memories found."));
        }

        tracing::info!(user_id, query, count = entries.len(), "recall_memory: returning entries");
        for entry in &entries {
            tracing::debug!(
                user_id,
                id = entry.id,
                category = %entry.category,
                role = %entry.role,
                importance = entry.importance,
                content = %entry.content,
                "recall_memory entry"
            );
        }

        Ok(serde_json::json!(format_context(&entries)))
    }
}
