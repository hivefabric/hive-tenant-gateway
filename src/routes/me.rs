//! Tenant self-service: manage your own LLM providers and view credit usage.
//!
//! Authenticated with the tenant's own bearer token (no admin key needed).
//!
//! GET    /v1/me/usage                    credit balance + recent ledger events
//! POST   /v1/me/llm-providers            register a new LLM API key
//! GET    /v1/me/llm-providers            list registered providers (no key material)
//! DELETE /v1/me/llm-providers/{id}       remove a provider

use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use uuid::Uuid;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::tenant::{ApiKeyScope, LlmProviderView, NewLlmProvider, TenantPreferences};
use crate::vault;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/usage", get(usage))
        .route("/v1/me/preferences", get(get_preferences).post(set_preferences))
        .route(
            "/v1/me/llm-providers",
            post(register_provider).get(list_providers),
        )
        .route("/v1/me/llm-providers/:id", delete(delete_provider))
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
