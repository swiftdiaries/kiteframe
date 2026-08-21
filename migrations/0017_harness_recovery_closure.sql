ALTER TABLE harness_invocations
    ADD COLUMN pre_accept_exhausted BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN exhaustion_request_digest TEXT,
    ADD COLUMN exhaustion_payload JSONB;

ALTER TABLE harness_invocations
    ADD CONSTRAINT harness_invocations_exhaustion_check CHECK (
        (
            pre_accept_exhausted
            AND prompt_accepted = FALSE
            AND state IN ('started', 'cancelled', 'failed')
            AND exhaustion_request_digest IS NOT NULL
            AND exhaustion_payload IS NOT NULL
            AND jsonb_typeof(exhaustion_payload) = 'object'
            AND exhaustion_payload->>'type' = 'failed'
            AND jsonb_typeof(exhaustion_payload->'code') = 'string'
            AND jsonb_typeof(exhaustion_payload->'detail') = 'string'
            AND exhaustion_payload - 'type' - 'code' - 'detail' = '{}'::jsonb
            AND (
                (
                    state = 'started'
                    AND terminal = FALSE
                    AND terminal_request_digest IS NULL
                    AND terminal_payload IS NULL
                )
                OR (
                    state = 'failed'
                    AND terminal = TRUE
                    AND terminal_request_digest = exhaustion_request_digest
                    AND terminal_payload = exhaustion_payload
                )
                OR (
                    state = 'cancelled'
                    AND terminal = TRUE
                    AND terminal_request_digest = 'harness-terminal:' || invocation_id::text
                    AND terminal_payload = '{"type":"cancelled"}'::jsonb
                )
            )
        )
        OR (
            NOT pre_accept_exhausted
            AND exhaustion_request_digest IS NULL
            AND exhaustion_payload IS NULL
        )
    );

CREATE TABLE harness_invocation_lifecycle_audits (
    invocation_id UUID NOT NULL REFERENCES harness_invocations(invocation_id),
    stage TEXT NOT NULL CHECK (stage = 'shutdown'),
    code TEXT NOT NULL CHECK (code IN (
        'harness_profile_unavailable', 'harness_protocol_incompatible',
        'harness_capability_unsupported', 'harness_permission_rejected',
        'harness_cancelled', 'harness_failed', 'harness_outcome_unknown'
    )),
    detail TEXT NOT NULL CHECK (detail <> '' AND octet_length(detail) <= 4096),
    event_digest TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (invocation_id, stage)
);

CREATE TRIGGER harness_invocation_lifecycle_audits_reject_update_delete
    BEFORE UPDATE OR DELETE ON harness_invocation_lifecycle_audits
    FOR EACH ROW EXECUTE FUNCTION reject_harness_invocation_event_mutation();

CREATE OR REPLACE FUNCTION reject_harness_invocation_mutation() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'harness invocations are immutable' USING ERRCODE = '55000';
    END IF;

    IF NEW.invocation_id IS DISTINCT FROM OLD.invocation_id
        OR NEW.command_id IS DISTINCT FROM OLD.command_id
        OR NEW.command_revision IS DISTINCT FROM OLD.command_revision
        OR NEW.profile_id IS DISTINCT FROM OLD.profile_id
        OR NEW.profile_digest IS DISTINCT FROM OLD.profile_digest
        OR NEW.payload_digest IS DISTINCT FROM OLD.payload_digest
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    THEN
        RAISE EXCEPTION 'harness invocation identity is immutable' USING ERRCODE = '55000';
    END IF;

    IF OLD.terminal THEN
        IF NOT NEW.terminal
            OR NEW.state IS DISTINCT FROM OLD.state
            OR NEW.attempt IS DISTINCT FROM OLD.attempt
            OR NEW.prompt_accepted IS DISTINCT FROM OLD.prompt_accepted
            OR NEW.terminal_request_digest IS DISTINCT FROM OLD.terminal_request_digest
            OR NEW.terminal_payload IS DISTINCT FROM OLD.terminal_payload
            OR NEW.pre_accept_exhausted IS DISTINCT FROM OLD.pre_accept_exhausted
            OR NEW.exhaustion_request_digest IS DISTINCT FROM OLD.exhaustion_request_digest
            OR NEW.exhaustion_payload IS DISTINCT FROM OLD.exhaustion_payload
        THEN
            RAISE EXCEPTION 'terminal harness invocation is immutable' USING ERRCODE = '55000';
        END IF;
        RETURN NEW;
    END IF;

    IF OLD.pre_accept_exhausted THEN
        IF NEW.terminal = TRUE
            AND NEW.attempt = OLD.attempt
            AND NEW.prompt_accepted = FALSE
            AND NEW.pre_accept_exhausted = TRUE
            AND NEW.exhaustion_request_digest IS NOT DISTINCT FROM OLD.exhaustion_request_digest
            AND NEW.exhaustion_payload IS NOT DISTINCT FROM OLD.exhaustion_payload
            AND (
                (
                    NEW.state = 'failed'
                    AND NEW.terminal_request_digest IS NOT DISTINCT FROM OLD.exhaustion_request_digest
                    AND NEW.terminal_payload IS NOT DISTINCT FROM OLD.exhaustion_payload
                )
                OR (
                    NEW.state = 'cancelled'
                    AND NEW.terminal_request_digest = 'harness-terminal:' || OLD.invocation_id::text
                    AND NEW.terminal_payload = '{"type":"cancelled"}'::jsonb
                )
            )
        THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'exhausted harness invocation is dispatch closed' USING ERRCODE = '55000';
    END IF;

    IF OLD.state = 'prepared' AND NEW.state IN ('started', 'cancelled', 'failed')
        AND NEW.attempt = OLD.attempt
        AND NEW.prompt_accepted = FALSE
        AND NEW.pre_accept_exhausted = FALSE
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'started' AND NEW.state = 'started'
        AND NEW.attempt = OLD.attempt + 1
        AND OLD.prompt_accepted = FALSE
        AND NEW.prompt_accepted = FALSE
        AND NEW.pre_accept_exhausted = FALSE
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'started' AND NEW.state = 'started'
        AND NEW.attempt = OLD.attempt
        AND OLD.prompt_accepted = FALSE
        AND NEW.prompt_accepted = FALSE
        AND NEW.pre_accept_exhausted = TRUE
        AND NEW.exhaustion_request_digest IS NOT NULL
        AND NEW.exhaustion_payload IS NOT NULL
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'started' AND NEW.state IN ('accepted', 'cancelled', 'failed')
        AND NEW.attempt = OLD.attempt
        AND (NEW.state <> 'accepted' OR NEW.prompt_accepted = TRUE)
        AND NEW.pre_accept_exhausted = FALSE
    THEN
        RETURN NEW;
    END IF;
    IF OLD.state = 'accepted' AND NEW.state IN ('completed', 'cancelled', 'failed', 'outcome_unknown')
        AND NEW.attempt = OLD.attempt
        AND NEW.prompt_accepted = TRUE
        AND NEW.pre_accept_exhausted = FALSE
    THEN
        RETURN NEW;
    END IF;

    RAISE EXCEPTION 'invalid harness invocation state transition' USING ERRCODE = '55000';
END;
$$ LANGUAGE plpgsql;
