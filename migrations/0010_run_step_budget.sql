ALTER TABLE runs
    ADD COLUMN step_count BIGINT NOT NULL DEFAULT 0 CHECK (step_count >= 0);
