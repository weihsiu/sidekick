mod auth;
mod chat_service;
mod config;
mod context;
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
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use anyhow::Context;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
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
    cookie_key: CookieKey,
    stt: Option<Arc<SttClient>>,
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
        let data = serde_json::to_string(&ev).ok()?;
        Some(Ok::<Event, Infallible>(Event::default().data(data)))
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new().interval(Duration::from_secs(25)),
    ))
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

    let chat_service = Arc::clone(&state.chat_service);
    let user_id = user.id.clone();
    let message = req.message.clone();
    let local_time = req.local_time.clone();
    tokio::spawn(async move {
        if let Err(e) = chat_service.run_llm(&user_id, &message, local_time.as_deref(), &now).await {
            tracing::error!("LLM processing failed for {user_id}: {e:#}");
            chat_service.broadcast_error(&user_id, "AI processing failed. Please try again.");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

/// Voice input: transcribe audio then immediately persist the human message,
/// broadcast it via SSE, and spawn LLM processing — all in one request.
/// The client never needs to make a separate /v1/chat call for voice input.
async fn voice_handler(
    mut auth_session: AuthSession<AuthBackend>,
    jar: Jar,
    State(state): State<Arc<AppState>>,
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

    // Spawn LLM in background — response arrives via SSE.
    let chat_service = Arc::clone(&state.chat_service);
    let user_id = user.id.clone();
    tokio::spawn(async move {
        if let Err(e) = chat_service.run_llm(&user_id, &transcript, None, &now).await {
            tracing::error!("LLM processing failed for {user_id}: {e:#}");
            chat_service.broadcast_error(&user_id, "AI processing failed. Please try again.");
        }
    });

    Ok(StatusCode::ACCEPTED)
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
    user_mem.store(&req.category, &req.role, &req.content, &now, req.importance).await?;

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
        user_mem.store("knowledge", "system", &name_fact, &now, 10.0).await?;

        let welcome = format!(
            "Welcome to Sidekick, {}! I'm your AI assistant with long-term memory. How can I help you today?",
            first_name
        );
        // Welcome message goes to history only — not worth embedding.
        user_mem.history.append("conversation", "ai", &welcome, &now, 1.0).await?;
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
        user_mem.history.fetch_after(after, limit, category).await?
    } else {
        user_mem.history.fetch(params.before, limit, category).await?
    };
    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

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
    let tool_failure_count = Arc::new(AtomicUsize::new(0));
    let max_tool_retries = cfg.agent.max_tool_retries;
    let mut all_tools: Vec<Arc<dyn synaptic::core::Tool>> = vec![
        tools::recall_memory::RecallMemory::new(pool.clone()),
        Arc::new(tools::web_search::WebSearch::new()),
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
        .map(|tool| {
            tools::retry_wrapper::RetryAwareTool::wrap(
                tool,
                tool_failure_count.clone(),
                max_tool_retries,
            )
        })
        .collect();

    let graph = create_react_agent(model, all_tools).context("failed to create agent")?;

    let chat_service = Arc::new(ChatService::new(
        pool,
        graph,
        cfg.agent.system_prompt,
        tool_failure_count,
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
        cookie_key: CookieKey(cookie_key),
        stt,
    });

    // --- Routes ---
    let api_routes = Router::new()
        .route("/v1/events", get(sse_handler))
        .route("/v1/chat", post(chat_handler))
        .route("/v1/voice", post(voice_handler))
        .route("/v1/memory/store", post(store_handler))
        .route("/v1/memory/search", post(search_handler))
        .route("/v1/history", get(history_handler))
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
