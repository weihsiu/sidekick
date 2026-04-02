use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};

tokio::task_local! {
    /// The current user's ID, set per-request before invoking the agent.
    /// Tools read this to look up per-user credentials.
    pub static CURRENT_USER_ID: String;

    /// Per-invocation tool failure counter. Scoped fresh around every
    /// `graph.invoke()` call so concurrent users cannot interfere.
    pub static TOOL_FAILURE_COUNT: AtomicUsize;

    /// Set to true by the `coordinate` tool when it spawns a background session.
    /// `run_llm` checks this after invoke to suppress its own response — the
    /// coordinator will deliver the final answer instead.
    pub static COORDINATION_SPAWNED: Arc<AtomicBool>;
}
