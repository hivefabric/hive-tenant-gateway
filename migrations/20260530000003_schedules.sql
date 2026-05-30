CREATE TABLE IF NOT EXISTS schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    title TEXT NOT NULL DEFAULT 'Scheduled task',
    cron TEXT NOT NULL,                    -- 5-field cron: "0 9 * * *"
    task_payload JSONB NOT NULL DEFAULT '{}', -- JSON sent to run_subagent: {prompt, capability_urn?}
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    next_run_at TIMESTAMPTZ,
    last_run_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_schedules_tenant ON schedules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_schedules_next_run ON schedules(next_run_at) WHERE enabled = TRUE;
