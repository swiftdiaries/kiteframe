CREATE FUNCTION harness_event_is_closed_v1(event JSONB) RETURNS BOOLEAN AS $$
BEGIN
    IF jsonb_typeof(event) IS DISTINCT FROM 'object' THEN
        RETURN FALSE;
    END IF;
    RETURN CASE event->>'type'
        WHEN 'started' THEN event = '{"type":"started"}'::jsonb
        WHEN 'cancelled' THEN event = '{"type":"cancelled"}'::jsonb
        WHEN 'progress' THEN event ? 'message'
            AND jsonb_typeof(event->'message') = 'string'
            AND event - 'type' - 'message' = '{}'::jsonb
        WHEN 'assistant_message' THEN event ? 'text'
            AND jsonb_typeof(event->'text') = 'string'
            AND event - 'type' - 'text' = '{}'::jsonb
        WHEN 'approval_requested' THEN event ? 'requestId' AND event ? 'title'
            AND jsonb_typeof(event->'requestId') = 'string'
            AND jsonb_typeof(event->'title') = 'string'
            AND event - 'type' - 'requestId' - 'title' = '{}'::jsonb
        WHEN 'approval_resolved' THEN event ? 'requestId' AND event ? 'decision'
            AND jsonb_typeof(event->'requestId') = 'string'
            AND jsonb_typeof(event->'decision') = 'string'
            AND event->>'decision' IN ('allow_once', 'reject_once')
            AND event - 'type' - 'requestId' - 'decision' = '{}'::jsonb
        ELSE FALSE
    END;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

CREATE FUNCTION harness_event_canonical_v1(
    invocation_id UUID,
    sequence BIGINT,
    event JSONB
) RETURNS TEXT AS $$
DECLARE
    canonical_event TEXT;
BEGIN
    IF NOT harness_event_is_closed_v1(event) THEN
        RAISE EXCEPTION 'harness event is not a closed v1 event' USING ERRCODE = '55000';
    END IF;
    canonical_event := CASE event->>'type'
        WHEN 'started' THEN '{"type":"started"}'
        WHEN 'cancelled' THEN '{"type":"cancelled"}'
        WHEN 'progress' THEN
            '{"message":' || to_jsonb(event->>'message')::text || ',"type":"progress"}'
        WHEN 'assistant_message' THEN
            '{"text":' || to_jsonb(event->>'text')::text || ',"type":"assistant_message"}'
        WHEN 'approval_requested' THEN
            '{"requestId":' || to_jsonb(event->>'requestId')::text
                || ',"title":' || to_jsonb(event->>'title')::text
                || ',"type":"approval_requested"}'
        WHEN 'approval_resolved' THEN
            '{"decision":' || to_jsonb(event->>'decision')::text
                || ',"requestId":' || to_jsonb(event->>'requestId')::text
                || ',"type":"approval_resolved"}'
        ELSE NULL
    END;
    IF canonical_event IS NULL THEN
        RAISE EXCEPTION 'harness event is not a closed v1 event' USING ERRCODE = '55000';
    END IF;
    RETURN '{"event":' || canonical_event
        || ',"invocationId":' || to_jsonb(invocation_id::text)::text
        || ',"protocolVersion":1,"sequence":' || sequence::text || '}';
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE FUNCTION harness_event_envelope_digest_v1(
    invocation_id UUID,
    sequence BIGINT,
    event JSONB
) RETURNS TEXT AS $$
    SELECT encode(
        sha256(convert_to(harness_event_canonical_v1($1, $2, $3), 'UTF8')),
        'hex'
    )
$$ LANGUAGE SQL IMMUTABLE STRICT;

-- Runtime registration/prepare acquires registration before invocation state.
-- Hold the migration's eventual registration ALTER/DELETE lock first so an
-- older dispatcher can never wait on registration while this migration holds
-- invocation or event locks.
LOCK TABLE harness_plugin_registrations IN ACCESS EXCLUSIVE MODE;
LOCK TABLE harness_invocations IN SHARE ROW EXCLUSIVE MODE;
LOCK TABLE harness_invocation_events IN ACCESS EXCLUSIVE MODE;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (
            SELECT invocation_id, MIN(sequence) AS minimum, MAX(sequence) AS maximum, COUNT(*) AS total
            FROM harness_invocation_events
            GROUP BY invocation_id
        ) sequences
        WHERE sequences.minimum <> 0 OR sequences.maximum <> sequences.total - 1
    ) THEN
        RAISE EXCEPTION 'existing harness event journal has a sequence gap' USING ERRCODE = '55000';
    END IF;
END;
$$;

CREATE TEMP TABLE harness_invocation_events_v1_backfill (
    invocation_id UUID NOT NULL,
    sequence BIGINT NOT NULL,
    event JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
) ON COMMIT DROP;

WITH needs_started AS (
    SELECT invocation_id
    FROM harness_invocation_events
    WHERE sequence = 0 AND event->>'type' <> 'started'
)
INSERT INTO harness_invocation_events_v1_backfill (invocation_id, sequence, event, created_at)
SELECT events.invocation_id,
       events.sequence + CASE WHEN needs_started.invocation_id IS NULL THEN 0 ELSE 1 END,
       CASE events.event->>'type'
           WHEN 'approval_requested' THEN jsonb_build_object(
               'type', 'approval_requested',
               'requestId', events.event->'request_id',
               'title', events.event->'title'
           )
           WHEN 'approval_resolved' THEN jsonb_build_object(
               'type', 'approval_resolved',
               'requestId', events.event->'request_id',
               'decision', events.event->'decision'
           )
           ELSE events.event
       END,
       events.created_at
FROM harness_invocation_events events
LEFT JOIN needs_started USING (invocation_id);

WITH needs_started AS (
    SELECT invocation_id, created_at
    FROM harness_invocation_events
    WHERE sequence = 0 AND event->>'type' <> 'started'
)
INSERT INTO harness_invocation_events_v1_backfill (invocation_id, sequence, event, created_at)
SELECT invocation_id, 0, '{"type":"started"}'::jsonb, created_at
FROM needs_started;

ALTER TABLE harness_invocation_events
    DISABLE TRIGGER harness_invocation_events_reject_update_delete;

ALTER TABLE harness_invocation_events
    DROP CONSTRAINT harness_invocation_events_event_check;

DELETE FROM harness_invocation_events;

INSERT INTO harness_invocation_events (
    invocation_id, sequence, event_digest, event, created_at
)
SELECT invocation_id,
       sequence,
       harness_event_envelope_digest_v1(invocation_id, sequence, event),
       event,
       created_at
FROM harness_invocation_events_v1_backfill
ORDER BY invocation_id, sequence;

ALTER TABLE harness_invocation_events
    ENABLE TRIGGER harness_invocation_events_reject_update_delete;

ALTER TABLE harness_invocation_events
    ADD CONSTRAINT harness_invocation_events_v1_event_check CHECK (
        harness_event_is_closed_v1(event)
    ),
    ADD CONSTRAINT harness_invocation_events_started_at_zero_check CHECK (
        (sequence = 0) = (event->>'type' = 'started')
    ),
    ADD CONSTRAINT harness_invocation_events_v1_digest_check CHECK (
        event_digest = harness_event_envelope_digest_v1(invocation_id, sequence, event)
    );

CREATE FUNCTION validate_harness_invocation_event_insert() RETURNS trigger AS $$
DECLARE
    maximum BIGINT;
    stored_digest TEXT;
    stored_event JSONB;
BEGIN
    PERFORM 1
    FROM harness_invocations
    WHERE invocation_id = NEW.invocation_id
    FOR UPDATE;

    IF (NEW.sequence = 0) <> (NEW.event->>'type' = 'started') THEN
        RAISE EXCEPTION 'harness event sequence zero must be Started' USING ERRCODE = '55000';
    END IF;

    SELECT event_digest, event
    INTO stored_digest, stored_event
    FROM harness_invocation_events
    WHERE invocation_id = NEW.invocation_id AND sequence = NEW.sequence;

    IF FOUND THEN
        IF stored_digest IS DISTINCT FROM NEW.event_digest
            OR stored_event IS DISTINCT FROM NEW.event THEN
            RAISE EXCEPTION 'harness event replay differs from the durable event'
                USING ERRCODE = '55000';
        END IF;
        RETURN NEW;
    END IF;

    SELECT COALESCE(MAX(sequence), -1)
    INTO maximum
    FROM harness_invocation_events
    WHERE invocation_id = NEW.invocation_id;

    IF NEW.sequence <> maximum + 1 THEN
        RAISE EXCEPTION 'harness event sequence gap' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER harness_invocation_events_validate_insert
    BEFORE INSERT ON harness_invocation_events
    FOR EACH ROW EXECUTE FUNCTION validate_harness_invocation_event_insert();

-- The all-zero built-in attestation was development-only.  Remove it only when
-- no durable invocation references it; otherwise stop so an operator can
-- reconcile that development journal rather than silently relabel provenance.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM harness_plugin_registrations registrations
        JOIN harness_invocations invocations USING (profile_id)
        WHERE registrations.profile_id = 'builtin-v1'
          AND registrations.source_revision = '0000000000000000000000000000000000000000'
    ) THEN
        RAISE EXCEPTION 'zero-revision builtin registration has durable invocations; reconcile the development journal before upgrade'
            USING ERRCODE = '55000';
    END IF;
END;
$$;

ALTER TABLE harness_plugin_registrations
    DISABLE TRIGGER harness_plugin_registrations_reject_update_delete;

DELETE FROM harness_plugin_registrations
WHERE profile_id = 'builtin-v1'
  AND source_revision = '0000000000000000000000000000000000000000';

ALTER TABLE harness_plugin_registrations
    ENABLE TRIGGER harness_plugin_registrations_reject_update_delete;

ALTER TABLE harness_plugin_registrations
    ADD CONSTRAINT harness_plugin_registrations_nonzero_revision_check CHECK (
        source_revision <> '0000000000000000000000000000000000000000'
    );
