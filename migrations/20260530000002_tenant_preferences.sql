-- Persist tenant preferences so they survive gateway restarts.
-- Stored as a JSONB blob matching TenantPreferences serde shape.
ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS preferences JSONB NOT NULL DEFAULT '{}';
