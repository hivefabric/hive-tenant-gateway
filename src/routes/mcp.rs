//! MCP-equivalent tool surface, exposed over HTTP.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use hive_mcp_gateway::tools::{EstimateCostRequest, RunSubagentRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::tenant::ApiKeyScope;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/whoami", get(whoami))
        .route("/v1/mcp/tools/list", post(tools_list))
        .route("/v1/mcp/tools/call", post(tools_call))
}

#[derive(Debug, Serialize)]
struct WhoamiResponse {
    tenant_id: String,
    tenant_name: String,
    plan: String,
    scopes: Vec<ApiKeyScope>,
}

async fn whoami(auth: AuthedTenant) -> Json<WhoamiResponse> {
    Json(WhoamiResponse {
        tenant_id: auth.tenant.id.to_string(),
        tenant_name: auth.tenant.name,
        plan: auth.tenant.plan,
        scopes: auth.key.scopes,
    })
}

async fn tools_list(_auth: AuthedTenant) -> Json<Value> {
    Json(serde_json::json!({
        "tools": [
            {
                "name": "describe_cluster",
                "description": "List the capabilities (workloads) HiveFabric can serve.",
                "input_schema": { "type": "object", "properties": {}, "required": [] }
            },
            {
                "name": "run_subagent",
                "description": "Run a generic-inference task on the HiveFabric network. Pick a model (model_id or capability_urn) and send a prompt. The 'what' lives in the prompt.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "model_id": { "type": "string" },
                        "capability_urn": { "type": "string" },
                        "prompt": { "type": "string" },
                        "profile": { "type": "string", "default": "default" },
                        "timeout_seconds": { "type": "integer", "minimum": 1, "default": 60 }
                    },
                    "required": ["prompt"]
                }
            },
            {
                "name": "estimate_cost",
                "description": "Pre-execution cost estimate (Phase 2 — requires Honey Ledger).",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "capability_urn": { "type": "string" },
                        "input_size_tokens": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["capability_urn", "input_size_tokens"]
                }
            }
        ]
    }))
}

#[derive(Debug, Deserialize)]
struct ToolsCallRequest {
    name: String,
    #[serde(default)]
    arguments: Value,
}

async fn tools_call(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<ToolsCallRequest>,
) -> GatewayResult<Json<Value>> {
    auth.require_scope(ApiKeyScope::ToolsInvoke)?;

    let span = tracing::info_span!(
        "tenant_gateway.tools_call",
        hivefabric.tenant_id = %auth.tenant.id,
        tool = %req.name,
    );
    let _e = span.enter();

    match req.name.as_str() {
        "describe_cluster" => {
            let resp = state.tools.describe_cluster().await?;
            Ok(Json(serde_json::to_value(resp).map_err(|e| {
                GatewayError::Internal(format!("serialize: {e}"))
            })?))
        }
        "run_subagent" => {
            let mut typed: RunSubagentRequest = serde_json::from_value(req.arguments)
                .map_err(|e| GatewayError::Invalid(format!("run_subagent args: {e}")))?;
            // Source of truth for tenant context is the authenticated bearer.
            // Anything the caller put in the body is silently overridden.
            typed.tenant_id = Some(auth.tenant.id);

            // Apply TenantPreferences sliders to the outbound request.
            // Preferences override TenantConfig defaults when set.
            let prefs = state
                .preferences
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&auth.tenant.id)
                .cloned()
                .unwrap_or_default();

            // Sensitivity: preferences floor takes precedence over TenantConfig default.
            // The Forager in honeycomb will still upgrade further if PII is detected.
            // Sensitivity must be lowercase snake_case to match hive_sdk::Sensitivity serde.
            typed.sensitivity_required = {
                let raw = if prefs.default_sensitivity != "Private" {
                    prefs.default_sensitivity.clone()
                } else {
                    auth.tenant.default_sensitivity.clone().unwrap_or_else(|| "private".to_string())
                };
                Some(raw.to_lowercase())
            };
            typed.jurisdiction_required = auth.tenant.jurisdiction_required.clone();

            // Task timeout: apply preference if caller hasn't set one.
            if typed.timeout_seconds.is_none() {
                typed.timeout_seconds = Some(prefs.max_execution_seconds as u64);
            }

            // Pool routing: if disabled in preferences, restrict to tenant's own combs.
            // This is passed via credits_budget hint for now; full allowed_nodes support
            // requires a protocol change to RunSubagentRequest (Phase 2).
            // TODO Phase 2: add allowed_nodes_hint field to RunSubagentRequest.

            // Session mode: inject the queen's LLM provider into the task payload so
            // queen combs can orchestrate without needing their own API key in the TOML.
            // Priority: dedicated queen_llm_provider_id > tenant default provider.
            if typed.queen_llm.is_none() {
                let prefs_for_queen = state
                    .preferences
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&auth.tenant.id)
                    .cloned()
                    .unwrap_or_default();

                let provider_result = if let Some(qpid) = prefs_for_queen.queen_llm_provider_id {
                    state.tenants.get_llm_provider(auth.tenant.id, qpid).await
                } else {
                    state.tenants.get_default_llm_provider(auth.tenant.id).await
                        .map(|opt| opt.map(|(p, k)| (p, k)))
                };

                if let Ok(Some((provider, enc_key))) = provider_result {
                    if let Ok(api_key) =
                        crate::vault::decode_from_storage(state.vault.as_deref(), &enc_key)
                    {
                        typed.queen_llm = match provider.provider.as_str() {
                            "anthropic" => Some(
                                hive_sdk::frontier::LlmProviderConfig::Anthropic {
                                    model: provider.model,
                                    api_key,
                                    base_url: provider.base_url,
                                },
                            ),
                            _ => Some(hive_sdk::frontier::LlmProviderConfig::Openai {
                                model: provider.model,
                                api_key,
                                base_url: provider.base_url,
                            }),
                        };
                    }
                }
            }

            // Phase-2 billing: debit before dispatch, refund on failure.
            // Idempotency keys mirror the task_id so duplicate POSTs (CDN
            // retries, network blips) don't double-charge. The amount
            // here is a flat 1 credit per call — the next iteration
            // tiers it by capability_urn / token usage.
            let correlation = uuid::Uuid::new_v4().to_string();
            let urn_label = typed
                .capability_urn
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(client) = &state.ledger_client {
                let metadata = serde_json::json!({
                    "capability_urn": urn_label,
                    "model_id": typed.model_id,
                });
                match client
                    .debit(
                        auth.tenant.id,
                        1,
                        &correlation,
                        &format!("debit:{correlation}"),
                        metadata,
                    )
                    .await
                {
                    Ok(resp) => {
                        // Post-debit balance flows into the ACP BudgetContext so
                        // sub-agents can enforce per-call spending caps without a
                        // separate ledger round-trip.
                        if let Some(bal) = resp.get("balance").and_then(|b| b.as_i64()) {
                            typed.credits_budget = Some(bal);
                        }
                    }
                    Err(e) => {
                        // Ledger failures don't block dispatch in dev mode.
                        // Production would gate the call here on balance.
                        tracing::warn!(error = %e, "ledger debit failed; continuing");
                    }
                }
            }

            let result = state.tools.run_subagent(typed.clone()).await;
            // Refund on failure or non-success status.
            if let Some(client) = &state.ledger_client {
                let needs_refund = match &result {
                    Err(_) => true,
                    Ok(resp) => !matches!(
                        resp.status.as_str(),
                        "succeeded" | "completed"
                    ),
                };
                if needs_refund {
                    let metadata = serde_json::json!({
                        "reason": "task did not succeed",
                        "capability_urn": urn_label,
                    });
                    if let Err(e) = client
                        .refund(
                            auth.tenant.id,
                            1,
                            &correlation,
                            &format!("refund:{correlation}"),
                            metadata,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "ledger refund failed");
                    }
                }
            }

            let resp = result?;
            Ok(Json(serde_json::to_value(resp).map_err(|e| {
                GatewayError::Internal(format!("serialize: {e}"))
            })?))
        }
        "estimate_cost" => {
            let typed: EstimateCostRequest = serde_json::from_value(req.arguments)
                .map_err(|e| GatewayError::Invalid(format!("estimate_cost args: {e}")))?;
            let resp = state.tools.estimate_cost(typed).await?;
            Ok(Json(serde_json::to_value(resp).map_err(|e| {
                GatewayError::Internal(format!("serialize: {e}"))
            })?))
        }
        other => Err(GatewayError::Invalid(format!("unknown tool: {other}"))),
    }
}
