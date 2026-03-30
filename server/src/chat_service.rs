use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use synaptic::core::{MemoryStore, Message};
use synaptic::graph::{CompiledGraph, MessageState};
use tokio::sync::broadcast;

use opencc_rust::{DefaultConfig, OpenCC};

use crate::context;
use crate::memory::UserStorePool;

const CHANNEL_CAPACITY: usize = 32;

/// Events pushed to all connected clients of the same user.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserEvent {
    HumanMessage { id: i64, content: String, timestamp: String },
    AiResponse   { id: i64, content: String, timestamp: String },
    Error        { message: String },
}

// ---------------------------------------------------------------------------
// Structured LLM response parsing (moved from main.rs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StructuredLlmResponse {
    response: String,
    #[serde(default = "default_importance")]
    importance: f32,
}

fn default_importance() -> f32 {
    5.0
}

/// Parse the LLM output as JSON `{"response": "...", "importance": N}`.
/// Falls back to treating the entire string as the response with default importance.
pub fn parse_structured_response(raw: &str) -> (String, f32) {
    if let Ok(parsed) = serde_json::from_str::<StructuredLlmResponse>(raw) {
        return (parsed.response, parsed.importance.clamp(1.0, 10.0));
    }
    if let Some(start) = raw.find('{') {
        if let Some(end) = raw.rfind('}') {
            if let Ok(parsed) = serde_json::from_str::<StructuredLlmResponse>(&raw[start..=end]) {
                return (parsed.response, parsed.importance.clamp(1.0, 10.0));
            }
        }
    }
    (raw.to_string(), 5.0)
}

// ---------------------------------------------------------------------------
// ChatService
// ---------------------------------------------------------------------------

pub struct ChatService {
    pool: Arc<UserStorePool>,
    graph: CompiledGraph<MessageState>,
    system_prompt: String,
    tool_failure_count: Arc<AtomicUsize>,
    /// Per-user broadcast channels. Created lazily on first subscribe.
    channels: DashMap<String, broadcast::Sender<UserEvent>>,
    /// Converts any Simplified Chinese characters in LLM output to Traditional Chinese (Taiwan).
    opencc: Option<Mutex<OpenCC>>,
}

impl ChatService {
    pub fn new(
        pool: Arc<UserStorePool>,
        graph: CompiledGraph<MessageState>,
        system_prompt: String,
        tool_failure_count: Arc<AtomicUsize>,
    ) -> Self {
        let opencc = match OpenCC::new(DefaultConfig::S2TWP) {
            Ok(cc) => {
                tracing::info!("OpenCC S2TWP initialised — Simplified→Traditional safeguard active");
                Some(Mutex::new(cc))
            }
            Err(e) => {
                tracing::warn!("OpenCC init failed, Traditional Chinese safeguard disabled: {e}");
                None
            }
        };
        Self {
            pool,
            graph,
            system_prompt,
            tool_failure_count,
            channels: DashMap::new(),
            opencc,
        }
    }

    /// Expose the pool so other handlers (store, search, history) can reach it.
    pub fn pool(&self) -> &Arc<UserStorePool> {
        &self.pool
    }

    /// Convert any Simplified Chinese characters to Traditional Chinese (Taiwan).
    /// No-op for non-Chinese text or when OpenCC is unavailable.
    pub fn convert(&self, text: &str) -> String {
        if let Some(ref cc) = self.opencc {
            cc.lock().unwrap().convert(text)
        } else {
            text.to_string()
        }
    }

    /// Subscribe to events for a user. Each caller gets its own receiver.
    /// If the user has no channel yet, one is created.
    pub fn subscribe(&self, user_id: &str) -> broadcast::Receiver<UserEvent> {
        self.channels
            .entry(user_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Persist and broadcast the human message. Returns the timestamp used so
    /// the caller can pass it to `run_llm` for a consistent conversation timestamp.
    /// This is intentionally fast (one DB write) so the HTTP handler can return quickly.
    pub async fn persist_human_message(
        &self,
        user_id: &str,
        message: &str,
    ) -> Result<String> {
        let user_mem = self.pool.get(user_id).await?;
        let now = Utc::now().to_rfc3339();
        let human_id = user_mem
            .store("conversation", "human", message, &now, 5.0)
            .await?;
        self.broadcast(user_id, UserEvent::HumanMessage {
            id: human_id,
            content: message.to_string(),
            timestamp: now.clone(),
        });
        Ok(now)
    }

    /// Invoke the LLM and broadcast the AI response. Intended to be called from
    /// a background task after `persist_human_message` has already returned.
    pub async fn run_llm(
        &self,
        user_id: &str,
        message: &str,
        local_time: Option<&str>,
        now: &str,
    ) -> Result<()> {
        let user_mem = self.pool.get(user_id).await?;

        // Build message list: system prompt + recent chat window + timestamp + user message.
        let mut messages = vec![Message::system(&self.system_prompt)];
        let history = user_mem
            .semantic
            .chat_memory
            .load(user_id)
            .await
            .context("failed to load chat history")?;
        messages.extend(history);
        let time_str = match local_time {
            Some(t) => format!("Current date and time: {}", t),
            None => format!(
                "Current date and time (UTC): {}",
                Utc::now().format("%A, %B %-d, %Y %H:%M UTC")
            ),
        };
        messages.push(Message::system(&time_str));
        messages.push(Message::human(message));

        // Invoke LLM agent.
        let msg_state = MessageState { messages };
        self.tool_failure_count.store(0, Ordering::SeqCst);
        let result = context::CURRENT_USER_ID
            .scope(user_id.to_string(), self.graph.invoke(msg_state))
            .await
            .context("LLM invocation failed")?;

        let raw = result
            .state()
            .last_message()
            .map(|m: &Message| m.content().to_string())
            .unwrap_or_default();

        let (response, importance) = parse_structured_response(&raw);
        tracing::debug!(importance = importance, "chat response importance");

        // Convert any Simplified Chinese characters to Traditional Chinese (Taiwan).
        // No-op for non-Chinese text.
        let response = if let Some(ref cc) = self.opencc {
            cc.lock().unwrap().convert(&response)
        } else {
            response
        };

        // Persist AI response to SQLite, LanceDB, and the chat window.
        let ai_id = user_mem
            .store("conversation", "ai", &response, now, importance)
            .await?;
        user_mem
            .semantic
            .chat_memory
            .append(user_id, Message::human(message))
            .await
            .context("failed to append human message to chat window")?;
        user_mem
            .semantic
            .chat_memory
            .append(user_id, Message::ai(&response))
            .await
            .context("failed to append AI message to chat window")?;

        // Broadcast AI response to all connected clients.
        self.broadcast(user_id, UserEvent::AiResponse {
            id: ai_id,
            content: response,
            timestamp: now.to_string(),
        });

        Ok(())
    }

    /// Remove channels that have no active subscribers.
    pub fn cleanup_channels(&self) {
        self.channels.retain(|_, sender| sender.receiver_count() > 0);
    }

    pub fn broadcast_error(&self, user_id: &str, message: &str) {
        self.broadcast(user_id, UserEvent::Error { message: message.to_string() });
    }

    fn broadcast(&self, user_id: &str, event: UserEvent) {
        if let Some(sender) = self.channels.get(user_id) {
            // Ignore send errors — they just mean no clients are connected.
            let _ = sender.send(event);
        }
    }
}
