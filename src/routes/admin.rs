//! Admin surface — provision tenants, mint API keys, revoke keys.
//!
//! All `/admin/v1/*` endpoints require the [`crate::auth::AdminAuth`]
//! extractor: the request must carry `x-admin-key: <HF_ADMIN_KEY>` and that
//! value must equal the plaintext key the operator configured at boot via
//! the `HF_ADMIN_KEY` env var. Without that env var set, the admin surface
//! is disabled and every endpoint returns 503.

use axum::{
    extract::{Path, State},
    routing::{delete, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AdminAuth;
use crate::budget::BudgetDefaults;
use crate::error::GatewayResult;
use crate::tenant::{ApiKeyMint, ApiKeyScope, Tenant};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/v1/tenants", post(create_tenant))
        .route("/admin/v1/tenants/:id/api-keys", post(mint_api_key))
        .route("/admin/v1/api-keys/:id", delete(revoke_api_key))
}

#[derive(Debug, Deserialize)]
struct CreateTenantRequest {
    name: String,
    plan: Option<String>,
    budget_defaults: Option<BudgetDefaults>,
}

#[derive(Debug, Serialize)]
struct CreateTenantResponse {
    tenant: Tenant,
    initial_api_key: ApiKeyMint,
}

async fn create_tenant(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> GatewayResult<Json<CreateTenantResponse>> {
    let tenant = state
        .tenants
        .create_tenant(req.name, req.plan, req.budget_defaults)
        .await?;
    let initial_api_key = state
        .tenants
        .mint_api_key(tenant.id, vec![ApiKeyScope::ToolsInvoke])
        .await?;
    Ok(Json(CreateTenantResponse {
        tenant,
        initial_api_key,
    }))
}

#[derive(Debug, Deserialize)]
struct MintApiKeyRequest {
    #[serde(default = "default_scopes")]
    scopes: Vec<ApiKeyScope>,
}

fn default_scopes() -> Vec<ApiKeyScope> {
    vec![ApiKeyScope::ToolsInvoke]
}

async fn mint_api_key(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    Json(req): Json<MintApiKeyRequest>,
) -> GatewayResult<Json<ApiKeyMint>> {
    let mint = state.tenants.mint_api_key(tenant_id, req.scopes).await?;
    Ok(Json(mint))
}

async fn revoke_api_key(
    _admin: AdminAuth,
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
) -> GatewayResult<Json<serde_json::Value>> {
    state.tenants.revoke_api_key(key_id).await?;
    Ok(Json(serde_json::json!({"ok": true, "id": key_id})))
}
