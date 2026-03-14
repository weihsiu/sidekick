use std::sync::Arc;

use crate::config::RerankConfig;

/// A scored document returned by a reranker.
pub struct RankedDocument {
    /// Index into the original input slice.
    pub index: usize,
    /// Relevance score (higher is better).
    pub score: f32,
}

/// Trait for reranking a set of documents against a query.
#[async_trait::async_trait]
pub trait Reranker: Send + Sync {
    /// Score each document against the query and return them ranked by relevance.
    async fn rerank(
        &self,
        query: &str,
        documents: &[&str],
        top_n: usize,
    ) -> anyhow::Result<Vec<RankedDocument>>;
}

/// A pass-through reranker that returns documents in their original order.
pub struct MockReranker;

#[async_trait::async_trait]
impl Reranker for MockReranker {
    async fn rerank(
        &self,
        _query: &str,
        documents: &[&str],
        top_n: usize,
    ) -> anyhow::Result<Vec<RankedDocument>> {
        let n = top_n.min(documents.len());
        let ranked = (0..n)
            .map(|i| RankedDocument {
                index: i,
                score: 1.0 - (i as f32 / documents.len() as f32),
            })
            .collect();
        Ok(ranked)
    }
}

pub fn build_reranker(config: &RerankConfig) -> Arc<dyn Reranker> {
    match config.provider.as_str() {
        // Future: "cohere" => { ... }
        // Future: "jina" => { ... }
        _ => Arc::new(MockReranker),
    }
}
