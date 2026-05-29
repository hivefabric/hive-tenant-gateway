//! Tenant + API key data model.
//!
//! Two store impls behind one trait:
//! - [`in_memory::InMemoryTenantStore`] — dev/local/tests.
//! - [`pg::PgTenantStore`] — production, sqlx + Postgres.
//!
//! Selection is runtime in `bin/tenant_gateway.rs`: `DATABASE_URL` set picks
//! Postgres; unset picks in-memory.

pub mod in_memory;
pub mod pg;

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::budget::BudgetDefaults;
use crate::error::{GatewayError, GatewayResult};

pub use in_memory::InMemoryTenantStore;

/// A registered LLM provider belonging to a tenant.
/// The `api_key_enc` field is NEVER returned in API responses — it is internal storage only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProvider {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

/// View returned to callers — no key material.
#[derive(Debug, Clone, Serialize)]
pub struct LlmProviderView {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

impl From<LlmProvider> for LlmProviderView {
    fn from(p: LlmProvider) -> Self {
        Self {
            id: p.id,
            name: p.name,
            provider: p.provider,
            model: p.model,
            base_url: p.base_url,
            is_default: p.is_default,
            created_at: p.created_at,
        }
    }
}

/// Input for registering a new LLM provider.
#[derive(Debug, Clone, Deserialize)]
pub struct NewLlmProvider {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

/// Per-tenant routing and quality preferences ("sliders").
/// Applied by the gateway to every outbound task request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantPreferences {
    /// 0–100: % of tasks that try own combs first. Default 80.
    #[serde(default = "default_local_preference_pct")]
    pub local_preference_pct: u8,
    /// Whether tasks can route to pool combs (Mode 3). Default false.
    #[serde(default)]
    pub pool_enabled: bool,
    /// Privacy floor for all tasks. Forager only upgrades, never demotes.
    /// Stored as string to match TaskCreateRequest.sensitivity_required wire format.
    #[serde(default = "default_sensitivity")]
    pub default_sensitivity: String,
    /// Times to retry a failed task before terminal. Default 2.
    #[serde(default = "default_retry_count")]
    pub retry_count: u8,
    /// Fall back to frontier LLM if no comb is available. Default true.
    #[serde(default = "default_frontier_fallback")]
    pub frontier_fallback: bool,
    /// Hard per-task timeout in seconds. Default 300.
    #[serde(default = "default_max_execution_seconds")]
    pub max_execution_seconds: u32,

    // ── Queen configuration ────────────────────────────────────────────────
    /// Node ID of the comb currently serving as this tenant's queen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queen_comb_id: Option<String>,
    /// Capability URN on that comb used for orchestration.
    /// e.g. "oasf://hive/queen/default/v1" or "oasf://hive/queen/qwen3/v1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queen_urn: Option<String>,
    /// LLM provider ID to inject as `queen_llm` for queen tasks.
    /// Takes precedence over the tenant's default provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queen_llm_provider_id: Option<uuid::Uuid>,
    /// Display label — model name shown in the UI (e.g. "qwen3.6:latest").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queen_model: Option<String>,
    /// "local" or "cloud". Drives the chat routing hint shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queen_type: Option<String>,

    // ── Tier + pool sharing ───────────────────────────────────────────────────
    /// Tenant tier controls minimum sharing requirements.
    /// "free" = no minimum pool share.
    /// "premium" = minimum pool_share_pct of 50 always enforced.
    #[serde(default = "default_tier")]
    pub tier: String,
    /// % of worker slots to offer to the pool. 0=none. Premium minimum=50.
    #[serde(default)]
    pub pool_share_pct: u8,
}

fn default_local_preference_pct() -> u8 { 80 }
fn default_sensitivity() -> String { "Private".to_string() }
fn default_retry_count() -> u8 { 2 }
fn default_frontier_fallback() -> bool { true }
fn default_max_execution_seconds() -> u32 { 300 }
fn default_tier() -> String { "free".to_string() }

impl Default for TenantPreferences {
    fn default() -> Self {
        Self {
            local_preference_pct: default_local_preference_pct(),
            pool_enabled: false,
            default_sensitivity: default_sensitivity(),
            retry_count: default_retry_count(),
            frontier_fallback: default_frontier_fallback(),
            max_execution_seconds: default_max_execution_seconds(),
            queen_comb_id: None,
            queen_urn: None,
            queen_llm_provider_id: None,
            queen_model: None,
            queen_type: None,
            tier: default_tier(),
            pool_share_pct: 0,
        }
    }
}

/// Per-key scope. We start narrow and widen by demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyScope {
    /// Call `/v1/mcp/tools/{list,call}`.
    ToolsInvoke,
    /// Call `/v1/orchestrate`.
    Orchestrate,
    /// Read usage reports (Phase 2.2 — needs Ledger).
    ReadUsage,
}

impl ApiKeyScope {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::ToolsInvoke => "tools_invoke",
            Self::Orchestrate => "orchestrate",
            Self::ReadUsage => "read_usage",
        }
    }
    pub fn from_db_str(s: &str) -> Option<Self> {
        Some(match s {
            "tools_invoke" => Self::ToolsInvoke,
            "orchestrate" => Self::Orchestrate,
            "read_usage" => Self::ReadUsage,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub plan: String,
    pub created_at: DateTime<Utc>,
    pub budget_defaults: BudgetDefaults,
    /// Default sensitivity tier the tenant's tasks may request.
    /// `None` = Public (any comb may process). Injected by the gateway into
    /// every outbound TaskCreateRequest so tenants cannot escalate beyond their
    /// plan-assigned tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_sensitivity: Option<String>,
    /// Jurisdiction tags required on every task this tenant submits.
    /// E.g. `["eu-gdpr"]` to ensure all tasks run in GDPR-compliant combs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jurisdiction_required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Argon2 PHC hash of the plaintext key.
    pub key_hash: String,
    pub scopes: Vec<ApiKeyScope>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Returned to the caller exactly once — when a key is freshly minted.
/// We never store the plaintext.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyMint {
    pub api_key: ApiKey,
    /// `hf_<32-byte-base64url>`. Show once; the caller stores it.
    pub plaintext: String,
}

#[async_trait]
pub trait TenantStore: Send + Sync {
    async fn create_tenant(
        &self,
        name: String,
        plan: Option<String>,
        budgets: Option<BudgetDefaults>,
    ) -> GatewayResult<Tenant>;

    async fn get_tenant(&self, id: Uuid) -> GatewayResult<Tenant>;

    async fn mint_api_key(
        &self,
        tenant_id: Uuid,
        scopes: Vec<ApiKeyScope>,
    ) -> GatewayResult<ApiKeyMint>;

    /// Resolve a presented bearer token to a tenant + api-key record.
    /// Returns `Unauthorized`/`InvalidApiKey` for any failure mode.
    async fn resolve_api_key(&self, plaintext: &str) -> GatewayResult<(Tenant, ApiKey)>;

    async fn revoke_api_key(&self, key_id: Uuid) -> GatewayResult<()>;

    // ── LLM provider vault ────────────────────────────────────────────────────

    /// Store a new LLM provider (encrypted key).
    async fn store_llm_provider(
        &self,
        tenant_id: Uuid,
        input: NewLlmProvider,
        api_key_enc: String,
    ) -> GatewayResult<LlmProvider>;

    /// List all LLM providers for a tenant (no key material).
    async fn list_llm_providers(&self, tenant_id: Uuid) -> GatewayResult<Vec<LlmProvider>>;

    /// Get a single provider (including encrypted key) for decryption at dispatch.
    async fn get_llm_provider(
        &self,
        tenant_id: Uuid,
        provider_id: Uuid,
    ) -> GatewayResult<Option<(LlmProvider, String)>>;

    /// Get the default provider for a tenant (including encrypted key).
    async fn get_default_llm_provider(
        &self,
        tenant_id: Uuid,
    ) -> GatewayResult<Option<(LlmProvider, String)>>;

    /// Delete a LLM provider.
    async fn delete_llm_provider(&self, tenant_id: Uuid, provider_id: Uuid) -> GatewayResult<()>;
}

/// Generate a fresh plaintext key in the `hf_<32-byte-base64url>` form.
/// The first 8 chars after `hf_` are the public_id (stored in DB for O(1) lookup).
pub fn mint_plaintext_key() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("hf_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Extract the public_id from a plaintext bearer token.
/// Returns `None` if the token is malformed (too short or wrong prefix).
pub(crate) fn public_id_from_token(token: &str) -> Option<&str> {
    let after_prefix = token.strip_prefix("hf_")?;
    if after_prefix.len() >= 8 {
        Some(&after_prefix[..8])
    } else {
        None
    }
}

pub(crate) fn hash_key(plaintext: &str) -> GatewayResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| GatewayError::Internal(format!("argon2 hash: {e}")))
}

pub(crate) fn verify_key(plaintext: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_plaintext_key_has_correct_shape() {
        let k = mint_plaintext_key();
        assert!(k.starts_with("hf_"));
        // 32 bytes -> 43 base64url-no-pad chars.
        assert_eq!(k.len(), 3 + 43);
    }

    #[test]
    fn public_id_from_token_extracts_first_8_chars() {
        let k = mint_plaintext_key(); // hf_XXXXXXXXXX...
        let pub_id = public_id_from_token(&k).expect("should extract");
        assert_eq!(pub_id.len(), 8);
        assert_eq!(pub_id, &k[3..11]);
    }

    #[test]
    fn public_id_from_token_rejects_malformed() {
        assert!(public_id_from_token("").is_none());
        assert!(public_id_from_token("hf_short").is_none()); // "short" = 5 chars < 8
        assert!(public_id_from_token("hf_12345678rest").is_some()); // 8+ chars → ok
        assert!(public_id_from_token("not-hf-prefix").is_none());
    }

    #[test]
    fn scope_db_round_trip() {
        for s in [
            ApiKeyScope::ToolsInvoke,
            ApiKeyScope::Orchestrate,
            ApiKeyScope::ReadUsage,
        ] {
            assert_eq!(ApiKeyScope::from_db_str(s.as_db_str()), Some(s));
        }
        assert_eq!(ApiKeyScope::from_db_str("nonsense"), None);
    }
}
