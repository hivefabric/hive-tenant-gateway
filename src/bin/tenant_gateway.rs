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

use std::str::FromStr;
use std::sync::Arc;

use hive_mcp_gateway::{tools::HttpMcpTools, HoneycombClient};
use hive_tenant_gateway::{
    router,
    tenant::{ApiKeyScope, InMemoryTenantStore, TenantStore},
    AppState, DefaultFrontierLlmFactory, FrontierLlmFactory, KeyVault,
};
use sqlx::postgres::PgPoolOptions;
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
    const DEV_API_KEYS: &[&str] = &["dev-hive-key", "dev", "secret", "test", "hive-key"];
    if let Some(ref k) = honeycomb_api_key {
        if DEV_API_KEYS.iter().any(|d| k.as_str() == *d) {
            tracing::warn!(
                "HONEYCOMB_API_KEY is set to a known dev default ('{}') — rotate before exposing to network",
                k
            );
        }
    }

    let tools = Arc::new(HttpMcpTools::new(HoneycombClient::new(
        honeycomb_url.clone(),
        honeycomb_api_key.clone(),
    )));
    let frontier_factory: Arc<dyn FrontierLlmFactory> = Arc::new(DefaultFrontierLlmFactory);

    let demo_queen_urn = std::env::var("DEMO_QUEEN_URN").ok();
    let mut dev_seed_key: Option<String> = None;
    let mut dev_seed_tenant_id: Option<uuid::Uuid> = None;

    let mut pg_pool: Option<sqlx::postgres::PgPool> = None;

    let tenants: Arc<dyn TenantStore> = match database_url {
        Some(url) => {
            tracing::info!("DATABASE_URL set — using Postgres TenantStore; running migrations");
            let store = hive_tenant_gateway::tenant::pg::PgTenantStore::connect(&url).await?;
            store.migrate().await?;
            // Also keep a direct pool for tables not covered by TenantStore (e.g. chat_sessions).
            let pool = PgPoolOptions::new()
                .max_connections(5)
                .connect(&url)
                .await?;
            pg_pool = Some(pool);
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
    if let Some(pool) = pg_pool {
        state = state.with_pg_pool(pool);
    }
    if let Some(vault) = KeyVault::from_env() {
        tracing::info!("TENANT_LLM_SECRET_KEY loaded — LLM API keys will be encrypted at rest");
        state = state.with_vault(vault);
    } else {
        tracing::warn!(
            "TENANT_LLM_SECRET_KEY not set — LLM API keys stored unencrypted (dev mode)"
        );
    }
    let app = router(state.clone());

    if state.pg_pool.is_some() {
        tokio::spawn(schedule_runner(state.clone()));
    }

    let listener = TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Background task: polls `schedules` every 30 s and fires any due entries.
///
/// Phase 1: computes `next_run_at`, sets `last_run_at`, and logs the task.
/// Phase 2: look up tenant's API key and fire via gateway's own /v1/mcp/tools/call.
async fn schedule_runner(state: hive_tenant_gateway::AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        if let Some(ref pool) = state.pg_pool {
            // Find schedules due to run (enabled + next_run_at in the past or NULL).
            let due = sqlx::query(
                "SELECT s.*, t.id as tid FROM schedules s
                 JOIN tenants t ON t.id = s.tenant_id
                 WHERE s.enabled = TRUE
                   AND (s.next_run_at IS NULL OR s.next_run_at <= NOW())
                 LIMIT 50",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_default();

            for row in due {
                use sqlx::Row as _;
                let sched_id: uuid::Uuid = row.get("id");
                let tenant_id: uuid::Uuid = row.get("tenant_id");
                let payload: serde_json::Value = row.get("task_payload");
                let cron_expr: String = row.get("cron");

                // Compute next_run_at from cron expression.
                let next = cron::Schedule::from_str(&cron_expr)
                    .ok()
                    .and_then(|s| s.upcoming(chrono::Utc).next())
                    .map(|t| t.with_timezone(&chrono::Utc));

                // Advance the schedule: set last_run_at and next_run_at.
                let _ = sqlx::query(
                    "UPDATE schedules SET last_run_at = NOW(), next_run_at = $1, updated_at = NOW() WHERE id = $2",
                )
                .bind(next)
                .bind(sched_id)
                .execute(pool)
                .await;

                // Extract task parameters.
                let prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _cap_urn = payload
                    .get("capability_urn")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if !prompt.is_empty() {
                    tracing::info!(
                        schedule_id = %sched_id,
                        tenant_id = %tenant_id,
                        "firing scheduled task (Phase 1: log only)"
                    );
                    // Phase 2: look up tenant's API key and fire via gateway's own /v1/mcp/tools/call.
                }
            }
        }
    }
}
