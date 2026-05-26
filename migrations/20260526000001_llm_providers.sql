-- Tenant LLM provider registry.
-- Moves LLM API keys from per-request bodies into server-side encrypted storage.
-- Resolves risk R-S1: keys no longer travel in HTTP request bodies.
--
-- api_key_enc format:
--   enc:{nonce_b64url}.{ciphertext_b64url}  — AES-256-GCM (production)
--   raw:{plaintext}                          — unencrypted (dev mode, no TENANT_LLM_SECRET_KEY)

CREATE TABLE IF NOT EXISTS tenant_llm_providers (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID        NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    name        TEXT        NOT NULL,
    provider    TEXT        NOT NULL,  -- 'anthropic' | 'openai'
    model       TEXT        NOT NULL,
    api_key_enc TEXT        NOT NULL,  -- encrypted; never returned in API responses
    base_url    TEXT,
    is_default  BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_llm_providers_tenant ON tenant_llm_providers (tenant_id);

-- Enforce at most one default provider per tenant.
CREATE UNIQUE INDEX IF NOT EXISTS idx_llm_providers_default
    ON tenant_llm_providers (tenant_id)
    WHERE is_default = TRUE;
