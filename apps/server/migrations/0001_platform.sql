-- janus-module: platform
-- recovery: append-only; safe to retry through SQLx migrations

CREATE TABLE public_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    actor_json TEXT NOT NULL,
    resource_json TEXT,
    correlation_id TEXT NOT NULL,
    causation_id TEXT,
    payload_json TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE INDEX public_events_type_cursor_idx
    ON public_events (event_type, cursor);
