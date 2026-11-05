//! The JSON error shape of `/api/*` and `/webhooks/*`:
//! `{"error": "<code>", "detail": "<human text>"}` (server-api.md, Conventions).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use guard_core::ports::StoreError;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub detail: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", detail)
    }

    pub fn payload_too_large(limit: usize) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("request body is larger than {limit} bytes"),
        )
    }

    pub fn not_configured(what: &str) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "not_configured",
            format!("{what} is not configured"),
        )
    }

    /// Fail closed: the caller runs every journey / routes to humans.
    pub fn engine_unavailable(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "engine_unavailable",
            detail,
        )
    }

    pub fn store_unavailable(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "store_unavailable", detail)
    }

    /// The detail is generic on purpose: internal errors may carry adapter
    /// responses, and nothing from them reaches the client.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error (see server logs)",
        )
    }

    /// `StoreError` mapping shared by every route.
    pub fn from_store(err: &StoreError) -> Self {
        match err {
            StoreError::Unavailable(_) => Self::store_unavailable("the store is unavailable"),
            StoreError::Unconfigured(_) => Self::not_configured("the store"),
            StoreError::Invalid(_) => {
                tracing::error!(error = %err, "store rejected the operation");
                Self::internal()
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.code, "detail": self.detail})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_errors_map_to_the_contract_codes() {
        let e = ApiError::from_store(&StoreError::Unavailable("pool".into()));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::SERVICE_UNAVAILABLE, "store_unavailable")
        );
        let e = ApiError::from_store(&StoreError::Unconfigured("x".into()));
        assert_eq!(e.code, "not_configured");
        let e = ApiError::from_store(&StoreError::Invalid("secret row".into()));
        assert_eq!(e.code, "internal");
        assert!(!e.detail.contains("secret"));
    }
}
