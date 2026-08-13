ALTER TABLE runs
    ADD COLUMN cancellation_idempotency_key TEXT,
    ADD COLUMN cancellation_reason JSONB;

CREATE UNIQUE INDEX runs_cancellation_idempotency_key
    ON runs (id, cancellation_idempotency_key)
    WHERE cancellation_idempotency_key IS NOT NULL;
