//! Tenant self-service: manage your own LLM providers and view credit usage.
//!
//! Authenticated with the tenant's own bearer token (no admin key needed).
//!
//! GET    /v1/me/usage                    credit balance + recent ledger events
//! GET    /v1/me/combs                    list combs belonging to this tenant (proxied from honeycomb)
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
        .route("/v1/me/combs/enrol", post(enrol_comb))
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

    // Determine capabilities to advertise based on the requested type.
    let (wasm_enabled, docker_enabled, capabilities_block) = match req.capabilities.to_lowercase().as_str() {
        "docker" => (false, true, format!(
            "[[capabilities]]\nurn = \"oasf://hive/docker/v1\"\nhandler = \"docker:default\"\ndescription = \"Docker task runner on {node_name}\"\n"
        )),
        "both" => (true, true, format!(
            "[[capabilities]]\nurn = \"oasf://commons/inference/generic/v1\"\nhandler = \"llm:default\"\ndescription = \"LLM inference on {node_name}\"\n\n[[capabilities]]\nurn = \"oasf://hive/docker/v1\"\nhandler = \"docker:default\"\ndescription = \"Docker task runner on {node_name}\"\n"
        )),
        // default: "llm"
        _ => (true, false, format!(
            "[[capabilities]]\nurn = \"oasf://commons/inference/generic/v1\"\nhandler = \"llm:default\"\ndescription = \"LLM inference on {node_name}\"\n"
        )),
    };

    let config_toml = format!(
        r#"node_name = "{node_name}"
control_plane_url = "{honeycomb_url}"
control_plane_api_key = "{honeycomb_api_key}"
advertise_node_api_base_url = "{advertise_url}"
node_id_file = "{node_id_file}"
wasm_enabled = {wasm_enabled}
docker_enabled = {docker_enabled}
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
    Json(update): Json<TenantPreferences>,
) -> GatewayResult<Json<TenantPreferences>> {
    // Validate ranges
    if update.max_execution_seconds < 30 || update.max_execution_seconds > 3600 {
        return Err(GatewayError::Invalid(
            "max_execution_seconds must be 30–3600".to_string(),
        ));
    }
    let mut map = state
        .preferences
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.insert(auth.tenant.id, update.clone());
    Ok(Json(update))
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
