//! `/v1/_demo/*` — demo-only aggregator endpoints used by the UI overview
//! panel. Not part of the tenant-facing API contract; gated to the
//! authenticated tenant the same way the rest of `/v1/*` is.

use axum::{
    extract::State,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use serde_json::Value;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/_demo/overview", get(overview))
}

#[derive(Debug, Serialize)]
struct OverviewResponse {
    /// Honeycomb-side state: registered combs.
    nodes: Value,
    /// Honeycomb-side state: most recent tasks (newest first, capped).
    tasks: Value,
    /// Honeycomb-side state: capability registry from `capabilities.toml`.
    capabilities: Value,
    /// Hive Ledger-side state: this tenant's running balance.
    ledger_balance: Option<i64>,
    /// Hive Ledger-side state: recent credit events.
    ledger_events: Option<Value>,
    /// Resolved tenant id for the seed tenant (so the UI knows which tenant
    /// the ledger panel reflects).
    tenant_id: String,
}

async fn overview(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<OverviewResponse>> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| GatewayError::Internal(format!("reqwest build: {e}")))?;

    let nodes = match (state.honeycomb_url.as_deref(), state.honeycomb_api_key.as_deref()) {
        (Some(url), key) => proxy(&http, url, "/api/nodes", key).await,
        _ => Value::Null,
    };
    let tasks = match (state.honeycomb_url.as_deref(), state.honeycomb_api_key.as_deref()) {
        (Some(url), key) => proxy(&http, url, "/api/tasks", key).await,
        _ => Value::Null,
    };
    let capabilities = match (state.honeycomb_url.as_deref(), state.honeycomb_api_key.as_deref()) {
        (Some(url), key) => proxy(&http, url, "/api/capabilities", key).await,
        _ => Value::Null,
    };

    let (ledger_balance, ledger_events) = match state.ledger_url.as_deref() {
        Some(ledger_url) => {
            let bal = proxy(
                &http,
                ledger_url,
                &format!("/v1/credits/{}/balance", auth.tenant.id),
                None,
            )
            .await;
            let evs = proxy(
                &http,
                ledger_url,
                &format!("/v1/credits/{}/events?limit=20", auth.tenant.id),
                None,
            )
            .await;
            (bal.get("balance").and_then(Value::as_i64), Some(evs))
        }
        None => (None, None),
    };

    Ok(Json(OverviewResponse {
        nodes,
        tasks,
        capabilities,
        ledger_balance,
        ledger_events,
        tenant_id: auth.tenant.id.to_string(),
    }))
}

/// Best-effort GET to a URL with an optional `x-api-key`. Returns
/// `Value::Null` on any error so the UI can render with partial data
/// when one upstream is sad.
async fn proxy(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    api_key: Option<&str>,
) -> Value {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let mut req = http.get(&url);
    if let Some(k) = api_key {
        req = req.header("x-api-key", k);
    }
    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<Value>().await.unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}
