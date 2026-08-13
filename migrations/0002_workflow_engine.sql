CREATE TABLE runs (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    definition_id UUID NOT NULL REFERENCES workflow_definitions(id),
    start_idempotency_key TEXT NOT NULL,
    input JSONB NOT NULL,
    input_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'completed', 'failed', 'cancelled')),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    output JSONB,
    failure JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (session_id, start_idempotency_key)
);

CREATE TABLE run_tool_bindings (
    run_id UUID NOT NULL REFERENCES runs(id),
    qualified_name TEXT NOT NULL,
    server_alias TEXT NOT NULL,
    catalog_epoch_id UUID NOT NULL REFERENCES mcp_catalog_epochs(epoch_id),
    descriptor_digest TEXT NOT NULL,
    capabilities JSONB NOT NULL,
    recovery_profile TEXT NOT NULL,
    PRIMARY KEY (run_id, qualified_name),
    UNIQUE (run_id, qualified_name, descriptor_digest)
);

CREATE TABLE frames (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id),
    parent_frame_id UUID REFERENCES frames(id),
    task_name TEXT NOT NULL,
    branch_index INTEGER NOT NULL DEFAULT 0 CHECK (branch_index >= 0),
    state TEXT NOT NULL CHECK (state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'completed', 'failed', 'cancelled')),
    bindings JSONB NOT NULL,
    result JSONB,
    failure JSONB,
    suspension_id UUID,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE model_items (
    frame_id UUID NOT NULL REFERENCES frames(id),
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    position INTEGER NOT NULL CHECK (position >= 0),
    direction TEXT NOT NULL CHECK (direction IN ('input', 'output')),
    item JSONB NOT NULL,
    PRIMARY KEY (frame_id, attempt, position, direction)
);

CREATE TABLE tool_calls (
    id UUID PRIMARY KEY,
    frame_id UUID NOT NULL REFERENCES frames(id),
    provider_call_id TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    descriptor_digest TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    arguments JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('intent', 'running', 'completed', 'failed', 'waiting_for_permission')),
    result JSONB,
    error JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (frame_id, provider_call_id),
    UNIQUE (frame_id, idempotency_key)
);

CREATE TABLE permission_requests (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id),
    frame_id UUID NOT NULL REFERENCES frames(id),
    provider_call_id TEXT NOT NULL,
    qualified_tool TEXT NOT NULL,
    descriptor_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    policy_revision BIGINT NOT NULL CHECK (policy_revision >= 0),
    state TEXT NOT NULL CHECK (state IN ('pending', 'resolved', 'cancelled')),
    resolution JSONB,
    resolution_idempotency_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolved_at TIMESTAMPTZ,
    UNIQUE (frame_id, provider_call_id),
    UNIQUE (id, resolution_idempotency_key)
);

CREATE TABLE events (
    run_id UUID NOT NULL REFERENCES runs(id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL,
    metadata JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (run_id, sequence)
);
