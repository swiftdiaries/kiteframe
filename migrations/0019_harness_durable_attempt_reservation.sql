LOCK TABLE harness_invocations IN SHARE ROW EXCLUSIVE MODE;

ALTER TABLE harness_invocations
    ADD COLUMN max_pre_accept_attempts BIGINT NOT NULL DEFAULT 3,
    ADD CONSTRAINT harness_invocations_positive_attempt_limit_check CHECK (
        max_pre_accept_attempts > 0
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
        OR NEW.max_pre_accept_attempts IS DISTINCT FROM OLD.max_pre_accept_attempts
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
    IF OLD.state = 'started' AND NEW.state = 'prepared'
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
