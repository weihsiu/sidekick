use std::sync::Arc;

use anyhow::{bail, Result};
use synaptic::anthropic::{AnthropicChatModel, AnthropicConfig};
use synaptic::core::ChatModel;
use synaptic::gemini::{GeminiChatModel, GeminiConfig};
use synaptic::models::HttpBackend;
use synaptic::ollama::{OllamaChatModel, OllamaConfig};
use synaptic::openai::{OpenAiChatModel, OpenAiConfig};

use crate::config::LlmConfig;

pub fn build_model(config: &LlmConfig) -> Result<Arc<dyn ChatModel>> {
    let backend = Arc::new(HttpBackend::new());

    match config.provider.as_str() {
        "openai" => {
            let mut cfg = OpenAiConfig::new(config.api_key()?, &config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(OpenAiChatModel::new(cfg, backend)))
        }

        "anthropic" => {
            let mut cfg = AnthropicConfig::new(config.api_key()?, &config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(AnthropicChatModel::new(cfg, backend)))
        }

        "gemini" => {
            let mut cfg = GeminiConfig::new(config.api_key()?, &config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(GeminiChatModel::new(cfg, backend)))
        }

        "ollama" => {
            let mut cfg = OllamaConfig::new(&config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(OllamaChatModel::new(cfg, backend)))
        }

        other => bail!("unsupported provider: '{other}'. Supported: openai, anthropic, gemini, ollama"),
    }
}
