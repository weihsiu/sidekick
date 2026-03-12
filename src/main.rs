mod config;
mod embeddings;
mod error;
mod memory;
mod provider;
mod rerank;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use synaptic::core::{MemoryStore, Message};
use synaptic::graph::{create_react_agent, CompiledGraph, MessageState};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use error::ApiError;
use memory::{format_context, MemoryPool};

/// Shared application state.
struct AppState {
    pool: MemoryPool,
    graph: CompiledGraph<MessageState>,
    system_prompt: String,
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    user_id: String,
    message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    response: String,
}

#[derive(Deserialize)]
struct StoreRequest {
    user_id: String,
    category: String,
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct SearchRequest {
    user_id: String,
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let mem = state.pool.get(&req.user_id).await?;

    // Retrieve relevant context from this user's memory.
    let context_entries = mem.retrieve(&req.message, None).await?;
    let context = format_context(&context_entries);

    let system_prompt = if context.is_empty() {
        state.system_prompt.clone()
    } else {
        format!("{}\n\n{}", state.system_prompt, context)
    };

    // Build message list: system prompt + recent chat window + current message.
    let mut messages = vec![Message::system(&system_prompt)];
    let history = mem
        .chat_memory
        .load(&req.user_id)
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

    // Persist both sides to long-term memory (LanceDB) and the chat window.
    mem.store("conversation", "human", &req.message).await?;
    mem.store("conversation", "ai", &response).await?;
    mem.chat_memory
        .append(&req.user_id, Message::human(&req.message))
        .await
        .context("failed to append to chat window")?;
    mem.chat_memory
        .append(&req.user_id, Message::ai(&response))
        .await
        .context("failed to append to chat window")?;

    Ok(Json(ChatResponse { response }))
}

async fn store_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StoreRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let mem = state.pool.get(&req.user_id).await?;
    mem.store(&req.category, &req.role, &req.content).await?;

    Ok(Json(MessageResponse {
        message: "stored".to_string(),
    }))
}

async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let mem = state.pool.get(&req.user_id).await?;

    let cat_refs: Option<Vec<&str>> = req
        .categories
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let entries = mem
        .retrieve(&req.query, cat_refs.as_deref())
        .await?;

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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sidekick=info,tower_http=info".into()),
        )
        .init();

    let cfg = config::load(Path::new("config.toml"))?;

    let emb = embeddings::build_embeddings(&cfg.embeddings)?;
    let reranker = rerank::build_reranker(&cfg.rerank);

    // Handle --import <user_id> <file> mode.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 4 && args[1] == "--import" {
        let user_id = &args[2];
        let file_path = Path::new(&args[3]);
        let db_path = format!("{}/{}.lancedb", cfg.memory.base_path, user_id);
        let mem = memory::ConversationMemory::new(
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
        println!(
            "Importing from {} for user '{user_id}'...",
            file_path.display()
        );
        let count = mem.import_jsonl(file_path).await?;
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
    });

    let app = Router::new()
        .route("/v1/chat", post(chat_handler))
        .route("/v1/memory/store", post(store_handler))
        .route("/v1/memory/search", post(search_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let bind = format!("{}:{}", cfg.server.host, cfg.server.port);
    tracing::info!("Sidekick listening on {bind}");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
