ALTER TABLE workflow_definitions
    ADD COLUMN definition_kind TEXT NOT NULL DEFAULT 'workflow'
        CHECK (definition_kind IN ('workflow', 'agent'));

CREATE TABLE user_tasks (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    principal TEXT NOT NULL,
    definition_id UUID NOT NULL REFERENCES workflow_definitions(id),
    submission_idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    target JSONB NOT NULL,
    preferences JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'accepted' CHECK (state = 'accepted'),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision = 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (session_id, submission_idempotency_key)
);

CREATE TABLE goal_contracts (
    id UUID PRIMARY KEY,
    task_id UUID NOT NULL UNIQUE REFERENCES user_tasks(id),
    contract_version INTEGER NOT NULL CHECK (contract_version = 1),
    objective TEXT NOT NULL,
    success_criteria JSONB NOT NULL,
    constraints JSONB NOT NULL,
    blocked_when JSONB NOT NULL,
    contract_digest TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (task_id, contract_digest)
);

CREATE FUNCTION reject_user_task_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'user tasks are immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER user_tasks_reject_update
    BEFORE UPDATE ON user_tasks
    FOR EACH ROW EXECUTE FUNCTION reject_user_task_mutation();

CREATE TRIGGER user_tasks_reject_delete
    BEFORE DELETE ON user_tasks
    FOR EACH ROW EXECUTE FUNCTION reject_user_task_mutation();

CREATE FUNCTION reject_goal_contract_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'goal contracts are immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER goal_contracts_reject_update
    BEFORE UPDATE ON goal_contracts
    FOR EACH ROW EXECUTE FUNCTION reject_goal_contract_mutation();

CREATE TRIGGER goal_contracts_reject_delete
    BEFORE DELETE ON goal_contracts
    FOR EACH ROW EXECUTE FUNCTION reject_goal_contract_mutation();
