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
}

/// Generate a fresh plaintext key in the `hf_<32-byte-base64url>` form.
pub fn mint_plaintext_key() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("hf_{}", URL_SAFE_NO_PAD.encode(bytes))
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
