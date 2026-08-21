LOCK TABLE harness_invocations IN SHARE ROW EXCLUSIVE MODE;

ALTER TABLE harness_invocations
    DISABLE TRIGGER harness_invocations_reject_invalid_update;

UPDATE harness_invocations
SET exhaustion_request_digest = 'harness-terminal:' || invocation_id::text,
    exhaustion_payload = jsonb_build_object(
        'type', 'failed',
        'code', 'harness_failed',
        'detail', 'harness pre-accept attempt budget exhausted'
    ),
    terminal_request_digest = CASE
        WHEN state = 'failed' THEN 'harness-terminal:' || invocation_id::text
        ELSE terminal_request_digest
    END,
    terminal_payload = CASE
        WHEN state = 'failed' THEN jsonb_build_object(
            'type', 'failed',
            'code', 'harness_failed',
            'detail', 'harness pre-accept attempt budget exhausted'
        )
        ELSE terminal_payload
    END
WHERE pre_accept_exhausted = TRUE;

ALTER TABLE harness_invocations
    ENABLE TRIGGER harness_invocations_reject_invalid_update;

ALTER TABLE harness_invocations
    DROP CONSTRAINT harness_invocations_exhaustion_check,
    ADD CONSTRAINT harness_invocations_exhaustion_check CHECK (
        (
            pre_accept_exhausted
            AND prompt_accepted = FALSE
            AND exhaustion_request_digest = 'harness-terminal:' || invocation_id::text
            AND exhaustion_payload = jsonb_build_object(
                'type', 'failed',
                'code', 'harness_failed',
                'detail', 'harness pre-accept attempt budget exhausted'
            )
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

    IF OLD.terminal AND NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'terminal harness invocation is immutable' USING ERRCODE = '55000';
    END IF;
    IF OLD.terminal THEN
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
