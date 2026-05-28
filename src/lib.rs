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
pub mod ledger;
pub mod rate_limit;
pub mod routes;
pub mod tenant;
pub mod vault;

// Frontier LLM adapters now live in hive-sdk and are re-exported for back-compat.
pub use hive_sdk::frontier::{
    self as frontier, AssistantBlock, AssistantContent, ChatMessage, ChatResponse,
    DefaultFrontierLlmFactory, FrontierLlm, FrontierLlmError, FrontierLlmFactory,
    LlmProviderConfig, ToolCall, ToolDef,
};

pub use error::{GatewayError, GatewayResult};
pub use tenant::{ApiKey, ApiKeyScope, InMemoryTenantStore, LlmProviderView, Tenant, TenantStore};
pub use vault::KeyVault;

use std::sync::Arc;

use axum::Router;
use tower_http::cors::CorsLayer;
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
    /// Dev-mode seed-tenant bearer; baked into the demo `/ui` page so the
    /// console works out of the box. `None` in production (operator
    /// provisions tenants via `/admin/v1/*`).
    pub dev_seed_key: Option<String>,
    /// Default capability URN to suggest in the demo console's URN field.
    /// Defaults to the queen-decompose URN when not set.
    pub demo_queen_urn: Option<String>,
    /// Tenant id of the dev seed tenant, surfaced by the UI overview
    /// panel for ledger balance lookups. `None` in production.
    pub dev_seed_tenant_id: Option<uuid::Uuid>,
    /// Honeycomb base URL (for proxied dashboard calls). Mirrors the
    /// upstream `HONEYCOMB_URL` env var.
    pub honeycomb_url: Option<String>,
    /// Honeycomb API key (for proxied dashboard calls).
    pub honeycomb_api_key: Option<String>,
    /// Hive ledger base URL. `None` disables ledger panels in the UI
    /// overview AND disables debit/refund recording on run_subagent.
    pub ledger_url: Option<String>,
    /// Lazily constructed ledger client. Built on first use; `None`
    /// when `ledger_url` is unset.
    pub ledger_client: Option<ledger::LedgerClient>,
    /// Key vault for encrypting/decrypting stored LLM API keys.
    /// `None` = dev mode (keys stored with "raw:" prefix, with a startup warning).
    pub vault: Option<std::sync::Arc<KeyVault>>,
    /// Per-tenant rate limiter. Configured via TENANT_RATE_LIMIT_RPM (default 300/min).
    pub rate_limiter: std::sync::Arc<rate_limit::RateLimiter>,
    /// Per-tenant routing/quality preferences (sliders). In-memory; Phase 2.5 will persist to DB.
    pub preferences: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<uuid::Uuid, tenant::TenantPreferences>>>,
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
            dev_seed_key: None,
            demo_queen_urn: None,
            dev_seed_tenant_id: None,
            honeycomb_url: None,
            honeycomb_api_key: None,
            ledger_url: None,
            ledger_client: None,
            vault: None,
            rate_limiter: std::sync::Arc::new(rate_limit::RateLimiter::from_env()),
            preferences: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn with_vault(mut self, vault: KeyVault) -> Self {
        self.vault = Some(std::sync::Arc::new(vault));
        self
    }

    /// Enable the admin surface by setting the expected `x-admin-key` value.
    pub fn with_admin_key(mut self, admin_key: String) -> Self {
        self.admin_key = Some(admin_key);
        self
    }

    pub fn with_dev_seed_key(mut self, key: String) -> Self {
        self.dev_seed_key = Some(key);
        self
    }

    pub fn with_demo_queen_urn(mut self, urn: String) -> Self {
        self.demo_queen_urn = Some(urn);
        self
    }

    pub fn with_dev_seed_tenant_id(mut self, id: uuid::Uuid) -> Self {
        self.dev_seed_tenant_id = Some(id);
        self
    }

    pub fn with_honeycomb_dashboard(mut self, url: String, api_key: Option<String>) -> Self {
        self.honeycomb_url = Some(url);
        self.honeycomb_api_key = api_key;
        self
    }

    pub fn with_ledger_url(mut self, url: String) -> Self {
        self.ledger_client = Some(ledger::LedgerClient::new(url.clone()));
        self.ledger_url = Some(url);
        self
    }
}

/// Build the axum `Router` for the tenant gateway.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::signup::router())
        .merge(routes::me::router())
        .merge(routes::mcp::router())
        .merge(routes::admin::router())
        .merge(routes::orchestrate::router())
        // Note: /ui and /v1/_demo/* have been retired.
        // The user-facing UI now lives in hive-app (separate service on :3000).
        .layer(CorsLayer::permissive())
        .with_state(state)
}
