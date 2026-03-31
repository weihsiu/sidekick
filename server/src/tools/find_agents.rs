use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;
use synaptic::core::{SynapticError, Tool};

/// Tool that searches the server's public directory for other agents.
///
/// Every user who has logged in appears in the directory. Results include
/// the `user_id` needed to start a coordination session.
pub struct FindAgents {
    db: Arc<SqlitePool>,
}

impl FindAgents {
    pub fn new(db: Arc<SqlitePool>) -> Arc<dyn Tool> {
        Arc::new(Self { db })
    }
}

#[async_trait]
impl Tool for FindAgents {
    fn name(&self) -> &'static str {
        "find_agents"
    }

    fn description(&self) -> &'static str {
        "Search the server directory for other users and their agents. \
         Returns user_id, display_name, and handle. \
         Use the user_id with start_coordination_session to contact an agent."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Name or handle to search for (case-insensitive partial match on display_name, exact match on handle)."
                }
            },
            "required": ["query"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("query is required".into()))?
            .trim_start_matches('@');

        let like_pattern = format!("%{}%", query);

        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT user_id, display_name, handle
             FROM directory
             WHERE visible = 1
               AND (display_name LIKE ? OR handle = ?)
             ORDER BY display_name
             LIMIT 20",
        )
        .bind(&like_pattern)
        .bind(query)
        .fetch_all(self.db.as_ref())
        .await
        .map_err(|e| SynapticError::Tool(format!("directory query failed: {e}")))?;

        if rows.is_empty() {
            return Ok(serde_json::json!(
                format!("No agents found matching '{query}'.")
            ));
        }

        let results: Vec<Value> = rows
            .into_iter()
            .map(|(user_id, display_name, handle)| {
                serde_json::json!({
                    "user_id":      user_id,
                    "display_name": display_name,
                    "handle":       handle,
                })
            })
            .collect();

        let text = serde_json::to_string_pretty(&results)
            .unwrap_or_else(|_| "[]".to_string());
        Ok(serde_json::json!(text))
    }
}
