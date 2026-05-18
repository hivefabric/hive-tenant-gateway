//! Postgres-backed [`super::TenantStore`].
//!
//! Schema lives in `migrations/`. Apply with [`PgTenantStore::migrate`].
//!
//! Today this resolves bearer tokens by argon2-verifying every unrevoked key
//! (O(N) per lookup). Acceptable for the first hundred tenants; tracked as a
//! Phase 2.3 perf item to split bearer tokens into `{public_id, secret}` so
//! resolve becomes a single indexed row lookup + one argon2 verify.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use uuid::Uuid;

use crate::budget::BudgetDefaults;
use crate::error::{GatewayError, GatewayResult};

use super::{
    hash_key, mint_plaintext_key, verify_key, ApiKey, ApiKeyMint, ApiKeyScope, Tenant, TenantStore,
};

#[derive(Clone)]
pub struct PgTenantStore {
    pool: PgPool,
}

impl PgTenantStore {
    /// Connect to the configured Postgres URL.
    pub async fn connect(database_url: &str) -> GatewayResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(|e| GatewayError::Internal(format!("postgres connect: {e}")))?;
        Ok(Self { pool })
    }

    /// Apply schema migrations from `migrations/`.
    pub async fn migrate(&self) -> GatewayResult<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| GatewayError::Internal(format!("migrate: {e}")))?;
        Ok(())
    }
}

fn pg_err(ctx: &str, e: sqlx::Error) -> GatewayError {
    GatewayError::Internal(format!("{ctx}: {e}"))
}

fn row_to_tenant(row: &sqlx::postgres::PgRow) -> Tenant {
    Tenant {
        id: row.get("id"),
        name: row.get("name"),
        plan: row.get("plan"),
        created_at: row.get::<DateTime<Utc>, _>("created_at"),
        budget_defaults: BudgetDefaults {
            max_credits_per_call: row.get::<i64, _>("budget_default_credits") as u64,
            ttl_secs: row.get::<i64, _>("budget_default_ttl_secs") as u64,
        },
    }
}

fn row_to_api_key(row: &sqlx::postgres::PgRow) -> ApiKey {
    let scopes_db: Vec<String> = row.get("scopes");
    let scopes = scopes_db
        .iter()
        .filter_map(|s| ApiKeyScope::from_db_str(s))
        .collect();
    ApiKey {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        key_hash: row.get("key_hash"),
        scopes,
        created_at: row.get::<DateTime<Utc>, _>("created_at"),
        last_used_at: row.get::<Option<DateTime<Utc>>, _>("last_used_at"),
        revoked_at: row.get::<Option<DateTime<Utc>>, _>("revoked_at"),
    }
}

#[async_trait]
impl TenantStore for PgTenantStore {
    async fn create_tenant(
        &self,
        name: String,
        plan: Option<String>,
        budgets: Option<BudgetDefaults>,
    ) -> GatewayResult<Tenant> {
        let id = Uuid::new_v4();
        let plan = plan.unwrap_or_else(|| "free".to_string());
        let budgets = budgets.unwrap_or_default();
        let row = sqlx::query(
            r#"
            INSERT INTO tenants (id, name, plan, budget_default_credits, budget_default_ttl_secs)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, name, plan, created_at, budget_default_credits, budget_default_ttl_secs
            "#,
        )
        .bind(id)
        .bind(name)
        .bind(plan)
        .bind(budgets.max_credits_per_call as i64)
        .bind(budgets.ttl_secs as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| pg_err("create_tenant", e))?;
        Ok(row_to_tenant(&row))
    }

    async fn get_tenant(&self, id: Uuid) -> GatewayResult<Tenant> {
        let row = sqlx::query(
            r#"
            SELECT id, name, plan, created_at, budget_default_credits, budget_default_ttl_secs
            FROM tenants WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| pg_err("get_tenant", e))?;
        row.as_ref()
            .map(row_to_tenant)
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
        let id = Uuid::new_v4();
        let scope_strs: Vec<String> = scopes.iter().map(|s| s.as_db_str().to_string()).collect();

        let row = sqlx::query(
            r#"
            INSERT INTO tenant_api_keys (id, tenant_id, key_hash, scopes)
            VALUES ($1, $2, $3, $4)
            RETURNING id, tenant_id, key_hash, scopes, created_at, last_used_at, revoked_at
            "#,
        )
        .bind(id)
        .bind(tenant_id)
        .bind(key_hash)
        .bind(&scope_strs)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| pg_err("mint_api_key", e))?;

        Ok(ApiKeyMint {
            api_key: row_to_api_key(&row),
            plaintext,
        })
    }

    async fn resolve_api_key(&self, plaintext: &str) -> GatewayResult<(Tenant, ApiKey)> {
        if !plaintext.starts_with("hf_") {
            return Err(GatewayError::InvalidApiKey);
        }
        let candidates = sqlx::query(
            r#"
            SELECT id, tenant_id, key_hash, scopes, created_at, last_used_at, revoked_at
            FROM tenant_api_keys
            WHERE revoked_at IS NULL
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| pg_err("resolve_api_key.candidates", e))?;

        for row in candidates {
            let key_hash: String = row.get("key_hash");
            if !verify_key(plaintext, &key_hash) {
                continue;
            }
            let key = row_to_api_key(&row);
            let tenant = self.get_tenant(key.tenant_id).await?;
            let now = Utc::now();
            let _ = sqlx::query(
                r#"UPDATE tenant_api_keys SET last_used_at = $1 WHERE id = $2"#,
            )
            .bind(now)
            .bind(key.id)
            .execute(&self.pool)
            .await;
            let mut returned = key;
            returned.last_used_at = Some(now);
            return Ok((tenant, returned));
        }
        Err(GatewayError::InvalidApiKey)
    }

    async fn revoke_api_key(&self, key_id: Uuid) -> GatewayResult<()> {
        let res = sqlx::query(
            r#"UPDATE tenant_api_keys SET revoked_at = NOW() WHERE id = $1 AND revoked_at IS NULL"#,
        )
        .bind(key_id)
        .execute(&self.pool)
        .await
        .map_err(|e| pg_err("revoke_api_key", e))?;
        if res.rows_affected() == 0 {
            return Err(GatewayError::TenantNotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Postgres integration tests are gated by env var so `cargo test`
    //! defaults stay offline. To run:
    //!
    //!     export DATABASE_URL_TEST=postgres://...
    //!     cargo test -p hive-tenant-gateway -- --ignored
    //!
    //! Each test uses a fresh schema namespace via uuid-suffixed table names
    //! is overkill for now; the simpler convention is: drop and re-migrate
    //! against a dedicated test DB.

    use super::*;

    async fn pool_or_skip() -> Option<PgTenantStore> {
        let url = std::env::var("DATABASE_URL_TEST").ok()?;
        let store = PgTenantStore::connect(&url).await.ok()?;
        store.migrate().await.ok()?;
        // Wipe state so tests are deterministic.
        let _ = sqlx::query("TRUNCATE tenant_api_keys, tenants RESTART IDENTITY CASCADE")
            .execute(&store.pool)
            .await;
        Some(store)
    }

    #[tokio::test]
    #[ignore]
    async fn pg_tenant_round_trip() {
        let Some(store) = pool_or_skip().await else {
            return;
        };
        let t = store
            .create_tenant("acme".into(), None, None)
            .await
            .unwrap();
        let fetched = store.get_tenant(t.id).await.unwrap();
        assert_eq!(fetched.id, t.id);
        assert_eq!(fetched.plan, "free");
    }

    #[tokio::test]
    #[ignore]
    async fn pg_mint_resolve_revoke_cycle() {
        let Some(store) = pool_or_skip().await else {
            return;
        };
        let t = store
            .create_tenant("acme".into(), None, None)
            .await
            .unwrap();
        let mint = store
            .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke, ApiKeyScope::Orchestrate])
            .await
            .unwrap();
        let (tenant, key) = store.resolve_api_key(&mint.plaintext).await.unwrap();
        assert_eq!(tenant.id, t.id);
        assert_eq!(key.scopes.len(), 2);
        assert!(key.last_used_at.is_some());

        store.revoke_api_key(mint.api_key.id).await.unwrap();
        let err = store.resolve_api_key(&mint.plaintext).await.unwrap_err();
        assert!(matches!(err, GatewayError::InvalidApiKey));
    }
}
