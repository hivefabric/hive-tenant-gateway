//! In-memory TenantStore — dev/local/tests.
//!
//! Loses everything on restart. Resolves bearer tokens by argon2-verifying
//! every unrevoked key (O(N)); fine for the handful of tenants a dev session
//! creates. Postgres-backed [`super::pg::PgTenantStore`] is the production
//! path.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::Utc;
use uuid::Uuid;

use crate::budget::BudgetDefaults;
use crate::error::{GatewayError, GatewayResult};

use super::{
    hash_key, mint_plaintext_key, verify_key, ApiKey, ApiKeyMint, ApiKeyScope, LlmProvider,
    NewLlmProvider, Tenant, TenantStore,
};

pub struct InMemoryTenantStore {
    inner: RwLock<Inner>,
}

struct Inner {
    tenants: HashMap<Uuid, Tenant>,
    keys: HashMap<Uuid, ApiKey>,
    llm_providers: HashMap<Uuid, (LlmProvider, String)>, // (provider, api_key_enc)
}

impl InMemoryTenantStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                tenants: HashMap::new(),
                keys: HashMap::new(),
                llm_providers: HashMap::new(),
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
            default_sensitivity: None,
            jurisdiction_required: Vec::new(),
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

    async fn store_llm_provider(
        &self,
        tenant_id: Uuid,
        input: NewLlmProvider,
        api_key_enc: String,
    ) -> GatewayResult<LlmProvider> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        if input.is_default {
            for (p, _) in g.llm_providers.values_mut() {
                if p.tenant_id == tenant_id {
                    p.is_default = false;
                }
            }
        }
        let id = Uuid::new_v4();
        let provider = LlmProvider {
            id,
            tenant_id,
            name: input.name,
            provider: input.provider,
            model: input.model,
            base_url: input.base_url,
            is_default: input.is_default,
            created_at: Utc::now(),
        };
        g.llm_providers.insert(id, (provider.clone(), api_key_enc));
        Ok(provider)
    }

    async fn list_llm_providers(&self, tenant_id: Uuid) -> GatewayResult<Vec<LlmProvider>> {
        let g = self
            .inner
            .read()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        Ok(g.llm_providers
            .values()
            .filter(|(p, _)| p.tenant_id == tenant_id)
            .map(|(p, _)| p.clone())
            .collect())
    }

    async fn get_llm_provider(
        &self,
        tenant_id: Uuid,
        provider_id: Uuid,
    ) -> GatewayResult<Option<(LlmProvider, String)>> {
        let g = self
            .inner
            .read()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        Ok(g.llm_providers.get(&provider_id).and_then(|(p, enc)| {
            if p.tenant_id == tenant_id {
                Some((p.clone(), enc.clone()))
            } else {
                None
            }
        }))
    }

    async fn get_default_llm_provider(
        &self,
        tenant_id: Uuid,
    ) -> GatewayResult<Option<(LlmProvider, String)>> {
        let g = self
            .inner
            .read()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        Ok(g.llm_providers
            .values()
            .find(|(p, _)| p.tenant_id == tenant_id && p.is_default)
            .map(|(p, enc)| (p.clone(), enc.clone())))
    }

    async fn delete_llm_provider(&self, tenant_id: Uuid, provider_id: Uuid) -> GatewayResult<()> {
        let mut g = self
            .inner
            .write()
            .map_err(|_| GatewayError::Internal("tenant store poisoned".into()))?;
        g.llm_providers
            .retain(|id, (p, _)| !(*id == provider_id && p.tenant_id == tenant_id));
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
}
