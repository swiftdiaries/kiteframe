ALTER TABLE permission_requests ADD COLUMN last_offered_at TIMESTAMPTZ;

CREATE INDEX permission_requests_fair_delivery_idx
    ON permission_requests (COALESCE(last_offered_at, created_at), created_at, id)
    WHERE state = 'pending';
