-- HiveFabric Tenant Gateway — initial schema.
--
-- Two tables: `tenants` and `tenant_api_keys`. The Phase 2.2
-- `tenant_llm_providers` table lands when we add the per-tenant LLM provider
-- registry; today customers send their LLM API key in each /v1/orchestrate
-- request body.

CREATE TABLE IF NOT EXISTS tenants (
    id                       UUID         PRIMARY KEY,
    name                     TEXT         NOT NULL,
    plan                     TEXT         NOT NULL DEFAULT 'free',
    created_at               TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    -- Defaults applied when a request omits a budget.
    budget_default_credits   BIGINT       NOT NULL DEFAULT 1000,
    budget_default_ttl_secs  BIGINT       NOT NULL DEFAULT 60
);

CREATE TABLE IF NOT EXISTS tenant_api_keys (
    id            UUID         PRIMARY KEY,
    tenant_id     UUID         NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- Argon2 PHC hash of the plaintext key. Plaintext is shown to the caller
    -- exactly once at mint time and never persisted.
    key_hash      TEXT         NOT NULL,
    -- Scopes are stored as a TEXT[] of stable snake_case identifiers
    -- (`tools_invoke`, `orchestrate`, `read_usage`). Translation lives in
    -- `ApiKeyScope::{as_db_str, from_db_str}`.
    scopes        TEXT[]       NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    last_used_at  TIMESTAMPTZ,
    revoked_at    TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_tenant_api_keys_tenant_id ON tenant_api_keys (tenant_id);

-- TODO (Phase 2.3): split bearer tokens into {public-id, secret} so resolve
-- becomes a one-row index lookup + one argon2 verify, instead of an O(N) scan.
-- Tracked in docs/02_architecture/18_tenant_gateway.md "Phase 2.3 perf
-- optimisations".
