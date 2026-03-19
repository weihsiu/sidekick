use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use synaptic::core::{SynapticError, Tool};

/// A tool wrapper that catches errors, returns them to the LLM as results,
/// and tracks the total number of failures per conversation turn.
///
/// After `max_retries` total tool failures (across all tools), the error
/// message includes an instruction to stop retrying and explain the problem.
///
/// The counter is shared across all tools for a given conversation turn and
/// must be reset before each `graph.invoke()`.
pub struct RetryAwareTool {
    inner: Arc<dyn Tool>,
    failure_count: Arc<AtomicUsize>,
    max_retries: usize,
}

impl RetryAwareTool {
    pub fn wrap(
        inner: Arc<dyn Tool>,
        failure_count: Arc<AtomicUsize>,
        max_retries: usize,
    ) -> Arc<dyn Tool> {
        Arc::new(Self {
            inner,
            failure_count,
            max_retries,
        })
    }
}

#[async_trait]
impl Tool for RetryAwareTool {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn description(&self) -> &'static str {
        self.inner.description()
    }

    fn parameters(&self) -> Option<Value> {
        self.inner.parameters()
    }

    async fn call(&self, args: Value) -> Result<Value, SynapticError> {
        tracing::debug!(tool = self.name(), args = %args, "tool call started");
        match self.inner.call(args).await {
            Ok(value) => {
                tracing::debug!(tool = self.name(), result = %value, "tool call succeeded");
                Ok(value)
            }
            Err(err) => {
                let count = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;
                let error_msg = err.to_string();
                tracing::warn!(
                    tool = self.name(),
                    error = %error_msg,
                    failure_count = count,
                    max_retries = self.max_retries,
                    "tool call failed"
                );

                if count >= self.max_retries {
                    Ok(serde_json::json!(format!(
                        "Error: {}. You have exhausted all {} retry attempts. \
                         Do NOT retry this tool call. Instead, explain the error to the user \
                         and suggest how they can resolve it.",
                        error_msg, self.max_retries
                    )))
                } else {
                    Ok(serde_json::json!(format!(
                        "Error: {}. You may retry with different parameters \
                         ({} of {} attempts used).",
                        error_msg, count, self.max_retries
                    )))
                }
            }
        }
    }
}
