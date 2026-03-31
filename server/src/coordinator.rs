use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use sqlx::SqlitePool;
use synaptic::core::{ChatModel, Message, SynapticError, Tool};
use synaptic::graph::{create_react_agent, MessageState};
use tokio::time::{timeout, Duration};
use uuid::Uuid;

/// Runs its own agentic loop to fulfill a user's request by communicating
/// with other agents via HTTP + SSE. The coordinator LLM has two tools:
/// `find_agent` (directory lookup) and `message_agent` (send + await response).
/// It decides when enough information has been gathered and returns the answer.
pub struct CoordinatorAgent {
    /// user_id of the agent that started this session.
    pub initiator_user_id: String,
    /// Display name of the initiating user (from their profile DB).
    pub initiator_name: String,
    /// This server's base URL — used to reach all local agents.
    pub base_url: String,
    /// Shared secret for server-to-server Bearer auth.
    pub secret: String,
    /// Global DB for directory lookups.
    pub db: Arc<SqlitePool>,
    /// LLM that drives the coordinator reasoning loop.
    pub model: Arc<dyn ChatModel>,
}

impl CoordinatorAgent {
    /// Fire-and-forget entry point: runs the coordination loop then delivers
    /// the conclusion back to the initiating agent as a visible chat message.
    /// Intended to be called inside `tokio::spawn`.
    pub async fn run_and_deliver(&self, user_request: &str) {
        let session_id = Uuid::new_v4().to_string();
        tracing::info!(session_id, initiator = %self.initiator_name, "coordinator session started");
        tracing::info!(session_id, "user request: {user_request}");
        match self.run_loop(user_request, &session_id).await {
            Ok(conclusion) => {
                tracing::info!(session_id, "coordinator concluded: {conclusion}");
                self.deliver_conclusion(user_request, &conclusion, &session_id).await;
            }
            Err(e) => tracing::error!(session_id, "coordinator loop failed: {e:#}"),
        }
    }

    /// Deliver the final conclusion to the initiating agent as a visible reply.
    async fn deliver_conclusion(&self, original_request: &str, conclusion: &str, session_id: &str) {
        let content = format!(
            "The user's original request was: \"{original_request}\"\n\nCoordination result: {conclusion}"
        );
        let http = Client::new();
        let url = format!("{}/v1/coordinator/message", self.base_url);
        match http
            .post(&url)
            .bearer_auth(&self.secret)
            .json(&serde_json::json!({
                "user_id":        self.initiator_user_id,
                "session_id":     session_id,
                "content":        content,
                "is_conclusion":  true,
                "session_type":   "agent_coordination",
                "initiator_name": self.initiator_name,
            }))
            .send()
            .await
        {
            Ok(resp) if !resp.status().is_success() => {
                tracing::error!(session_id, status = %resp.status(), "conclusion delivery rejected by server");
            }
            Err(e) => {
                tracing::error!(session_id, "failed to deliver conclusion to initiator: {e:#}");
            }
            Ok(_) => {}
        }
    }

    /// Run the coordinator ReAct loop and return the final answer text.
    async fn run_loop(&self, user_request: &str, session_id: &str) -> Result<String> {
        let session_id = Arc::new(session_id.to_string());
        let http = Client::new();

        let find_agent = Arc::new(FindAgentTool { db: self.db.clone() }) as Arc<dyn Tool>;
        let message_agent = Arc::new(MessageAgentTool {
            http,
            secret: self.secret.clone(),
            base_url: self.base_url.clone(),
            session_id: session_id.clone(),
            initiator_name: self.initiator_name.clone(),
        }) as Arc<dyn Tool>;

        let graph = create_react_agent(self.model.clone(), vec![find_agent, message_agent])
            .context("failed to create coordinator agent")?;

        let system_prompt = format!(
            "You are a coordinator agent acting on behalf of {initiator_name}. \
             Your job is to fulfill their request by communicating with other users' agents.\n\n\
             When you send a message to another agent, always identify yourself as a coordinator \
             and state who you are coordinating for. For example, start your message with: \
             \"I am a coordinator agent acting on behalf of {initiator_name}. [your message]\"\n\n\
             Tools:\n\
             - `find_agent`: search the directory by name or @handle — returns user_id values\n\
             - `message_agent`: send a message to an agent by user_id and receive their response\n\n\
             If you need more context from {initiator_name}, you can also message their agent \
             (user_id: `{initiator_id}`).\n\n\
             When you have enough information to answer the original request, return the final \
             answer directly without calling any more tools. \
             Do not invent information — only use what agents actually tell you.",
            initiator_name = self.initiator_name,
            initiator_id = self.initiator_user_id,
        );

        let msg_state = MessageState {
            messages: vec![
                Message::system(&system_prompt),
                Message::human(user_request),
            ],
        };

        let result = graph.invoke(msg_state).await
            .context("coordinator agent loop failed")?;

        Ok(result
            .state()
            .last_message()
            .map(|m: &Message| m.content().to_string())
            .unwrap_or_else(|| "No conclusion reached.".to_string()))
    }
}

// ---------------------------------------------------------------------------
// FindAgentTool — queries the server directory
// ---------------------------------------------------------------------------

struct FindAgentTool {
    db: Arc<SqlitePool>,
}

#[async_trait]
impl Tool for FindAgentTool {
    fn name(&self) -> &'static str {
        "find_agent"
    }

    fn description(&self) -> &'static str {
        "Search the server directory for agents by name or @handle. \
         Returns a list of matching agents with their user_id. \
         Use the user_id with message_agent to contact the agent."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Name or @handle to search for."
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
            "SELECT user_id, display_name, handle FROM directory \
             WHERE visible = 1 AND (display_name LIKE ? OR handle = ?) \
             ORDER BY display_name LIMIT 20",
        )
        .bind(&like_pattern)
        .bind(query)
        .fetch_all(self.db.as_ref())
        .await
        .map_err(|e| SynapticError::Tool(format!("directory query failed: {e}")))?;

        if rows.is_empty() {
            return Ok(serde_json::json!(format!("No agents found matching '{query}'.")));
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

        Ok(serde_json::json!(
            serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string())
        ))
    }
}

// ---------------------------------------------------------------------------
// MessageAgentTool — sends a message to an agent and awaits the SSE response
// ---------------------------------------------------------------------------

struct MessageAgentTool {
    http: Client,
    secret: String,
    base_url: String,
    session_id: Arc<String>,
    initiator_name: String,
}

#[async_trait]
impl Tool for MessageAgentTool {
    fn name(&self) -> &'static str {
        "message_agent"
    }

    fn description(&self) -> &'static str {
        "Send a message to an agent by user_id and wait for their response. \
         Use find_agent first to get a user_id. \
         The agent will consult their user's context and reply."
    }

    fn parameters(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "user_id": {
                    "type": "string",
                    "description": "user_id of the agent to message (from find_agent, or the initiator's known user_id)."
                },
                "name": {
                    "type": "string",
                    "description": "Display name of the agent (from find_agent). Used for logging."
                },
                "message": {
                    "type": "string",
                    "description": "The message to send."
                }
            },
            "required": ["user_id", "message"]
        }))
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        let user_id = args["user_id"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("user_id is required".into()))?;
        let message = args["message"]
            .as_str()
            .ok_or_else(|| SynapticError::Tool("message is required".into()))?;
        let agent_name = args["name"].as_str().unwrap_or(user_id);

        let response = send_and_await(
            &self.http,
            &self.base_url,
            &self.secret,
            user_id,
            agent_name,
            &self.session_id,
            &self.initiator_name,
            message,
        )
        .await
        .map_err(|e| SynapticError::Tool(format!("message_agent failed: {e}")))?;

        Ok(serde_json::json!(response))
    }
}

// ---------------------------------------------------------------------------
// HTTP + SSE helper shared by MessageAgentTool
// ---------------------------------------------------------------------------

async fn send_and_await(
    http: &Client,
    agent_url: &str,
    secret: &str,
    user_id: &str,
    agent_name: &str,
    session_id: &str,
    initiator_name: &str,
    content: &str,
) -> Result<String> {
    // Subscribe to SSE *before* posting so we cannot miss the response.
    let events_url = format!(
        "{}/v1/coordinator/events/{}?session_id={}",
        agent_url, user_id, session_id
    );
    let http_clone = http.clone();
    let secret_owned = secret.to_string();

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();

    let sse_task = tokio::spawn(async move {
        match http_clone.get(&events_url).bearer_auth(&secret_owned).send().await {
            Ok(resp) => {
                let mut stream = resp.bytes_stream();
                let mut buf = String::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(bytes) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(pos) = buf.find('\n') {
                                let line = buf[..pos].trim_end_matches('\r').to_string();
                                buf = buf[pos + 1..].to_string();
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if let Ok(ev) = serde_json::from_str::<Value>(data) {
                                        if ev["type"] == "coordinator_response" {
                                            let content = ev["content"]
                                                .as_str()
                                                .unwrap_or("")
                                                .to_string();
                                            let _ = tx.send(content);
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("coordinator SSE read error: {e}");
                            return;
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("coordinator SSE connect failed: {e}"),
        }
    });

    // Post the coordinator message.
    tracing::info!(session_id, agent_name, "coordinator → agent: {content}");
    http.post(&format!("{}/v1/coordinator/message", agent_url))
        .bearer_auth(secret)
        .json(&serde_json::json!({
            "user_id":        user_id,
            "session_id":     session_id,
            "content":        content,
            "is_conclusion":  false,
            "session_type":   "agent_coordination",
            "initiator_name": initiator_name,
        }))
        .send()
        .await
        .context("failed to post coordinator message")?;

    // Wait for the SSE response, then always abort the SSE task.
    let result = timeout(Duration::from_secs(60), rx).await;
    sse_task.abort();
    let response = result
        .context("timed out waiting for agent response")?
        .context("SSE channel closed without a coordinator_response event")?;
    tracing::info!(session_id, agent_name, "agent → coordinator: {response}");
    Ok(response)
}
