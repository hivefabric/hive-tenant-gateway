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
    #[error("missing required scope: {0}")]
    MissingScope(&'static str),
    #[error("tenant not found")]
    TenantNotFound,
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("upstream MCP gateway error: {0}")]
    Upstream(#[from] hive_mcp_gateway::GatewayError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type GatewayResult<T> = Result<T, GatewayError>;

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            GatewayError::Unauthorized | GatewayError::InvalidApiKey | GatewayError::KeyRevoked => {
                (StatusCode::UNAUTHORIZED, "unauthorized")
            }
            GatewayError::MissingScope(_) => (StatusCode::FORBIDDEN, "forbidden"),
            GatewayError::TenantNotFound => (StatusCode::NOT_FOUND, "not_found"),
            GatewayError::BudgetExceeded => (StatusCode::PAYMENT_REQUIRED, "budget_exceeded"),
            GatewayError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid"),
            GatewayError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream"),
            GatewayError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let body = Json(json!({
            "error": code,
            "message": self.to_string(),
        }));
        (status, body).into_response()
    }
}
