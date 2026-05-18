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
pub mod routes;
pub mod tenant;

pub use error::{GatewayError, GatewayResult};
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
}

impl AppState {
    pub fn new(tenants: Arc<dyn TenantStore>, tools: DynMcpTools) -> Self {
        Self { tenants, tools }
    }
}

/// Build the axum `Router` for the tenant gateway.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::mcp::router())
        .merge(routes::admin::router())
        .with_state(state)
}
