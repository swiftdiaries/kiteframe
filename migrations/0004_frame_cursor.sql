ALTER TABLE frames
    ADD COLUMN outcome_name TEXT,
    ADD COLUMN statement_index INTEGER NOT NULL DEFAULT 0 CHECK (statement_index >= 0);
