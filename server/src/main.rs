mod auth;
mod config;
mod embeddings;
mod error;
mod history;
mod memory;
mod provider;
mod rerank;
mod user;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_login::login_required;
use axum_login::{AuthManagerLayerBuilder, AuthSession};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use synaptic::core::{MemoryStore, Message};
use synaptic::graph::{create_react_agent, CompiledGraph, MessageState};
use axum::http::{header, StatusCode, Uri};
use rust_embed::RustEmbed;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use axum_extra::extract::cookie::Key;
use tower_sessions::cookie::SameSite;
use tower_sessions::{MemoryStore as SessionMemoryStore, SessionManagerLayer};

use auth::AuthBackend;
use error::ApiError;
use memory::{format_context, MemoryPool};
use dotenvy::dotenv;

#[derive(RustEmbed)]
#[folder = "../client/dist/"]
struct Assets;

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (StatusCode::OK, [(header::CONTENT_TYPE, mime.as_ref().to_string())], content.data.to_vec()).into_response()
        }
        None => match Assets::get("index.html") {
            Some(content) => {
                (StatusCode::OK, [(header::CONTENT_TYPE, "text/html".to_string())], content.data.to_vec()).into_response()
            }
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
    pool: MemoryPool,
    graph: CompiledGraph<MessageState>,
    system_prompt: String,
    cookie_key: CookieKey,
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
}

#[derive(Serialize)]
struct ChatResponse {
    response: String,
}

#[derive(Deserialize)]
struct StoreRequest {
    category: String,
    role: String,
    content: String,
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
    limit: Option<i64>,
    category: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers (user_id comes from the authenticated session)
// ---------------------------------------------------------------------------

async fn chat_handler(
    auth_session: AuthSession<AuthBackend>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = auth_session.user.ok_or(ApiError::Unauthorized)?;
    let user_id = &user.id;

    let user_mem = state.pool.get(user_id).await?;

    // Retrieve relevant context from this user's memory.
    let context_entries = user_mem.semantic.retrieve(&req.message, None).await?;
    let context = format_context(&context_entries);

    let system_prompt = if context.is_empty() {
        state.system_prompt.clone()
    } else {
        format!("{}\n\n{}", state.system_prompt, context)
    };

    // Build message list: system prompt + recent chat window + current message.
    let mut messages = vec![Message::system(&system_prompt)];
    let history = user_mem
        .semantic
        .chat_memory
        .load(user_id)
        .await
        .context("failed to load chat history")?;
    messages.extend(history);
    messages.push(Message::human(&req.message));

    let msg_state = MessageState { messages };

    let result = state
        .graph
        .invoke(msg_state)
        .await
        .context("LLM invocation failed")?;
    let final_state: &MessageState = result.state();

    let response = final_state
        .last_message()
        .map(|m: &Message| m.content().to_string())
        .unwrap_or_default();

    // Persist both sides to long-term memory (LanceDB), history (SQLite),
    // and the chat window.
    let now = chrono::Utc::now().to_rfc3339();
    user_mem.semantic.store("conversation", "human", &req.message).await?;
    user_mem.semantic.store("conversation", "ai", &response).await?;
    user_mem.history.append("conversation", "human", &req.message, &now).await?;
    user_mem.history.append("conversation", "ai", &response, &now).await?;
    user_mem.semantic.chat_memory
        .append(user_id, Message::human(&req.message))
        .await
        .context("failed to append to chat window")?;
    user_mem.semantic.chat_memory
        .append(user_id, Message::ai(&response))
        .await
        .context("failed to append to chat window")?;

    Ok(Json(ChatResponse { response }))
}

async fn store_handler(
    auth_session: AuthSession<AuthBackend>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<StoreRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = auth_session.user.ok_or(ApiError::Unauthorized)?;
    let user_mem = state.pool.get(&user.id).await?;
    user_mem.semantic.store(&req.category, &req.role, &req.content).await?;
    let now = chrono::Utc::now().to_rfc3339();
    user_mem.history.append(&req.category, &req.role, &req.content, &now).await?;

    Ok(Json(MessageResponse {
        message: "stored".to_string(),
    }))
}

async fn search_handler(
    auth_session: AuthSession<AuthBackend>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user = auth_session.user.ok_or(ApiError::Unauthorized)?;
    let user_mem = state.pool.get(&user.id).await?;

    let cat_refs: Option<Vec<&str>> = req
        .categories
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let entries = user_mem.semantic.retrieve(&req.query, cat_refs.as_deref()).await?;

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
    auth_session: AuthSession<AuthBackend>,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<HistoryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user = auth_session.user.ok_or(ApiError::Unauthorized)?;
    let user_mem = state.pool.get(&user.id).await?;
    let limit = params.limit.unwrap_or(20).min(100);
    let category = params.category.as_deref();
    let entries = user_mem.history.fetch(params.before, limit, category).await?;
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
        let db_path = format!("{}/{}.lancedb", cfg.memory.base_path, user_id);
        let mem = memory::SemanticMemory::new(
            &db_path,
            &cfg.memory.table_name,
            emb,
            reranker,
            cfg.embeddings.dimensions,
            cfg.memory.top_k,
            cfg.rerank.top_n,
            cfg.rerank.category_weights,
            cfg.memory.chat_window,
        )
        .await?;
        let hist_db_path = format!("{}/{}.memory.db", cfg.memory.base_path, user_id);
        let hist = history::MemoryHistory::new(&hist_db_path).await?;
        println!(
            "Importing from {} for user '{user_id}'...",
            file_path.display()
        );
        let count = mem.import_jsonl(file_path, &hist).await?;
        println!("Imported {count} entries.");
        return Ok(());
    }

    let model = provider::build_model(&cfg.llm)?;
    let graph = create_react_agent(model, vec![]).context("failed to create agent")?;

    let pool = memory::MemoryPool::new(
        &cfg.memory,
        emb,
        reranker,
        cfg.embeddings.dimensions,
        cfg.rerank.top_n,
        cfg.rerank.category_weights,
    )?;

    let state = Arc::new(AppState {
        pool,
        graph,
        system_prompt: cfg.agent.system_prompt,
        cookie_key: CookieKey(cookie_key),
    });

    // --- Routes ---
    // Protected API routes (require login).
    let api_routes = Router::new()
        .route("/v1/chat", post(chat_handler))
        .route("/v1/memory/store", post(store_handler))
        .route("/v1/memory/search", post(search_handler))
        .route("/v1/history", get(history_handler))
        .route_layer(login_required!(AuthBackend))
        .with_state(state.clone());

    // Auth routes (public).
    // NOTE: exact routes must be registered before the wildcard {provider}
    // so that /auth/me and /auth/logout don't get captured by {provider}.
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
