//! Tenant + API key data model and store trait.
//!
//! Phase 2.0 ships an in-memory store for dev/local. Phase 2.1 swaps in a
//! Postgres-backed implementation behind the same trait.

use std::collections::HashMap;
use std::sync::RwLock;

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

/// Per-key scope. We start narrow and widen by demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyScope {
    /// Call `/v1/mcp/tools/{list,call}`.
    ToolsInvoke,
    /// Call `/v1/orchestrate` (Phase 2.1).
    Orchestrate,
    /// Read usage reports (Phase 2.2 — needs Ledger).
    ReadUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub plan: String,
    pub created_at: DateTime<Utc>,
    pub budget_defaults: BudgetDefaults,
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
    /// Returns `Unauthorized` for any failure mode (timing-safe by design —
    /// the caller cannot distinguish "no such key" from "wrong key").
    async fn resolve_api_key(&self, plaintext: &str) -> GatewayResult<(Tenant, ApiKey)>;

    async fn revoke_api_key(&self, key_id: Uuid) -> GatewayResult<()>;
}

/// Generate a fresh plaintext key in the `hf_<32-byte-base64url>` form.
/// Prefix is intentional — leaked keys grep more easily than opaque tokens.
pub fn mint_plaintext_key() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("hf_{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_key(plaintext: &str) -> GatewayResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| GatewayError::Internal(format!("argon2 hash: {e}")))
}

fn verify_key(plaintext: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok()
}

/// Default in-memory implementation. Swap for Postgres in Phase 2.1.
pub struct InMemoryTenantStore {
    inner: RwLock<Inner>,
}

struct Inner {
    tenants: HashMap<Uuid, Tenant>,
    keys: HashMap<Uuid, ApiKey>,
}

impl InMemoryTenantStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                tenants: HashMap::new(),
                keys: HashMap::new(),
            }),
        }
    }
}

impl Default for InMemoryTenantStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TenantStore for InMemoryTenantStore {
    async fn create_tenant(
        &self,
        name: String,
        plan: Option<String>,
        budgets: Option<BudgetDefaults>,
    ) -> GatewayResult<Tenant> {
        let tenant = Tenant {
            id: Uuid::new_v4(),
            name,
            plan: plan.unwrap_or_else(|| "free".to_string()),
            created_at: Utc::now(),
            budget_defaults: budgets.unwrap_or_default(),
        };
        let mut g = self
            .inner
            .write()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        g.tenants.insert(tenant.id, tenant.clone());
        Ok(tenant)
    }

    async fn get_tenant(&self, id: Uuid) -> GatewayResult<Tenant> {
        let g = self
            .inner
            .read()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        g.tenants
            .get(&id)
            .cloned()
            .ok_or(GatewayError::TenantNotFound)
    }

    async fn mint_api_key(
        &self,
        tenant_id: Uuid,
        scopes: Vec<ApiKeyScope>,
    ) -> GatewayResult<ApiKeyMint> {
        // Confirm tenant exists.
        let _ = self.get_tenant(tenant_id).await?;
        let plaintext = mint_plaintext_key();
        let key_hash = hash_key(&plaintext)?;
        let api_key = ApiKey {
            id: Uuid::new_v4(),
            tenant_id,
            key_hash,
            scopes,
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };
        let mut g = self
            .inner
            .write()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        g.keys.insert(api_key.id, api_key.clone());
        Ok(ApiKeyMint { api_key, plaintext })
    }

    async fn resolve_api_key(&self, plaintext: &str) -> GatewayResult<(Tenant, ApiKey)> {
        if !plaintext.starts_with("hf_") {
            return Err(GatewayError::InvalidApiKey);
        }
        // O(N) on the key set. Phase 2.1 (Postgres): index by a separate
        // deterministic prefix-derived shard so we don't argon2-verify every key.
        let snapshot: Vec<ApiKey> = {
            let g = self
                .inner
                .read()
                .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
            g.keys.values().cloned().collect()
        };
        for key in snapshot {
            if key.revoked_at.is_some() {
                continue;
            }
            if verify_key(plaintext, &key.key_hash) {
                let tenant = self.get_tenant(key.tenant_id).await?;
                let now = Utc::now();
                if let Ok(mut g) = self.inner.write() {
                    if let Some(k) = g.keys.get_mut(&key.id) {
                        k.last_used_at = Some(now);
                    }
                }
                let mut returned = key;
                returned.last_used_at = Some(now);
                return Ok((tenant, returned));
            }
        }
        Err(GatewayError::InvalidApiKey)
    }

    async fn revoke_api_key(&self, key_id: Uuid) -> GatewayResult<()> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        let key = g.keys.get_mut(&key_id).ok_or(GatewayError::TenantNotFound)?;
        key.revoked_at = Some(Utc::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_tenant_then_resolve_key_round_trip() {
        let store = InMemoryTenantStore::new();
        let t = store
            .create_tenant("acme".into(), None, None)
            .await
            .unwrap();
        let mint = store
            .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke])
            .await
            .unwrap();
        assert!(mint.plaintext.starts_with("hf_"));
        let (tenant, key) = store.resolve_api_key(&mint.plaintext).await.unwrap();
        assert_eq!(tenant.id, t.id);
        assert_eq!(key.scopes, vec![ApiKeyScope::ToolsInvoke]);
        assert!(key.last_used_at.is_some());
    }

    #[tokio::test]
    async fn revoked_key_is_rejected() {
        let store = InMemoryTenantStore::new();
        let t = store
            .create_tenant("acme".into(), None, None)
            .await
            .unwrap();
        let mint = store
            .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke])
            .await
            .unwrap();
        store.revoke_api_key(mint.api_key.id).await.unwrap();
        let err = store.resolve_api_key(&mint.plaintext).await.unwrap_err();
        assert!(matches!(err, GatewayError::InvalidApiKey));
    }

    #[tokio::test]
    async fn unknown_key_is_rejected() {
        let store = InMemoryTenantStore::new();
        let err = store
            .resolve_api_key("hf_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .await
            .unwrap_err();
        assert!(matches!(err, GatewayError::InvalidApiKey));
    }

    #[tokio::test]
    async fn malformed_key_is_rejected_without_a_hash_check() {
        let store = InMemoryTenantStore::new();
        let err = store.resolve_api_key("totally-not-an-hf-key").await.unwrap_err();
        assert!(matches!(err, GatewayError::InvalidApiKey));
    }

    #[test]
    fn mint_plaintext_key_has_correct_shape() {
        let k = mint_plaintext_key();
        assert!(k.starts_with("hf_"));
        // 32 bytes -> 43 base64url-no-pad chars.
        assert_eq!(k.len(), 3 + 43);
    }
}
