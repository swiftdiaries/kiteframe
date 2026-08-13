ALTER TABLE frames DROP CONSTRAINT frames_state_check;
ALTER TABLE frames
    ADD CONSTRAINT frames_state_check CHECK (state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'waiting_for_children', 'completed', 'failed', 'cancelled'));
ALTER TABLE frames ADD COLUMN parent_binding TEXT;
