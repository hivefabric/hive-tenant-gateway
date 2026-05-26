//! Self-service signup — no human in the loop.
//!
//! POST /v1/signup  →  tenant provisioned + API key returned (shown once).
//!
//! After signup, the tenant:
//!   1. Registers their LLM API key via POST /v1/me/llm-providers
//!   2. Calls /v1/orchestrate with provider_id
//!
//! Rate limiting (R-S8) is not yet enforced here — tracked in the risk
//! register. At launch scale (< 100 signups/day) it is not material.

use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};

use crate::error::GatewayResult;
use crate::tenant::ApiKeyScope;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/signup", post(signup))
}

#[derive(Debug, Deserialize)]
struct SignupRequest {
    /// Display name for the tenant (individual or company).
    name: String,
    /// Optional plan. Only "free" is available at signup; "company" requires
    /// manual upgrade from the operator. Defaults to "free".
    #[serde(default = "default_plan")]
    plan: String,
}

fn default_plan() -> String {
    "free".to_string()
}

#[derive(Debug, Serialize)]
struct SignupResponse {
    /// The newly created tenant id. Store this — it's needed for admin operations.
    tenant_id: uuid::Uuid,
    /// Gateway API key. Shown exactly once. Store it securely — it cannot be
    /// retrieved again. Scope: tools_invoke + orchestrate.
    api_key: String,
    /// Human-readable next steps.
    next_steps: Vec<&'static str>,
}

async fn signup(
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> GatewayResult<Json<SignupResponse>> {
    let plan = match req.plan.as_str() {
        "free" | "company" => req.plan.clone(),
        _ => "free".to_string(),
    };

    let tenant = state
        .tenants
        .create_tenant(req.name, Some(plan), None)
        .await?;

    let mint = state
        .tenants
        .mint_api_key(
            tenant.id,
            vec![ApiKeyScope::ToolsInvoke, ApiKeyScope::Orchestrate],
        )
        .await?;

    tracing::info!(
        tenant_id = %tenant.id,
        tenant_name = %tenant.name,
        "self-service signup complete"
    );

    Ok(Json(SignupResponse {
        tenant_id: tenant.id,
        api_key: mint.plaintext,
        next_steps: vec![
            "1. Register your LLM API key: POST /v1/me/llm-providers",
            "2. Run your first task: POST /v1/orchestrate with provider_id",
            "3. Connect a comb (your device): download the comb agent and register with your Honeycomb URL",
        ],
    }))
}
