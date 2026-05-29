//! Tenant self-service: manage your own LLM providers and view credit usage.
//!
//! Authenticated with the tenant's own bearer token (no admin key needed).
//!
//! GET    /v1/me/usage                    credit balance + recent ledger events
//! GET    /v1/me/combs                    list combs belonging to this tenant (proxied from honeycomb)
//! GET    /v1/me/combs/owned              list only combs where owner_user_id == this tenant's UUID
//! POST   /v1/me/combs/refresh            refresh cells on a specific comb
//! POST   /v1/me/combs/enrol              generate start command for a user-owned comb
//! POST   /v1/me/llm-providers            register a new LLM API key
//! GET    /v1/me/llm-providers            list registered providers (no key material)
//! DELETE /v1/me/llm-providers/{id}       remove a provider

use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::tenant::{ApiKeyScope, LlmProviderView, NewLlmProvider, TenantPreferences};
use crate::vault;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/usage", get(usage))
        .route("/v1/me/combs", get(list_combs))
        .route("/v1/me/combs/owned", get(list_owned_combs))
        .route("/v1/me/combs/enrol", post(enrol_comb))
        .route("/v1/me/combs/refresh", post(refresh_comb))
        .route("/v1/me/preferences", get(get_preferences).post(set_preferences))
        .route(
            "/v1/me/llm-providers",
            post(register_provider).get(list_providers),
        )
        .route("/v1/me/llm-providers/:id", delete(delete_provider))
}

/// GET /v1/me/combs
///
/// Returns the list of combs belonging to this tenant. Combs register directly
/// with honeycomb (not the gateway), so we proxy GET /api/nodes from honeycomb.
///
/// TODO: filter by owner_user_id once the enrolment flow stamps owner on registration.
///       For now we return ALL online nodes unfiltered so the demo works end-to-end.
async fn list_combs(
    _auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<serde_json::Value>> {
    let honeycomb_url = state.honeycomb_url.as_deref().unwrap_or("http://localhost:8080");
    let url = format!("{honeycomb_url}/api/nodes");

    let mut req = reqwest::Client::new().get(&url);
    if let Some(api_key) = state.honeycomb_api_key.as_deref() {
        req = req.header("x-api-key", api_key);
    }

    let resp = req.send().await.map_err(|e| {
        GatewayError::Internal(format!("honeycomb request failed: {e}"))
    })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(GatewayError::Internal(format!(
            "honeycomb /api/nodes returned {status}: {body}"
        )));
    }

    let nodes: serde_json::Value = resp.json().await.map_err(|e| {
        GatewayError::Internal(format!("failed to parse honeycomb response: {e}"))
    })?;

    Ok(Json(nodes))
}

/// GET /v1/me/combs/owned
///
/// Returns only combs where `owner_user_id` matches this tenant's UUID.
/// Proxies GET /api/nodes from honeycomb and filters client-side.
async fn list_owned_combs(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<serde_json::Value>> {
    let honeycomb_url = state.honeycomb_url.as_deref().unwrap_or("http://localhost:8080");
    let url = format!("{honeycomb_url}/api/nodes");

    let mut req = reqwest::Client::new().get(&url);
    if let Some(api_key) = state.honeycomb_api_key.as_deref() {
        req = req.header("x-api-key", api_key);
    }

    let resp = req.send().await.map_err(|e| {
        GatewayError::Internal(format!("honeycomb request failed: {e}"))
    })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(GatewayError::Internal(format!(
            "honeycomb /api/nodes returned {status}: {body}"
        )));
    }

    let nodes: serde_json::Value = resp.json().await.map_err(|e| {
        GatewayError::Internal(format!("failed to parse honeycomb response: {e}"))
    })?;

    let owner_id = auth.tenant.id.to_string();

    // Filter to nodes where owner_user_id matches this tenant's UUID.
    let owned: Vec<JsonValue> = match nodes {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter(|n| {
                n.get("owner_user_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s == owner_id)
                    .unwrap_or(false)
            })
            .collect(),
        other => {
            // If honeycomb returns a non-array (e.g. wrapped object), return as-is.
            return Ok(Json(other));
        }
    };

    Ok(Json(serde_json::Value::Array(owned)))
}

/// Request body for POST /v1/me/combs/refresh
#[derive(Debug, Deserialize)]
struct RefreshCombRequest {
    comb_id: String,
}

/// POST /v1/me/combs/refresh
///
/// Asks honeycomb to refresh cells on a specific comb via
/// `PATCH /api/nodes/{comb_id}/refresh_cells`. If the endpoint does not exist
/// on the upstream, returns a guidance note instead.
async fn refresh_comb(
    _auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<RefreshCombRequest>,
) -> GatewayResult<Json<serde_json::Value>> {
    let honeycomb_url = state.honeycomb_url.as_deref().unwrap_or("http://localhost:8080");
    let url = format!("{honeycomb_url}/api/nodes/{}/refresh_cells", req.comb_id);

    let mut patch_req = reqwest::Client::new().patch(&url);
    if let Some(api_key) = state.honeycomb_api_key.as_deref() {
        patch_req = patch_req.header("x-api-key", api_key);
    }

    let resp = patch_req.send().await.map_err(|e| {
        GatewayError::Internal(format!("honeycomb request failed: {e}"))
    })?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND
        || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
        // Endpoint not implemented on this honeycomb version — return guidance.
        return Ok(Json(serde_json::json!({
            "ok": true,
            "note": "restart your comb to regenerate cells"
        })));
    }

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(GatewayError::Internal(format!(
            "honeycomb /api/nodes/{}/refresh_cells returned {status}: {body}",
            req.comb_id
        )));
    }

    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({"ok": true}));
    Ok(Json(body))
}

/// Request body for POST /v1/me/combs/enrol
#[derive(Debug, Deserialize)]
struct EnrolCombRequest {
    name: String,
    capabilities: String,
    #[serde(default = "default_comb_port")]
    port: u16,
}

fn default_comb_port() -> u16 { 7072 }

/// POST /v1/me/combs/enrol
///
/// Generates the start command and TOML config for a user-owned comb. Pure
/// generation — no DB write. The caller runs the command on their device;
/// the comb will register with honeycomb carrying the owner's user UUID so it
/// appears in their "My Hive" tab.
async fn enrol_comb(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<EnrolCombRequest>,
) -> GatewayResult<Json<serde_json::Value>> {
    let owner_user_id = auth.tenant.id.to_string();
    let honeycomb_url = state.honeycomb_url.as_deref().unwrap_or("http://localhost:8080");
    let honeycomb_api_key = state.honeycomb_api_key.as_deref().unwrap_or("dev-hive-key");

    let node_name = req.name.clone();
    let port = req.port;
    let listen_addr = format!("0.0.0.0:{port}");
    let advertise_url = format!("http://localhost:{port}");
    let node_id_file = format!("/tmp/hive-combs/{}.node_id", node_name);
    let config_path = format!("/tmp/hive-combs/{}.toml", node_name);

    // LLM-only capabilities.
    let capabilities_block = format!(
        "[[capabilities]]\nurn = \"oasf://commons/inference/generic/v1\"\nhandler = \"llm:default\"\ndescription = \"LLM inference on {node_name}\"\n"
    );

    let config_toml = format!(
        r#"node_name = "{node_name}"
control_plane_url = "{honeycomb_url}"
control_plane_api_key = "{honeycomb_api_key}"
advertise_node_api_base_url = "{advertise_url}"
node_id_file = "{node_id_file}"
wasm_enabled = false
docker_enabled = false
max_concurrency = 4
listen_addr = "{listen_addr}"

[resource_offer]
memory_offer_pct = 50
cpu_max_percent = 75
battery_min_pct = 0
only_when_charging = false
thermal_max = "warm"
sensitivity_accepted = ["public", "private"]

{capabilities_block}"#
    );

    let command = format!(
        "mkdir -p /tmp/hive-combs && cat > {config_path} << 'TOML'\n{config_toml}\nTOML\nCOMB_NODE_CONFIG={config_path} HIVE_OWNER_USER_ID={owner_user_id} RUST_LOG=info ./headless_server"
    );

    Ok(Json(serde_json::json!({
        "command": command,
        "config_toml": config_toml,
        "note": "Run this command on any device. The comb will appear in your My Hive tab."
    })))
}

async fn usage(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<serde_json::Value>> {
    let Some(ref client) = state.ledger_client else {
        return Ok(Json(serde_json::json!({
            "tenant_id": auth.tenant.id,
            "balance_credits": null,
            "recent_events": [],
            "note": "ledger not configured — set LEDGER_URL to enable credit tracking"
        })));
    };

    let balance = client.balance(auth.tenant.id).await.unwrap_or(-1);
    Ok(Json(serde_json::json!({
        "tenant_id": auth.tenant.id,
        "balance_credits": balance,
    })))
}

async fn register_provider(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(input): Json<NewLlmProvider>,
) -> GatewayResult<Json<LlmProviderView>> {
    auth.require_scope(ApiKeyScope::Orchestrate)?;

    let api_key_enc = vault::encode_for_storage(state.vault.as_deref(), &input.api_key)?;
    let provider = state
        .tenants
        .store_llm_provider(auth.tenant.id, input, api_key_enc)
        .await?;

    tracing::info!(
        tenant_id = %auth.tenant.id,
        provider_id = %provider.id,
        provider_name = %provider.name,
        "LLM provider registered"
    );

    Ok(Json(LlmProviderView::from(provider)))
}

async fn list_providers(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<Vec<LlmProviderView>>> {
    let providers = state.tenants.list_llm_providers(auth.tenant.id).await?;
    Ok(Json(providers.into_iter().map(LlmProviderView::from).collect()))
}

async fn get_preferences(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<TenantPreferences>> {
    let prefs = state
        .preferences
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&auth.tenant.id)
        .cloned()
        .unwrap_or_default();
    Ok(Json(prefs))
}

async fn set_preferences(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(mut update): Json<TenantPreferences>,
) -> GatewayResult<Json<TenantPreferences>> {
    // Validate ranges
    if update.max_execution_seconds < 30 || update.max_execution_seconds > 3600 {
        return Err(GatewayError::Invalid(
            "max_execution_seconds must be 30–3600".to_string(),
        ));
    }
    // Enforce premium tier minimum pool share.
    if update.tier == "premium" && update.pool_share_pct < 50 {
        update.pool_share_pct = 50;
    }
    {
        let mut map = state
            .preferences
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(auth.tenant.id, update.clone());
    }
    // Fire-and-forget: ask honeycomb to refresh cells so the comb immediately
    // reflects any queen_model or pool_share_pct changes without a restart.
    maybe_refresh_comb_cells(&state, &update).await;
    Ok(Json(update))
}

/// Fire-and-forget helper: calls PATCH /api/nodes/{comb_id}/refresh_cells on
/// honeycomb when `queen_comb_id` is set in preferences. Errors are silently
/// swallowed — this is a best-effort optimisation, not a hard requirement.
async fn maybe_refresh_comb_cells(
    state: &AppState,
    prefs: &TenantPreferences,
) {
    let comb_id = match &prefs.queen_comb_id {
        Some(id) => id.clone(),
        None => return,
    };
    let pool_share_pct = prefs.pool_share_pct;
    let queen_model = prefs.queen_model.clone();
    let honeycomb_url = state.honeycomb_url.as_deref().unwrap_or("http://localhost:8080");
    let api_key = state.honeycomb_api_key.as_deref().unwrap_or("");
    let url = format!(
        "{}/api/nodes/{}/refresh_cells",
        honeycomb_url.trim_end_matches('/'),
        comb_id
    );
    let body = serde_json::json!({
        "queen_model": queen_model,
        "pool_share_pct": pool_share_pct,
    });
    let client = reqwest::Client::new();
    let _ = client
        .patch(&url)
        .header("x-api-key", api_key)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    // Fire-and-forget: errors are silently ignored
}

async fn delete_provider(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(provider_id): Path<Uuid>,
) -> GatewayResult<Json<serde_json::Value>> {
    // Verify ownership before deleting.
    let found = state
        .tenants
        .get_llm_provider(auth.tenant.id, provider_id)
        .await?;
    if found.is_none() {
        return Err(GatewayError::TenantNotFound);
    }
    state
        .tenants
        .delete_llm_provider(auth.tenant.id, provider_id)
        .await?;
    Ok(Json(serde_json::json!({"ok": true})))
}
