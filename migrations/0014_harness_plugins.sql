CREATE TABLE harness_plugin_registrations (
    profile_id TEXT PRIMARY KEY,
    protocol_version INTEGER NOT NULL CHECK (protocol_version = 1),
    profile_digest TEXT NOT NULL,
    source_repository TEXT NOT NULL CHECK (source_repository <> ''),
    source_revision TEXT NOT NULL CHECK (source_revision ~ '^[0-9A-Fa-f]{40}$'),
    transport TEXT NOT NULL CHECK (transport IN ('builtin', 'acp_stdio')),
    enabled BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE harness_invocations (
    invocation_id UUID PRIMARY KEY,
    command_id UUID NOT NULL REFERENCES agent_commands(id),
    command_revision BIGINT NOT NULL CHECK (command_revision >= 0),
    profile_id TEXT NOT NULL REFERENCES harness_plugin_registrations(profile_id),
    profile_digest TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'prepared', 'started', 'accepted', 'completed',
        'cancelled', 'failed', 'outcome_unknown'
    )),
    attempt BIGINT NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    prompt_accepted BOOLEAN NOT NULL DEFAULT FALSE,
    terminal BOOLEAN NOT NULL DEFAULT FALSE,
    terminal_request_digest TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (command_id, command_revision),
    CHECK (
        terminal = (state IN ('completed', 'cancelled', 'failed', 'outcome_unknown'))
    ),
    CHECK (
        (terminal AND terminal_request_digest IS NOT NULL)
        OR (NOT terminal AND terminal_request_digest IS NULL)
    )
);

CREATE TABLE harness_invocation_events (
    invocation_id UUID NOT NULL REFERENCES harness_invocations(invocation_id),
    sequence BIGINT NOT NULL CHECK (sequence >= 0),
    event_digest TEXT NOT NULL,
    event JSONB NOT NULL CHECK (
        jsonb_typeof(event) = 'object' AND CASE event->>'type'
            WHEN 'started' THEN event - 'type' = '{}'::jsonb
            WHEN 'cancelled' THEN event - 'type' = '{}'::jsonb
            WHEN 'progress' THEN event ? 'message'
                AND jsonb_typeof(event->'message') = 'string'
                AND event - 'type' - 'message' = '{}'::jsonb
            WHEN 'assistant_message' THEN event ? 'text'
                AND jsonb_typeof(event->'text') = 'string'
                AND event - 'type' - 'text' = '{}'::jsonb
            WHEN 'approval_requested' THEN event ? 'request_id' AND event ? 'title'
                AND jsonb_typeof(event->'request_id') = 'string'
                AND jsonb_typeof(event->'title') = 'string'
                AND event - 'type' - 'request_id' - 'title' = '{}'::jsonb
            WHEN 'approval_resolved' THEN event ? 'request_id' AND event ? 'decision'
                AND jsonb_typeof(event->'request_id') = 'string'
                AND event->>'decision' IN ('allow_once', 'reject_once')
                AND event - 'type' - 'request_id' - 'decision' = '{}'::jsonb
            ELSE FALSE
        END
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (invocation_id, sequence)
);

CREATE FUNCTION reject_harness_plugin_registration_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'harness plugin registrations are immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER harness_plugin_registrations_reject_update_delete
    BEFORE UPDATE OR DELETE ON harness_plugin_registrations
    FOR EACH ROW EXECUTE FUNCTION reject_harness_plugin_registration_mutation();

CREATE FUNCTION reject_harness_invocation_mutation() RETURNS trigger AS $$
BEGIN
    IF NEW.invocation_id IS DISTINCT FROM OLD.invocation_id
        OR NEW.command_id IS DISTINCT FROM OLD.command_id
        OR NEW.command_revision IS DISTINCT FROM OLD.command_revision
        OR NEW.profile_id IS DISTINCT FROM OLD.profile_id
        OR NEW.profile_digest IS DISTINCT FROM OLD.profile_digest
        OR NEW.payload_digest IS DISTINCT FROM OLD.payload_digest
    THEN
        RAISE EXCEPTION 'harness invocation identity is immutable' USING ERRCODE = '55000';
    END IF;

    IF OLD.terminal THEN
        IF NOT NEW.terminal
            OR NEW.state IS DISTINCT FROM OLD.state
            OR NEW.terminal_request_digest IS DISTINCT FROM OLD.terminal_request_digest
        THEN
            RAISE EXCEPTION 'terminal harness invocation is immutable' USING ERRCODE = '55000';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.state = 'prepared' AND NEW.state IN ('started', 'cancelled', 'failed')
        AND NEW.attempt = OLD.attempt
        AND NEW.prompt_accepted = FALSE
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'started' AND NEW.state = 'started'
        AND NEW.attempt = OLD.attempt + 1
        AND OLD.prompt_accepted = FALSE
        AND NEW.prompt_accepted = FALSE
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'started' AND NEW.state IN ('accepted', 'cancelled', 'failed')
        AND NEW.attempt = OLD.attempt
        AND (NEW.state <> 'accepted' OR NEW.prompt_accepted = TRUE)
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'accepted' AND NEW.state IN ('completed', 'cancelled', 'failed', 'outcome_unknown')
        AND NEW.attempt = OLD.attempt
        AND NEW.prompt_accepted = TRUE
    THEN
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'invalid harness invocation state transition' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER harness_invocations_reject_invalid_update
    BEFORE UPDATE ON harness_invocations
    FOR EACH ROW EXECUTE FUNCTION reject_harness_invocation_mutation();

CREATE FUNCTION reject_harness_invocation_event_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'harness invocation events are immutable' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER harness_invocation_events_reject_update_delete
    BEFORE UPDATE OR DELETE ON harness_invocation_events
    FOR EACH ROW EXECUTE FUNCTION reject_harness_invocation_event_mutation();
