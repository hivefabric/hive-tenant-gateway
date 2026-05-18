//! Multi-tenant BYO-LLM HTTP gateway.
//!
//! Each customer brings their own frontier LLM (Anthropic / OpenAI / Gemini /
//! Bedrock / self-hosted) via API. They authenticate as a tenant against this
//! gateway and dispatch work to the Comb network through HTTP equivalents of
//! the MCP tools (`describe_cluster`, `run_subagent`, `estimate_cost`).
//!
//! The customer owns the orchestrator loop. We own the network and the SLM
//! substrate.

pub mod auth;
pub mod budget;
pub mod error;
pub mod frontier;
pub mod routes;
pub mod tenant;

pub use error::{GatewayError, GatewayResult};
pub use frontier::{
    DefaultFrontierLlmFactory, FrontierLlm, FrontierLlmError, FrontierLlmFactory,
    LlmProviderConfig,
};
pub use tenant::{ApiKey, ApiKeyScope, InMemoryTenantStore, Tenant, TenantStore};

use std::sync::Arc;

use axum::Router;
use hive_mcp_gateway::tools::McpTools;

/// Erased pointer to the underlying MCP tools impl. Using `dyn` avoids
/// forcing handlers to be generic over a concrete `McpTools` type.
pub type DynMcpTools = Arc<dyn McpTools + Send + Sync + 'static>;

/// Application state shared across handlers.
#[derive(Clone)]
pub struct AppState {
    pub tenants: Arc<dyn TenantStore>,
    pub tools: DynMcpTools,
    pub frontier_factory: Arc<dyn FrontierLlmFactory>,
    /// Plaintext admin key required on `/admin/v1/*`. `None` disables the
    /// admin surface entirely (every admin route returns 503).
    pub admin_key: Option<String>,
}

impl AppState {
    pub fn new(
        tenants: Arc<dyn TenantStore>,
        tools: DynMcpTools,
        frontier_factory: Arc<dyn FrontierLlmFactory>,
    ) -> Self {
        Self {
            tenants,
            tools,
            frontier_factory,
            admin_key: None,
        }
    }

    /// Enable the admin surface by setting the expected `x-admin-key` value.
    pub fn with_admin_key(mut self, admin_key: String) -> Self {
        self.admin_key = Some(admin_key);
        self
    }
}

/// Build the axum `Router` for the tenant gateway.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::mcp::router())
        .merge(routes::admin::router())
        .merge(routes::orchestrate::router())
        .with_state(state)
}
