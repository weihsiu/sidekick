use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub llm: LlmConfig,
    pub embeddings: EmbeddingsConfig,
    pub memory: MemoryConfig,
    pub agent: AgentConfig,
    pub rerank: RerankConfig,
    pub stt: Option<SttConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// URL the browser sees (for OAuth redirect URIs).
    pub base_url: String,
    /// URL to redirect to after login. Defaults to base_url.
    /// Only set this when the frontend is served separately (e.g. Vite dev server).
    pub frontend_url: Option<String>,
    /// How often (in minutes) to run background cleanup tasks. Defaults to 5.
    #[serde(default = "default_cleanup_interval_minutes")]
    pub cleanup_interval_minutes: u64,
}

fn default_cleanup_interval_minutes() -> u64 {
    5
}

impl ServerConfig {
    pub fn frontend_url(&self) -> &str {
        self.frontend_url.as_deref().unwrap_or(&self.base_url)
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthConfig {
    /// Path to the SQLite database for users and sessions.
    pub db_path: String,
    /// OAuth provider configurations, keyed by provider name.
    pub providers: HashMap<String, OAuthProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthProviderConfig {
    pub client_id: String,
    pub client_secret_env: String,
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    /// OAuth scopes to request.
    pub scopes: Vec<String>,
    /// Extra query parameters to add to the authorization URL.
    /// e.g. `{ access_type = "offline", prompt = "consent" }` for Google.
    #[serde(default)]
    pub extra_auth_params: HashMap<String, String>,
}

impl OAuthProviderConfig {
    pub fn client_secret(&self) -> Result<String> {
        std::env::var(&self.client_secret_env).with_context(|| {
            format!(
                "environment variable '{}' must be set for OAuth provider",
                self.client_secret_env
            )
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
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
    /// Number of recent chat messages to keep in the chat window.
    /// These are passed to the LLM as message history for short-term context.
    pub chat_window: usize,
}

#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: String,
    /// Maximum number of times a tool call can fail before the agent gives up
    /// for that conversation turn. Defaults to 3.
    #[serde(default = "default_max_tool_retries")]
    pub max_tool_retries: usize,
    /// Shared secret for server-to-server coordinator authentication.
    /// Set the same value on all Sidekick instances that should be able to
    /// coordinate with each other. Required to use multi-agent coordination.
    #[serde(default)]
    pub coordinator_secret: Option<String>,
    /// Tools available to the main chat LLM. Each entry is matched as a
    /// substring of the tool name — e.g. "gmail" enables all Gmail tools.
    /// Empty list means all tools are enabled.
    #[serde(default)]
    pub chat_tools: Vec<String>,
    /// Tools available to the LLM when processing incoming coordinator messages.
    /// Same substring matching as chat_tools. Empty list means all tools are enabled.
    #[serde(default)]
    pub coordinator_tools: Vec<String>,
    /// Tools available to this agent when being queried by a coordinator.
    /// Same substring matching as chat_tools. Empty list means all tools are enabled.
    #[serde(default)]
    pub agent_tools: Vec<String>,
}

fn default_max_tool_retries() -> usize {
    3
}

#[derive(Debug, Deserialize)]
pub struct SttConfig {
    pub api_key_env: String,
    #[serde(default = "default_stt_model")]
    pub model: String,
    pub base_url: Option<String>,
}

fn default_stt_model() -> String {
    "whisper-large-v3-turbo".to_string()
}

impl SttConfig {
    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.api_key_env).with_context(|| {
            format!("environment variable '{}' must be set for STT", self.api_key_env)
        })
    }

    pub fn endpoint(&self) -> String {
        let base = self.base_url.as_deref().unwrap_or("https://api.groq.com/openai/v1");
        format!("{base}/audio/transcriptions")
    }
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
    let mut cfg: AppConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;

    // Allow environment variables to override server URLs so the same
    // config.toml works for both local dev and production deployment.
    if let Ok(val) = std::env::var("BASE_URL") {
        cfg.server.base_url = val;
    }
    if let Ok(val) = std::env::var("FRONTEND_URL") {
        cfg.server.frontend_url = Some(val);
    }
    if let Ok(val) = std::env::var("COORDINATOR_SECRET") {
        cfg.agent.coordinator_secret = Some(val);
    }

    Ok(cfg)
}
