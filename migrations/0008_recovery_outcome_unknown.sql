ALTER TABLE runs DROP CONSTRAINT runs_state_check;
ALTER TABLE runs ADD CONSTRAINT runs_state_check CHECK (state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'outcome_unknown', 'completed', 'failed', 'cancelled'));

ALTER TABLE frames DROP CONSTRAINT frames_state_check;
ALTER TABLE frames ADD CONSTRAINT frames_state_check CHECK (state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'outcome_unknown', 'completed', 'failed', 'cancelled'));

ALTER TABLE tool_calls DROP CONSTRAINT tool_calls_state_check;
ALTER TABLE tool_calls ADD CONSTRAINT tool_calls_state_check CHECK (state IN ('intent', 'running', 'completed', 'failed', 'waiting_for_permission', 'outcome_unknown'));

ALTER TABLE tool_calls ADD COLUMN recovery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (recovery_attempts >= 0);
