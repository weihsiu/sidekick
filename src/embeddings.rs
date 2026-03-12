use std::sync::Arc;

use anyhow::{bail, Result};
use synaptic::core::Embeddings;
use synaptic::models::HttpBackend;

use crate::config::EmbeddingsConfig;

pub fn build_embeddings(config: &EmbeddingsConfig) -> Result<Arc<dyn Embeddings>> {
    let backend = Arc::new(HttpBackend::new());

    match config.provider.as_str() {
        "openai" => {
            use synaptic::openai::{OpenAiEmbeddings, OpenAiEmbeddingsConfig};

            let mut cfg = OpenAiEmbeddingsConfig::new(config.api_key()?)
                .with_model(&config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(OpenAiEmbeddings::new(cfg, backend)))
        }

        "ollama" => {
            use synaptic::ollama::{OllamaEmbeddings, OllamaEmbeddingsConfig};

            let mut cfg = OllamaEmbeddingsConfig::new(&config.model);
            if let Some(url) = &config.base_url {
                cfg = cfg.with_base_url(url);
            }
            Ok(Arc::new(OllamaEmbeddings::new(cfg, backend)))
        }

        other => bail!("unsupported embeddings provider: '{other}'. Supported: openai, ollama"),
    }
}
