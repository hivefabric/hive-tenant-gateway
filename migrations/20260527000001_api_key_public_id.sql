-- Phase 2.3: split bearer token lookup into O(1) index scan + single Argon2 verify.
--
-- The first 8 characters of a tenant API key after the "hf_" prefix are stored
-- as a public_id column with a unique index. resolve_api_key now:
--   1. SELECT WHERE public_id = token[3:11]   → O(1), at most 1 row
--   2. Argon2 verify that single row           → O(1) cryptographic work
--
-- Old keys (public_id IS NULL) fall back to the legacy O(N) scan until they
-- are rotated or revoked. New keys issued after this migration are always O(1).

ALTER TABLE tenant_api_keys ADD COLUMN IF NOT EXISTS public_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_tenant_api_keys_public_id
    ON tenant_api_keys (public_id)
    WHERE public_id IS NOT NULL;
