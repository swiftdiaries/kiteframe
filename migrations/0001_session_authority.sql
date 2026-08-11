CREATE TABLE sessions (
    id UUID PRIMARY KEY,
    principal TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'closed')),
    working_directory TEXT NOT NULL,
    retention_until TIMESTAMPTZ NOT NULL,
    current_catalog_epoch UUID,
    current_policy_revision BIGINT NOT NULL DEFAULT 0 CHECK (current_policy_revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE mcp_catalog_epochs (
    epoch_id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    server_alias TEXT NOT NULL,
    refresh_identity TEXT NOT NULL,
    canonical_digest TEXT NOT NULL,
    connection_binding JSONB NOT NULL,
    descriptor_snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (session_id, server_alias, canonical_digest),
    UNIQUE (session_id, server_alias, refresh_identity)
);

ALTER TABLE sessions
    ADD CONSTRAINT sessions_current_catalog_epoch_fk
    FOREIGN KEY (current_catalog_epoch) REFERENCES mcp_catalog_epochs(epoch_id);

CREATE TABLE session_policy_rules (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id),
    revision BIGINT NOT NULL CHECK (revision > 0),
    idempotency_key TEXT NOT NULL,
    typed_source JSONB NOT NULL,
    compiled_cedar TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (session_id, revision),
    UNIQUE (session_id, idempotency_key)
);

CREATE TABLE workflow_definitions (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    definition_hash TEXT NOT NULL,
    source TEXT NOT NULL,
    compiled_definition JSONB NOT NULL,
    tool_bindings JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (name, version),
    UNIQUE (definition_hash)
);
