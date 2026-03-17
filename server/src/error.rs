use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Application-level error returned by API handlers.
///
/// Wraps `anyhow::Error` and implements `IntoResponse` so handlers can
/// simply use `?` on any fallible operation.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Not authenticated — mapped to 401.
    #[error("not authenticated")]
    Unauthorized,

    /// An internal/unexpected error — mapped to 500.
    #[error("{0:#}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "not authenticated".to_string()),
            ApiError::Internal(err) => {
                tracing::error!(error = %err, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}"))
            }
        };
        let body = Json(serde_json::json!({ "error": message }));
        (status, body).into_response()
    }
}
