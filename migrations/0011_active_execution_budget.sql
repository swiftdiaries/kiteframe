ALTER TABLE runs
    ADD COLUMN active_execution_millis BIGINT NOT NULL DEFAULT 0
        CHECK (active_execution_millis >= 0),
    ADD COLUMN active_execution_started_at TIMESTAMPTZ,
    ADD COLUMN active_execution_count INTEGER NOT NULL DEFAULT 0
        CHECK (active_execution_count >= 0),
    ADD CONSTRAINT runs_active_execution_lease_check CHECK (
        (active_execution_count = 0 AND active_execution_started_at IS NULL)
        OR (active_execution_count > 0 AND active_execution_started_at IS NOT NULL)
    );
