ALTER TABLE harness_invocations
    ADD COLUMN terminal_payload JSONB;

-- Earlier journal rows predate terminal payload persistence.  A completed row
-- cannot safely recreate its evidence, so converge it to outcome_unknown; all
-- backfilled rows then use the same canonical terminal identity as new rows.
ALTER TABLE harness_invocations
    DISABLE TRIGGER harness_invocations_reject_invalid_update;

UPDATE harness_invocations
SET state = CASE WHEN state = 'completed' THEN 'outcome_unknown' ELSE state END,
    terminal_request_digest = 'harness-terminal:' || invocation_id::text,
    terminal_payload = CASE
        WHEN state = 'cancelled' THEN '{"type":"cancelled"}'::jsonb
        WHEN state = 'completed' THEN jsonb_build_object(
            'type', 'outcome_unknown',
            'code', 'harness_outcome_unknown',
            'detail', 'harness terminal outcome is unknown'
        )
        ELSE jsonb_build_object(
            'type', 'failed',
            'code', 'harness_failed',
            'detail', 'legacy terminal payload was unavailable'
        )
    END
WHERE terminal = TRUE AND terminal_payload IS NULL;

ALTER TABLE harness_invocations
    ENABLE TRIGGER harness_invocations_reject_invalid_update;

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
