//! `tenant-gateway` — multi-tenant BYO-LLM HTTP entry point.
//!
//! Configure via env:
//!   GATEWAY_BIND        bind address (default 0.0.0.0:8090)
//!   HONEYCOMB_URL       upstream Honeycomb base URL (default http://localhost:8080)
//!   HONEYCOMB_API_KEY   optional Honeycomb x-api-key
//!   DATABASE_URL        Postgres URL. When set, a Postgres-backed
//!                       TenantStore is used and migrations are run at boot.
//!                       When unset, an in-memory TenantStore is used and a
//!                       dev seed tenant is created (useful for local dev).
//!   HF_ADMIN_KEY        plaintext admin key required on every `/admin/v1/*`
//!                       request as `x-admin-key`. When unset, the admin
//!                       surface is disabled (every admin endpoint returns
//!                       503 — operators must opt in explicitly).
//!   SEED_TENANT_NAME    name of the dev seed tenant (default "dev"), only
//!                       used when DATABASE_URL is unset.

use std::sync::Arc;

use hive_mcp_gateway::{tools::HttpMcpTools, HoneycombClient};
use hive_tenant_gateway::{
    router,
    tenant::{ApiKeyScope, InMemoryTenantStore, TenantStore},
    AppState, DefaultFrontierLlmFactory, FrontierLlmFactory, KeyVault,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind = std::env::var("GATEWAY_BIND").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let honeycomb_url =
        std::env::var("HONEYCOMB_URL").unwrap_or_else(|_| "http://localhost:8080".into());
    let honeycomb_api_key = std::env::var("HONEYCOMB_API_KEY").ok();
    let admin_key = std::env::var("HF_ADMIN_KEY").ok();
    let database_url = std::env::var("DATABASE_URL").ok();
    let seed_name = std::env::var("SEED_TENANT_NAME").unwrap_or_else(|_| "dev".into());
    let ledger_url = std::env::var("LEDGER_URL").ok();

    tracing::info!(%bind, %honeycomb_url, "tenant-gateway starting");
    if admin_key.is_none() {
        tracing::warn!(
            "HF_ADMIN_KEY is unset — admin surface (/admin/v1/*) will return 503. \
             Set it to enable tenant provisioning."
        );
    }

    let tools = Arc::new(HttpMcpTools::new(HoneycombClient::new(
        honeycomb_url.clone(),
        honeycomb_api_key.clone(),
    )));
    let frontier_factory: Arc<dyn FrontierLlmFactory> = Arc::new(DefaultFrontierLlmFactory);

    let demo_queen_urn = std::env::var("DEMO_QUEEN_URN").ok();
    let mut dev_seed_key: Option<String> = None;
    let mut dev_seed_tenant_id: Option<uuid::Uuid> = None;

    let tenants: Arc<dyn TenantStore> = match database_url {
        Some(url) => {
            tracing::info!("DATABASE_URL set — using Postgres TenantStore; running migrations");
            let store = hive_tenant_gateway::tenant::pg::PgTenantStore::connect(&url).await?;
            store.migrate().await?;
            Arc::new(store)
        }
        None => {
            tracing::info!("DATABASE_URL unset — using in-memory TenantStore (dev mode)");
            let store = InMemoryTenantStore::new();
            // Dev seed: one tenant + one fully-scoped key. Skip when running
            // against Postgres (operator provisions tenants via /admin).
            let tenant = store.create_tenant(seed_name, None, None).await?;
            let mint = store
                .mint_api_key(
                    tenant.id,
                    vec![ApiKeyScope::ToolsInvoke, ApiKeyScope::Orchestrate],
                )
                .await?;
            dev_seed_key = Some(mint.plaintext.clone());
            dev_seed_tenant_id = Some(tenant.id);
            eprintln!();
            eprintln!("[tenant-gateway] dev seed tenant ready");
            eprintln!("[tenant-gateway]   tenant_id   = {}", tenant.id);
            eprintln!("[tenant-gateway]   tenant_name = {}", tenant.name);
            eprintln!("[tenant-gateway]   plan        = {}", tenant.plan);
            eprintln!("[tenant-gateway]   API KEY (shown once) = {}", mint.plaintext);
            eprintln!();
            eprintln!(
                "Try: curl -H 'Authorization: Bearer {}' http://{}/v1/whoami",
                mint.plaintext, bind
            );
            eprintln!("Demo console: http://{}/ui", bind);
            eprintln!();
            Arc::new(store)
        }
    };

    let mut state = AppState::new(tenants, tools, frontier_factory);
    if let Some(key) = admin_key {
        state = state.with_admin_key(key);
    }
    if let Some(key) = dev_seed_key {
        state = state.with_dev_seed_key(key);
    }
    if let Some(urn) = demo_queen_urn {
        state = state.with_demo_queen_urn(urn);
    }
    if let Some(tid) = dev_seed_tenant_id {
        state = state.with_dev_seed_tenant_id(tid);
    }
    state = state.with_honeycomb_dashboard(honeycomb_url, honeycomb_api_key);
    if let Some(url) = ledger_url {
        state = state.with_ledger_url(url);
    }
    if let Some(vault) = KeyVault::from_env() {
        tracing::info!("TENANT_LLM_SECRET_KEY loaded — LLM API keys will be encrypted at rest");
        state = state.with_vault(vault);
    } else {
        tracing::warn!(
            "TENANT_LLM_SECRET_KEY not set — LLM API keys stored unencrypted (dev mode)"
        );
    }
    let app = router(state);

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
