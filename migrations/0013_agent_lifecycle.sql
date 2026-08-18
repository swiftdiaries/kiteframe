DROP TRIGGER user_tasks_reject_update ON user_tasks;

CREATE FUNCTION reject_user_task_contract_mutation() RETURNS trigger AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
        OR NEW.session_id IS DISTINCT FROM OLD.session_id
        OR NEW.principal IS DISTINCT FROM OLD.principal
        OR NEW.definition_id IS DISTINCT FROM OLD.definition_id
        OR NEW.submission_idempotency_key IS DISTINCT FROM OLD.submission_idempotency_key
        OR NEW.request_digest IS DISTINCT FROM OLD.request_digest
        OR NEW.target IS DISTINCT FROM OLD.target
        OR NEW.preferences IS DISTINCT FROM OLD.preferences
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'user task contract is immutable' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER user_tasks_reject_contract_update
    BEFORE UPDATE ON user_tasks
    FOR EACH ROW EXECUTE FUNCTION reject_user_task_contract_mutation();

ALTER TABLE user_tasks DROP CONSTRAINT user_tasks_state_check;
ALTER TABLE user_tasks DROP CONSTRAINT user_tasks_revision_check;
ALTER TABLE user_tasks
    ADD CONSTRAINT user_tasks_state_check
        CHECK (state IN ('accepted', 'running', 'succeeded', 'failed', 'cancelled')),
    ADD CONSTRAINT user_tasks_revision_check CHECK (revision >= 0),
    ADD COLUMN operational_status TEXT NOT NULL DEFAULT 'not_started'
        CHECK (operational_status IN (
            'not_started', 'runnable', 'waiting_for_context', 'waiting_for_event',
            'waiting_for_harness', 'waiting_for_approval', 'paused', 'blocked',
            'finished'
        )),
    ADD COLUMN failure JSONB,
    ADD COLUMN cancellation_reason TEXT,
    ADD COLUMN started_at TIMESTAMPTZ,
    ADD COLUMN finished_at TIMESTAMPTZ;

CREATE TABLE goal_states (
    task_id UUID PRIMARY KEY REFERENCES user_tasks(id),
    phase TEXT NOT NULL DEFAULT 'active'
        CHECK (phase IN ('active', 'paused', 'blocked', 'complete')),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    current_blocker_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO goal_states (task_id)
SELECT id FROM user_tasks;

CREATE TABLE agent_runtime_snapshots (
    task_id UUID PRIMARY KEY REFERENCES user_tasks(id),
    task_revision BIGINT NOT NULL CHECK (task_revision > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    runtime_id TEXT NOT NULL CHECK (runtime_id = 'kite.v1'),
    disposition TEXT NOT NULL
        CHECK (disposition IN ('running', 'returned', 'blocked', 'terminating')),
    payload JSONB NOT NULL,
    ceiling_bytes INTEGER NOT NULL CHECK (ceiling_bytes BETWEEN 1 AND 262144),
    canonical_bytes INTEGER NOT NULL CHECK (canonical_bytes BETWEEN 1 AND 262144),
    snapshot_digest TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE agent_transitions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    source_event_id TEXT,
    expected_task_revision BIGINT NOT NULL CHECK (expected_task_revision >= 0),
    expected_goal_revision BIGINT NOT NULL CHECK (expected_goal_revision >= 0),
    committed_task_revision BIGINT NOT NULL CHECK (committed_task_revision > 0),
    committed_goal_revision BIGINT NOT NULL CHECK (committed_goal_revision >= 0),
    snapshot_digest TEXT NOT NULL,
    command_digests JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, idempotency_key),
    UNIQUE (task_id, id),
    UNIQUE (task_id, committed_task_revision)
);

CREATE TABLE agent_commands (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    transition_id UUID NOT NULL REFERENCES agent_transitions(id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 63),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    runtime_key TEXT NOT NULL,
    command_kind TEXT NOT NULL CHECK (command_kind IN (
        'request_context', 'wait_for_event', 'invoke_harness',
        'request_approval', 'complete_goal', 'block_goal', 'fail_task'
    )),
    payload JSONB NOT NULL,
    payload_digest TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'claimed', 'completed', 'failed', 'cancelled')),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    claimant TEXT,
    claim_idempotency_key TEXT,
    claim_request_digest TEXT,
    completion_idempotency_key TEXT,
    completion_request_digest TEXT,
    result JSONB,
    failure JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (transition_id, ordinal),
    UNIQUE (task_id, id),
    UNIQUE (task_id, runtime_key)
);

CREATE TABLE agent_policy_decisions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    transition_id UUID NOT NULL UNIQUE REFERENCES agent_transitions(id),
    action_kind TEXT NOT NULL
        CHECK (action_kind IN ('start', 'complete_goal', 'block_goal')),
    allowed BOOLEAN NOT NULL CHECK (allowed),
    policy_revision BIGINT NOT NULL CHECK (policy_revision >= 0),
    reason_code TEXT NOT NULL
        CHECK (reason_code ~ '^[a-z][a-z0-9_]{0,63}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (task_id, transition_id)
        REFERENCES agent_transitions(task_id, id)
);

CREATE TABLE goal_evidence (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    evidence_type TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind = 'builtin_harness'),
    source_identity UUID NOT NULL,
    content_digest TEXT NOT NULL,
    UNIQUE (task_id, id),
    FOREIGN KEY (task_id, source_identity)
        REFERENCES agent_commands(task_id, id)
);

CREATE TABLE goal_criterion_assertions (
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    criterion_id TEXT NOT NULL,
    evidence_id UUID NOT NULL,
    goal_revision BIGINT NOT NULL CHECK (goal_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (task_id, criterion_id),
    FOREIGN KEY (task_id, evidence_id)
        REFERENCES goal_evidence(task_id, id)
);

CREATE TABLE goal_blockers (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    blocked_when_index INTEGER NOT NULL CHECK (blocked_when_index BETWEEN 0 AND 65535),
    reason_code TEXT NOT NULL,
    explanation TEXT NOT NULL,
    evidence_ids JSONB NOT NULL,
    unlock_conditions JSONB NOT NULL,
    goal_revision BIGINT NOT NULL CHECK (goal_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, id),
    UNIQUE (task_id, goal_revision)
);

ALTER TABLE goal_states
    ADD CONSTRAINT goal_states_current_blocker_fk
    FOREIGN KEY (task_id, current_blocker_id)
    REFERENCES goal_blockers(task_id, id);

CREATE TABLE agent_task_actions (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    action_kind TEXT NOT NULL CHECK (action_kind IN ('start', 'pause', 'resume', 'cancel')),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    result_task_revision BIGINT NOT NULL CHECK (result_task_revision >= 0),
    result_goal_revision BIGINT NOT NULL CHECK (result_goal_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, action_kind, idempotency_key)
);

CREATE TABLE agent_task_events (
    task_id UUID NOT NULL REFERENCES user_tasks(id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL CHECK (kind IN (
        'agent.started', 'command.claimed', 'runtime.transition_committed',
        'runtime.command_unavailable', 'goal.completed', 'goal.blocked',
        'task.paused', 'task.resumed', 'task.cancelled', 'task.failed',
        'task.succeeded'
    )),
    metadata JSONB NOT NULL,
    task_revision BIGINT NOT NULL CHECK (task_revision >= 0),
    goal_revision BIGINT NOT NULL CHECK (goal_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (task_id, sequence)
);

CREATE FUNCTION reject_agent_history_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'agent history is immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER agent_transitions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_transitions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_policy_decisions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_policy_decisions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_evidence_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_evidence
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_assertions_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_criterion_assertions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER goal_blockers_reject_update_delete
    BEFORE UPDATE OR DELETE ON goal_blockers
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_actions_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_task_actions
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
CREATE TRIGGER agent_events_reject_update_delete
    BEFORE UPDATE OR DELETE ON agent_task_events
    FOR EACH ROW EXECUTE FUNCTION reject_agent_history_mutation();
