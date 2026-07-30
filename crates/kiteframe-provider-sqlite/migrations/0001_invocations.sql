CREATE TABLE invocations (
    invocation_id TEXT PRIMARY KEY NOT NULL,
    status_id TEXT NOT NULL UNIQUE,
    actor_ref TEXT NOT NULL,
    capability_name TEXT NOT NULL,
    capability_version TEXT NOT NULL,
    normalized_resource TEXT NOT NULL,
    semantic_operation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    state_kind TEXT NOT NULL,
    state_json TEXT NOT NULL,
    admission_id TEXT NOT NULL,
    grant_digest TEXT NOT NULL,
    catalog_name TEXT NOT NULL,
    catalog_revision TEXT NOT NULL,
    catalog_digest TEXT NOT NULL,
    descriptor_digest TEXT NOT NULL,
    authority_revision_digest TEXT NOT NULL,
    tenant_ref TEXT NOT NULL,
    human_ref TEXT NOT NULL,
    workload_ref TEXT NOT NULL,
    run_ref TEXT NOT NULL,
    agent_ref TEXT NOT NULL,
    task_ref TEXT NOT NULL,
    session_ref TEXT NOT NULL,
    proposal_digest TEXT NOT NULL,
    protected_evidence_refs_json TEXT NOT NULL,
    audit_authorization_record_id TEXT,
    audit_outcome_record_id TEXT,
    abandonment_authorization_record_id TEXT,
    abandoned_by TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    retention_until INTEGER NOT NULL,
    UNIQUE (
        actor_ref,
        capability_name,
        capability_version,
        normalized_resource,
        semantic_operation,
        idempotency_key
    ),
    CHECK (created_at >= 0),
    CHECK (updated_at >= created_at),
    CHECK (retention_until > created_at),
    CHECK (state_kind IN (
        'reserved',
        'pending',
        'suspended',
        'succeeded',
        'failed',
        'denied',
        'outcome_unknown',
        'abandoned'
    ))
);

CREATE INDEX invocations_scope_state
ON invocations (
    actor_ref,
    capability_name,
    capability_version,
    normalized_resource,
    semantic_operation,
    state_kind
);

CREATE INDEX invocations_retention
ON invocations (retention_until);
