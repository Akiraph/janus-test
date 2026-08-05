-- janus-module: workspace-sync
-- recovery: append-only; safe to retry through SQLx migrations

-- Durable intent for the filesystem half of Apply/Sync. The intent is
-- committed before any path is copied, so startup can replay an interrupted
-- transfer instead of guessing whether the database cursor is authoritative.
ALTER TABLE propagation_links ADD COLUMN recovery_state TEXT NOT NULL DEFAULT 'idle'
    CHECK (recovery_state IN ('idle', 'transferring'));
ALTER TABLE propagation_links ADD COLUMN recovery_intent_json TEXT;
ALTER TABLE propagation_links ADD COLUMN recovery_error TEXT;
