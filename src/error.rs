use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("missing or malformed Authorization header")]
    Unauthorized,
    #[error("invalid API key")]
    InvalidApiKey,
    #[error("API key revoked")]
    KeyRevoked,
    #[error("admin surface disabled — set HF_ADMIN_KEY to enable")]
    AdminDisabled,
    #[error("missing required scope: {0}")]
    MissingScope(&'static str),
    #[error("tenant not found")]
    TenantNotFound,
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("rate limit exceeded")]
    RateLimited { retry_after_secs: u64 },
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("upstream MCP gateway error: {0}")]
    Upstream(#[from] hive_mcp_gateway::GatewayError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type GatewayResult<T> = Result<T, GatewayError>;

impl From<hive_sdk::frontier::FrontierLlmError> for GatewayError {
    fn from(e: hive_sdk::frontier::FrontierLlmError) -> Self {
        GatewayError::Internal(format!("frontier LLM: {e}"))
    }
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            GatewayError::Unauthorized | GatewayError::InvalidApiKey | GatewayError::KeyRevoked => {
                (StatusCode::UNAUTHORIZED, "unauthorized")
            }
            GatewayError::AdminDisabled => (StatusCode::SERVICE_UNAVAILABLE, "admin_disabled"),
            GatewayError::MissingScope(_) => (StatusCode::FORBIDDEN, "forbidden"),
            GatewayError::TenantNotFound => (StatusCode::NOT_FOUND, "not_found"),
            GatewayError::BudgetExceeded => (StatusCode::PAYMENT_REQUIRED, "budget_exceeded"),
            GatewayError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            GatewayError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid"),
            GatewayError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream"),
            GatewayError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let mut response = (
            status,
            Json(json!({
                "error": code,
                "message": self.to_string(),
            })),
        )
            .into_response();
        if let GatewayError::RateLimited { retry_after_secs } = self {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("60")),
            );
        }
        response
    }
}
