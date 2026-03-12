use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Application-level error returned by API handlers.
///
/// Wraps `anyhow::Error` and implements `IntoResponse` so handlers can
/// simply use `?` on any fallible operation.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// An internal/unexpected error — mapped to 500.
    #[error("{0:#}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self, "request failed");
        let (status, message) = match &self {
            ApiError::Internal(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")),
        };
        let body = Json(serde_json::json!({ "error": message }));
        (status, body).into_response()
    }
}
