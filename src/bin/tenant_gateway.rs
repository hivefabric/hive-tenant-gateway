//! `tenant-gateway` — multi-tenant BYO-LLM HTTP entry point.
//!
//! Dev-mode bootstrap: spins up an in-memory tenant store and creates a
//! single seed tenant + API key on startup so the operator can curl the
//! service without provisioning a Postgres.
//!
//! Configure via env:
//!   GATEWAY_BIND        bind address (default 0.0.0.0:8090)
//!   HONEYCOMB_URL       upstream Honeycomb base URL (default http://localhost:8080)
//!   HONEYCOMB_API_KEY   optional Honeycomb x-api-key
//!   SEED_TENANT_NAME    name of the dev seed tenant (default "dev")
//!
//! On boot the gateway prints the seed tenant's plaintext API key to stderr.
//! Use that as `Authorization: Bearer <plaintext>` for `/v1/*` calls.

use std::sync::Arc;

use hive_mcp_gateway::{tools::HttpMcpTools, HoneycombClient};
use hive_tenant_gateway::{
    router, tenant::ApiKeyScope, AppState, InMemoryTenantStore, TenantStore,
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
    let seed_name = std::env::var("SEED_TENANT_NAME").unwrap_or_else(|_| "dev".into());

    tracing::info!(%bind, %honeycomb_url, "tenant-gateway starting");

    let tools = Arc::new(HttpMcpTools::new(HoneycombClient::new(
        honeycomb_url,
        honeycomb_api_key,
    )));
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());

    let tenant = tenants.create_tenant(seed_name, None, None).await?;
    let mint = tenants
        .mint_api_key(tenant.id, vec![ApiKeyScope::ToolsInvoke])
        .await?;
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
    eprintln!();

    let state = AppState::new(tenants, tools);
    let app = router(state);

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
