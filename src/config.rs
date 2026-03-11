use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub llm: LlmConfig,
    pub embeddings: EmbeddingsConfig,
    pub memory: MemoryConfig,
    pub agent: AgentConfig,
    pub user: UserConfig,
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
    pub db_path: String,
    pub table_name: String,
    pub top_k: usize,
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct UserConfig {
    pub id: String,
}

impl LlmConfig {
    pub fn api_key(&self) -> String {
        std::env::var(&self.api_key_env).unwrap_or_else(|_| {
            panic!(
                "environment variable '{}' must be set for provider '{}'",
                self.api_key_env, self.provider
            )
        })
    }
}

impl EmbeddingsConfig {
    pub fn api_key(&self) -> String {
        std::env::var(&self.api_key_env).unwrap_or_else(|_| {
            panic!(
                "environment variable '{}' must be set for embeddings provider '{}'",
                self.api_key_env, self.provider
            )
        })
    }
}

pub fn load(path: &Path) -> AppConfig {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read config file {}: {e}", path.display()));
    toml::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse config file {}: {e}", path.display()))
}
