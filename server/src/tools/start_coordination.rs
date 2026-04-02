use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;
use synaptic::core::{SynapticError, Tool};

use crate::config::LlmConfig;
use crate::context::{COORDINATION_SPAWNED, CURRENT_USER_ID};
use crate::coordinator::CoordinatorAgent;
use crate::memory::UserStorePool;
use crate::provider;

/// Tool that launches a coordinator agentic loop to fulfill a user's request
/// by communicating with other agents. The coordinator finds relevant agents,
/// exchanges messages with them, and returns the conclusion directly so the
/// calling LLM can relay it to the user.
pub struct Coordinate {
    db: Arc<SqlitePool>,
    pool: Arc<UserStorePool>,
    llm_config: Arc<LlmConfig>,
    secret: Option<String>,
    /// This server's base URL — used to reach local agents.
    base_url: String,
}

impl Coordinate {
    pub fn new(
        db: Arc<SqlitePool>,
        pool: Arc<UserStorePool>,
        llm_config: Arc<LlmConfig>,
        secret: Option<String>,
        base_url: String,
    ) -> Arc<dyn Tool> {
        Arc::new(Self { db, pool, llm_config, secret, base_url })
    }
}

#[async_trait]
impl Tool for Coordinate {
    fn name(&self) -> &'static str {
        "coordinate"
    }

    fn description(&self) -> &'static str {
        "Coordinate with one or more agents on this server to fulfill a user's request. \
         Pass the full user request including all @mentions — the coordinator handles \
         finding and contacting every relevant agent itself. \
         Call this tool ONCE per user request, never once per @mention. \
         The coordinator will return a conclusion you can relay directly to the user."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": "The full user request the coordinator should fulfill, including any @mentions."
                }
            },
            "required": ["request"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let request = args["request"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("request is required".into()))?
            .to_string();

        let initiator_user_id = CURRENT_USER_ID
            .try_with(|id| id.clone())
            .map_err(|_| SynapticError::Tool("no user context available".into()))?;

        let secret = self
            .secret
            .clone()
            .ok_or_else(|| SynapticError::Tool("coordinator_secret is not configured".into()))?;

        let model = provider::build_model(&self.llm_config)
            .map_err(|e| SynapticError::Tool(format!("failed to build coordinator model: {e}")))?;

        let initiator_name = match self.pool.get(&initiator_user_id).await {
            Ok(m) => m.history.get_profile().await
                .ok()
                .flatten()
                .map(|p| p.name)
                .unwrap_or_else(|| initiator_user_id.clone()),
            Err(e) => {
                tracing::warn!(user_id = %initiator_user_id, "failed to load user store for initiator name: {e:#}");
                initiator_user_id.clone()
            }
        };

        let agent = CoordinatorAgent {
            initiator_user_id,
            initiator_name,
            base_url: self.base_url.clone(),
            secret,
            db: self.db.clone(),
            model,
        };

        tokio::spawn(async move {
            agent.run_and_deliver(&request).await;
        });

        COORDINATION_SPAWNED
            .try_with(|flag| flag.store(true, Ordering::SeqCst))
            .ok();

        Ok(serde_json::json!("Coordinating."))
    }
}
