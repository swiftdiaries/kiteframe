CREATE TABLE questions (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id),
    frame_id UUID NOT NULL REFERENCES frames(id),
    binding TEXT NOT NULL,
    text TEXT NOT NULL,
    schema JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'answered', 'cancelled')),
    answer JSONB,
    answer_idempotency_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    answered_at TIMESTAMPTZ,
    UNIQUE (frame_id, id),
    UNIQUE (id, answer_idempotency_key)
);
