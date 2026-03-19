tokio::task_local! {
    /// The current user's ID, set per-request before invoking the agent.
    /// Tools read this to look up per-user credentials.
    pub static CURRENT_USER_ID: String;
}
