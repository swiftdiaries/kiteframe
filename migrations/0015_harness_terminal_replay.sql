ALTER TABLE harness_invocations
    ADD COLUMN terminal_payload JSONB;

ALTER TABLE harness_invocations
    ADD CONSTRAINT harness_invocations_terminal_payload_check CHECK (
        (terminal AND terminal_payload IS NOT NULL AND jsonb_typeof(terminal_payload) = 'object')
        OR (NOT terminal AND terminal_payload IS NULL)
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
    THEN
        RAISE EXCEPTION 'harness invocation identity is immutable' USING ERRCODE = '55000';
    END IF;

    IF OLD.terminal THEN
        IF NOT NEW.terminal
            OR NEW.state IS DISTINCT FROM OLD.state
            OR NEW.terminal_request_digest IS DISTINCT FROM OLD.terminal_request_digest
            OR NEW.terminal_payload IS DISTINCT FROM OLD.terminal_payload
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
