-- janus-module: platform
-- recovery: append-only; cleanup intents are safe to retry

-- A reference may be written before its owning row, or its deletion may fail
-- after the owning row commits. Keep the cleanup obligation durable instead of
-- relying on a best-effort post-commit filesystem/database call.
CREATE TABLE blob_cleanup_intents (
    id TEXT PRIMARY KEY,
    owner_module TEXT NOT NULL,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(owner_module, owner_type, owner_id, purpose)
);

CREATE INDEX blob_cleanup_intents_due_idx
    ON blob_cleanup_intents (next_attempt_at, updated_at);
