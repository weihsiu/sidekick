use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub llm: LlmConfig,
    pub embeddings: EmbeddingsConfig,
    pub memory: MemoryConfig,
    pub agent: AgentConfig,
    pub rerank: RerankConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingsConfig {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub base_url: Option<String>,
    pub dimensions: usize,
}

#[derive(Debug, Deserialize)]
pub struct MemoryConfig {
    /// Base directory for per-user LanceDB databases.
    /// Each user gets `{base_path}/{user_id}.lancedb`.
    pub base_path: String,
    pub table_name: String,
    /// Number of relevant past entries to retrieve per query.
    pub top_k: usize,
    /// Max number of user DBs to keep open in the LRU pool.
    pub pool_size: usize,
    /// Number of recent chat messages to keep in the conversation window.
    /// These are passed to the LLM as message history for short-term context.
    pub chat_window: usize,
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct RerankConfig {
    pub provider: String,
    pub top_n: usize,
    #[serde(default)]
    pub category_weights: HashMap<String, f32>,
}

impl LlmConfig {
    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.api_key_env).with_context(|| {
            format!(
                "environment variable '{}' must be set for provider '{}'",
                self.api_key_env, self.provider
            )
        })
    }
}

impl EmbeddingsConfig {
    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.api_key_env).with_context(|| {
            format!(
                "environment variable '{}' must be set for embeddings provider '{}'",
                self.api_key_env, self.provider
            )
        })
    }
}

pub fn load(path: &Path) -> Result<AppConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse config file {}", path.display()))
}
