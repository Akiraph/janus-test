-- janus-module: runtime
-- recovery: additive; existing Jobs have no pending cancellation request.

ALTER TABLE jobs ADD COLUMN cancellation_requested_at TEXT;
