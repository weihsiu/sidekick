mod auth;
mod chat_service;
mod config;
mod context;
mod coordinator;
mod embeddings;
mod error;
mod history;
mod memory;
mod migrations;
mod provider;
mod rerank;
mod stt;
mod tools;
mod user;

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::Key;
use axum_extra::extract::cookie::PrivateCookieJar;
use axum_login::{AuthManagerLayerBuilder, AuthSession};
use dotenvy::dotenv;
use futures::StreamExt;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use synaptic::core::MemoryStore;
use synaptic::core::Message;
use synaptic::graph::create_react_agent;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::SameSite;
use tower_sessions::{MemoryStore as SessionMemoryStore, SessionManagerLayer};

use auth::AuthBackend;
use auth::routes::require_user;
use chat_service::ChatService;
use error::ApiError;
use stt::SttClient;

type Jar = PrivateCookieJar<CookieKey>;

#[derive(RustEmbed)]
#[folder = "../client/dist/"]
struct Assets;

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let cache_control = if path == "index.html" || path == "sw.js" {
                "no-store"
            } else {
                "public, max-age=31536000, immutable"
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
                content.data.to_vec(),
            )
                .into_response()
        }
        None => match Assets::get("index.html") {
            Some(content) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/html".to_string()),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                content.data.to_vec(),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    }
}

/// Shared application state.
#[derive(Clone)]
pub struct CookieKey(Key);

impl From<CookieKey> for Key {
    fn from(k: CookieKey) -> Self {
        k.0
    }
}

struct AppState {
    chat_service: Arc<ChatService>,
    db: Arc<sqlx::SqlitePool>,
    cookie_key: CookieKey,
    stt: Option<Arc<SttClient>>,
    coordinator_secret: Option<String>,
}

impl axum::extract::FromRef<Arc<AppState>> for CookieKey {
    fn from_ref(state: &Arc<AppState>) -> Self {
        state.cookie_key.clone()
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
    local_time: Option<String>,
}

#[derive(Deserialize)]
struct StoreRequest {
    category: String,
    role: String,
    content: String,
    #[serde(default = "default_importance")]
    importance: f32,
}

fn default_importance() -> f32 {
    5.0
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default)]
    categories: Option<Vec<String>>,
}

#[derive(Serialize)]
struct SearchEntry {
    category: String,
    role: String,
    content: String,
    timestamp: String,
}

#[derive(Serialize)]
struct SearchResponse {
    entries: Vec<SearchEntry>,
}

#[derive(Serialize)]
struct MessageResponse {
    message: String,
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<i64>,
    category: Option<String>,
}

#[derive(Deserialize)]
struct CoordinatorMessageRequest {
    user_id: String,
    session_id: String,
    content: String,
    #[serde(default)]
    is_conclusion: bool,
    /// "agent_coordination" when the sender is another agent acting as orchestrator.
    session_type: String,
    /// Display name of the initiating user on whose behalf the coordinator is acting.
    initiator_name: String,
    /// Caller's local time string (e.g. from JS Date.toString()). Used to inject
    /// timezone-aware time into the receiving agent's system prompt.
    #[serde(default)]
    local_time: Option<String>,
}

#[derive(Deserialize)]
struct CoordinatorEventsQuery {
    session_id: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// SSE endpoint — each authenticated client subscribes here to receive
/// real-time events (human messages and AI responses) for their user account.
async fn sse_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;
    let rx = state.chat_service.subscribe(&user.id);

    let stream = BroadcastStream::new(rx).filter_map(|result| async move {
        let ev = result.ok()?;
        // Coordinator messages are not shown in the user's chat view.
        if matches!(ev, chat_service::UserEvent::CoordinatorResponse { .. }) {
            return None;
        }
        let data = serde_json::to_string(&ev).ok()?;
        Some(Ok::<Event, Infallible>(Event::default().data(data)))
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new().interval(Duration::from_secs(25)),
    ))
}

/// Extract @handles from a message and look each one up in the directory.
/// Returns one entry per unique handle: (handle, matches) where matches is a
/// Vec<(user_id, display_name)>. Empty matches = not found; 2+ = ambiguous.
/// Lookup is case-insensitive: tries handle first, then display_name prefix.
async fn resolve_mentions(
    text: &str,
    db: &sqlx::SqlitePool,
) -> Vec<(String, Vec<(String, String)>)> {
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Only treat as a mention if @ is at the start or preceded by whitespace.
            // This avoids treating email addresses (user@example.com) as mentions.
            let preceded_by_space = i == 0 || bytes[i - 1].is_ascii_whitespace();
            i += 1;
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if !preceded_by_space || i == start {
                continue;
            }
            let handle = text[start..i].to_string();
            if seen.insert(handle.clone()) {
                let prefix = format!("{}%", handle);
                let rows: Vec<(String, String)> = sqlx::query_as(
                    "SELECT user_id, display_name FROM directory \
                     WHERE visible = 1 AND (\
                         LOWER(handle) = LOWER(?) \
                         OR LOWER(display_name) LIKE LOWER(?) \
                     ) \
                     ORDER BY CASE WHEN LOWER(handle) = LOWER(?) THEN 0 ELSE 1 END \
                     LIMIT 3",
                )
                .bind(&handle)
                .bind(&prefix)
                .bind(&handle)
                .fetch_all(db)
                .await
                .unwrap_or_default();
                results.push((handle, rows));
            }
        } else {
            i += 1;
        }
    }
    results
}

/// Format mention resolution results into a system message for the LLM.
/// Returns None if there are no @mentions in the message.
fn build_mention_context(mentions: &[(String, Vec<(String, String)>)]) -> Option<String> {
    if mentions.is_empty() {
        return None;
    }
    let mut resolved = Vec::new();
    let mut ambiguous = Vec::new();
    let mut not_found = Vec::new();

    for (handle, matches) in mentions {
        match matches.len() {
            0 => not_found.push(format!("@{handle}")),
            1 => {
                let (user_id, display_name) = &matches[0];
                resolved.push(format!("  @{handle} → {display_name} (user_id: {user_id})"));
            }
            _ => {
                let options: Vec<String> = matches
                    .iter()
                    .map(|(uid, name)| format!("{name} (user_id: {uid})"))
                    .collect();
                ambiguous.push(format!("  @{handle} matches multiple users: {}", options.join(", ")));
            }
        }
    }

    let mut parts = Vec::new();
    if !resolved.is_empty() {
        parts.push(format!(
            "The following @mentions have been resolved to registered users on this server:\n{}\n\
             If you have all the information needed to fulfill the request, call the `coordinate` \
             tool with the full message. If anything is missing, ask the user first.",
            resolved.join("\n")
        ));
    }
    if !ambiguous.is_empty() {
        parts.push(format!(
            "The following @mentions are ambiguous — ask the user to clarify which person they mean:\n{}",
            ambiguous.join("\n")
        ));
    }
    if !not_found.is_empty() {
        parts.push(format!(
            "The following @mentions could not be resolved to any registered user: {}. \
             Inform the user that these handles were not found.",
            not_found.join(", ")
        ));
    }
    Some(parts.join("\n\n"))
}

/// Submit a message. The human message and AI response are delivered to all
/// connected clients (including the sender) via SSE.
async fn chat_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;

    // Persist the human message synchronously so it's immediately visible,
    // then return 202 right away. LLM work runs in the background to avoid
    // HTTP timeouts on slow responses.
    let now = state
        .chat_service
        .persist_human_message(&user.id, &req.message)
        .await?;

    let mentions = resolve_mentions(&req.message, &state.db).await;
    let mention_context = build_mention_context(&mentions);

    let chat_service = Arc::clone(&state.chat_service);
    let user_id = user.id.clone();
    let message = req.message.clone();
    let local_time = req.local_time.clone();
    tokio::spawn(async move {
        if let Err(e) = chat_service.run_llm(&user_id, &message, local_time.as_deref(), &now, mention_context.as_deref()).await {
            tracing::error!("LLM processing failed for {user_id}: {e:#}");
            chat_service.broadcast_error(&user_id, "AI processing failed. Please try again.");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

/// Voice input: transcribe audio then immediately persist the human message,
/// broadcast it via SSE, and spawn LLM processing — all in one request.
/// The client never needs to make a separate /v1/chat call for voice input.

#[derive(Deserialize)]
struct VoiceQuery {
    local_time: Option<String>,
}

async fn voice_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
    Query(query): Query<VoiceQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;

    let stt = state
        .stt
        .as_ref()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("STT is not configured")))?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/webm");

    let transcript = stt
        .transcribe(body.to_vec(), content_type)
        .await
        .map_err(ApiError::Internal)?;

    let transcript = state.chat_service.convert(transcript.trim());
    if transcript.is_empty() {
        return Ok(StatusCode::NO_CONTENT);
    }

    // Persist human message and broadcast it via SSE immediately.
    let now = state
        .chat_service
        .persist_human_message(&user.id, &transcript)
        .await?;

    let mentions = resolve_mentions(&transcript, &state.db).await;
    let mention_context = build_mention_context(&mentions);

    // Spawn LLM in background — response arrives via SSE.
    let chat_service = Arc::clone(&state.chat_service);
    let user_id = user.id.clone();
    tokio::spawn(async move {
        if let Err(e) = chat_service.run_llm(&user_id, &transcript, query.local_time.as_deref(), &now, mention_context.as_deref()).await {
            tracing::error!("LLM processing failed for {user_id}: {e:#}");
            chat_service.broadcast_error(&user_id, "AI processing failed. Please try again.");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// Coordinator endpoints (server-to-server, Bearer-token auth)
// ---------------------------------------------------------------------------

fn verify_coordinator_auth(
    headers: &HeaderMap,
    secret: &Option<String>,
) -> Result<(), ApiError> {
    let expected = secret
        .as_deref()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("coordinator_secret not configured")))?;
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    if provided != expected {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

/// Receive a coordinator message, run the agent's LLM on it, and broadcast
/// the response. Used by `CoordinatorSession` on remote (or same-host) agents.
async fn coordinator_message_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(req): Json<CoordinatorMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    verify_coordinator_auth(&headers, &state.coordinator_secret)?;

    let now = chrono::Utc::now().to_rfc3339();
    let chat_service = Arc::clone(&state.chat_service);
    let user_id = req.user_id.clone();
    let message = req.content.clone();
    let session_id = req.session_id.clone();
    let is_conclusion = req.is_conclusion;
    let is_agent_coordination = req.session_type == "agent_coordination";
    let initiator_name = req.initiator_name.clone();
    let local_time = req.local_time.clone();

    tokio::spawn(async move {
        if let Err(e) = chat_service
            .run_llm_coordinator(&user_id, &message, &session_id, is_conclusion, is_agent_coordination, &initiator_name, local_time.as_deref(), &now)
            .await
        {
            tracing::error!("coordinator LLM failed for {user_id}: {e:#}");
            // Do not broadcast errors to the user — coordinator failures are
            // handled internally (marked as unavailable and the session continues).
        }
    });

    Ok(StatusCode::ACCEPTED)
}

/// SSE stream for the coordinator — delivers only `CoordinatorResponse` events
/// matching the requested `session_id`. Not exposed to regular users.
async fn coordinator_events_handler(
    axum::extract::Path(user_id): axum::extract::Path<String>,
    Query(params): Query<CoordinatorEventsQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    verify_coordinator_auth(&headers, &state.coordinator_secret)?;

    let session_id = params.session_id.clone();
    let rx = state.chat_service.subscribe(&user_id);

    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        let sid = session_id.clone();
        async move {
            let ev = result.ok()?;
            match &ev {
                chat_service::UserEvent::CoordinatorResponse { session_id: ev_sid, .. }
                    if ev_sid == &sid =>
                {
                    let data = serde_json::to_string(&ev).ok()?;
                    Some(Ok::<Event, Infallible>(Event::default().data(data)))
                }
                _ => None,
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(25))))
}

async fn store_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
    Json(req): Json<StoreRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;
    let user_mem = state.chat_service.pool().get(&user.id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    user_mem.store(&req.category, &req.role, &req.content, &now, req.importance, None, "human").await?;

    Ok(Json(MessageResponse {
        message: "stored".to_string(),
    }))
}

async fn search_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;
    let user_mem = state.chat_service.pool().get(&user.id).await?;

    let cat_refs: Option<Vec<&str>> = req
        .categories
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let entries = user_mem.retrieve(&req.query, cat_refs.as_deref()).await?;

    let entries: Vec<SearchEntry> = entries
        .into_iter()
        .map(|e| SearchEntry {
            category: e.category,
            role: e.role,
            content: e.content,
            timestamp: e.timestamp,
        })
        .collect();

    Ok(Json(SearchResponse { entries }))
}

async fn history_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<HistoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&mut auth_session, &jar).await?;
    let user_mem = state.chat_service.pool().get(&user.id).await?;

    // First-time user: store their name as knowledge and add a welcome message.
    if user_mem.history.is_empty().await? {
        let profile = user_mem.history.get_profile().await?;
        let (display_name, first_name) = profile
            .map(|p| (p.name, p.first_name))
            .unwrap_or_else(|| (user.email.clone(), user.email.clone()));

        let name_fact = format!("The user's name is {}.", display_name);
        let now = chrono::Utc::now().to_rfc3339();
        user_mem.store("knowledge", "system", &name_fact, &now, 10.0, None, "human").await?;

        let welcome = format!(
            "Welcome to Sidekick, {}! I'm your AI assistant with long-term memory. How can I help you today?",
            first_name
        );
        // Welcome message goes to history only — not worth embedding.
        user_mem.history.append("conversation", "ai", &welcome, &now, 1.0, None, "human").await?;
        user_mem
            .semantic
            .chat_memory
            .append(&user.id, Message::ai(&welcome))
            .await
            .context("failed to append welcome message")?;
    }

    let limit = params.limit.unwrap_or(20).min(100);
    let category = params.category.as_deref();
    let entries = if let Some(after) = params.after {
        user_mem.history.fetch_after(after, limit, category, true).await?
    } else {
        user_mem.history.fetch(params.before, limit, category, true).await?
    };
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Tool filtering
// ---------------------------------------------------------------------------

/// Filter a tool list by an optional allowlist.
/// Each allowlist entry is matched as a **substring** of the tool name, so
/// `"gmail"` matches `list_gmail_messages`, `send_gmail_message`, etc.
/// When `allowlist` is `None` all tools are returned unchanged.
fn filter_tools(
    tools: &[Arc<dyn synaptic::core::Tool>],
    allowlist: &[String],
) -> Vec<Arc<dyn synaptic::core::Tool>> {
    if allowlist.is_empty() {
        return tools.to_vec();
    }
    tools
        .iter()
        .filter(|t| {
            let name = t.name();
            allowlist.iter().any(|entry| name.contains(entry.as_str()))
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Err(e) = dotenv() {
        tracing::debug!("dotenv: {e}");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sidekick_server=info,tower_http=info".into()),
        )
        .init();

    let config_path = std::env::var("SIDEKICK_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("config.toml")));
            match exe_dir {
                Some(p) if p.exists() => p,
                _ => std::path::PathBuf::from("config.toml"),
            }
        });
    let cfg = config::load(&config_path)?;

    // --- SQLite for users ---
    std::fs::create_dir_all(
        Path::new(&cfg.auth.db_path)
            .parent()
            .unwrap_or(Path::new(".")),
    )?;
    let db = SqlitePoolOptions::new()
        .connect(&format!("sqlite:{}?mode=rwc", cfg.auth.db_path))
        .await
        .context("failed to connect to SQLite")?;
    user::init_db(&db).await?;

    // --- Auth backend ---
    let auth_backend = AuthBackend::new(
        db.clone(),
        &cfg.server.base_url,
        cfg.server.frontend_url(),
        &cfg.auth.providers,
    )?;

    // --- Cookie encryption key for "remember me" ---
    let cookie_key = match std::env::var("SESSION_SECRET") {
        Ok(secret) => {
            let mut key_bytes = [0u8; 64];
            let src = secret.as_bytes();
            for (i, b) in key_bytes.iter_mut().enumerate() {
                *b = src[i % src.len()];
            }
            Key::from(&key_bytes)
        }
        Err(_) => {
            tracing::warn!("SESSION_SECRET not set — generating a random key (sessions won't survive restarts)");
            Key::generate()
        }
    };

    // --- Sessions ---
    let session_store = SessionMemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_same_site(SameSite::Lax);

    let auth_layer = AuthManagerLayerBuilder::new(auth_backend, session_layer).build();

    // --- Embeddings & LLM ---
    let emb = embeddings::build_embeddings(&cfg.embeddings)?;
    let reranker = rerank::build_reranker(&cfg.rerank);

    // Handle --import <user_id> <file> mode.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 4 && args[1] == "--import" {
        let user_id = &args[2];
        let file_path = Path::new(&args[3]);
        let pool = memory::UserStorePool::new(
            &cfg.memory,
            emb,
            reranker,
            cfg.embeddings.dimensions,
            cfg.rerank.top_n,
            cfg.rerank.category_weights,
        )?;
        let user_mem = pool.get(user_id).await?;
        println!(
            "Importing from {} for user '{user_id}'...",
            file_path.display()
        );
        let count = user_mem.import_jsonl(file_path).await?;
        println!("Imported {count} entries.");
        return Ok(());
    }

    let model = provider::build_model(&cfg.llm)?;

    let pool = Arc::new(memory::UserStorePool::new(
        &cfg.memory,
        emb,
        reranker,
        cfg.embeddings.dimensions,
        cfg.rerank.top_n,
        cfg.rerank.category_weights,
    )?);

    // --- Tools ---
    let max_tool_retries = cfg.agent.max_tool_retries;
    let mut all_tools: Vec<Arc<dyn synaptic::core::Tool>> = vec![
        tools::recall_memory::RecallMemory::new(pool.clone()),
        Arc::new(tools::web_search::WebSearch::new()),
        tools::find_agents::FindAgents::new(Arc::new(db.clone())),
        tools::start_coordination::Coordinate::new(
            Arc::new(db.clone()),
            pool.clone(),
            Arc::new(cfg.llm.clone()),
            cfg.agent.coordinator_secret.clone(),
            cfg.server.base_url.clone(),
        ),
    ];
    if let Some(google_config) = cfg.auth.providers.get("google") {
        let api = tools::google_api::GoogleApiClient::new(pool.clone(), google_config)?;
        let mut t = tools::google_calendar::create_tools(api.clone());
        t.extend(tools::gmail::create_tools(api.clone()));
        t.extend(tools::google_tasks::create_tools(api.clone()));
        t.extend(tools::google_people::create_tools(api));
        all_tools.extend(t);
    }
    let all_tools: Vec<Arc<dyn synaptic::core::Tool>> = all_tools
        .into_iter()
        .map(|tool| tools::retry_wrapper::RetryAwareTool::wrap(tool, max_tool_retries))
        .collect();

    let chat_graph = create_react_agent(
        model.clone(),
        filter_tools(&all_tools, &cfg.agent.chat_tools),
    ).context("failed to create chat agent")?;

    let coordinator_graph = create_react_agent(
        model.clone(),
        filter_tools(&all_tools, &cfg.agent.coordinator_tools),
    ).context("failed to create coordinator agent")?;

    let agent_graph = create_react_agent(
        model,
        filter_tools(&all_tools, &cfg.agent.agent_tools),
    ).context("failed to create agent graph")?;

    let chat_service = Arc::new(ChatService::new(
        pool,
        chat_graph,
        coordinator_graph,
        agent_graph,
        cfg.agent.system_prompt,
    ));

    // Background cleanup task: evict broadcast channels with no active subscribers.
    {
        let svc = Arc::clone(&chat_service);
        let interval_secs = cfg.server.cleanup_interval_minutes * 60;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            ticker.tick().await; // skip immediate first tick
            loop {
                ticker.tick().await;
                svc.cleanup_channels();
                tracing::debug!("cleaned up inactive broadcast channels");
            }
        });
    }

    let stt = cfg.stt.as_ref().and_then(|s| {
        match s.api_key() {
            Ok(key) => Some(Arc::new(SttClient::new(key, s.model.clone(), s.endpoint()))),
            Err(e) => {
                tracing::warn!("STT disabled: {e}");
                None
            }
        }
    });

    let state = Arc::new(AppState {
        chat_service,
        db: Arc::new(db),
        cookie_key: CookieKey(cookie_key),
        stt,
        coordinator_secret: cfg.agent.coordinator_secret.clone(),
    });

    // --- Routes ---
    let api_routes = Router::new()
        .route("/v1/events", get(sse_handler))
        .route("/v1/chat", post(chat_handler))
        .route("/v1/voice", post(voice_handler))
        .route("/v1/memory/store", post(store_handler))
        .route("/v1/memory/search", post(search_handler))
        .route("/v1/history", get(history_handler))
        .route("/v1/coordinator/message", post(coordinator_message_handler))
        .route("/v1/coordinator/events/{user_id}", get(coordinator_events_handler))
        .with_state(state.clone());

    let auth_routes = Router::new()
        .route("/auth/me", get(auth::routes::me))
        .route("/auth/logout", post(auth::routes::logout))
        .route("/auth/{provider}", get(auth::routes::login))
        .route("/auth/{provider}/callback", get(auth::routes::callback))
        .with_state(state);

    let app = Router::new()
        .merge(api_routes)
        .merge(auth_routes)
        .fallback(get(static_handler))
        .layer(auth_layer)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let bind = format!("{}:{}", cfg.server.host, cfg.server.port);
    tracing::info!("Sidekick listening on {bind}");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
