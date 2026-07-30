CREATE TABLE invocation_audit_links (
    invocation_id TEXT NOT NULL
        REFERENCES invocations (invocation_id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    kind TEXT NOT NULL CHECK (kind IN ('authorization', 'outcome')),
    record_id TEXT NOT NULL CHECK (length(trim(record_id)) > 0),
    attached_at INTEGER NOT NULL CHECK (attached_at >= 0),
    PRIMARY KEY (invocation_id, sequence),
    UNIQUE (invocation_id, kind, record_id)
);

INSERT INTO invocation_audit_links (
    invocation_id, sequence, kind, record_id, attached_at
)
SELECT
    invocation_id, 1, 'authorization', audit_authorization_record_id, updated_at
FROM invocations
WHERE audit_authorization_record_id IS NOT NULL;

INSERT INTO invocation_audit_links (
    invocation_id, sequence, kind, record_id, attached_at
)
SELECT
    invocation_id,
    CASE WHEN audit_authorization_record_id IS NULL THEN 1 ELSE 2 END,
    'outcome',
    audit_outcome_record_id,
    updated_at
FROM invocations
WHERE audit_outcome_record_id IS NOT NULL;

ALTER TABLE invocations DROP COLUMN audit_authorization_record_id;
ALTER TABLE invocations DROP COLUMN audit_outcome_record_id;

CREATE INDEX invocation_audit_links_record
ON invocation_audit_links (record_id);
