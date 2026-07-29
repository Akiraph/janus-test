-- janus-module: supervisor
-- recovery: forward-only; preserves existing Asks and records unknown for
-- cancellations written before closure attribution existed.

CREATE TEMP TABLE asks_snapshot AS SELECT * FROM asks;
DROP TABLE asks;

CREATE TABLE asks (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    tool_call_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('blocking', 'best_effort')),
    prompt_json TEXT NOT NULL,
    choices_json TEXT NOT NULL,
    default_json TEXT,
    answer_json TEXT,
    status TEXT NOT NULL CHECK (
        status IN ('open', 'answered', 'expired', 'closed_by_handoff', 'canceled')
    ),
    closure_reason TEXT,
    expires_at TEXT,
    answered_at TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (status IN ('open', 'answered', 'expired') AND closure_reason IS NULL)
        OR (status = 'closed_by_handoff' AND closure_reason = 'handoff')
        OR (status = 'canceled' AND closure_reason IS NOT NULL)
    )
);

INSERT INTO asks (
    id, turn_id, tool_call_id, mode, prompt_json, choices_json, default_json,
    answer_json, status, closure_reason, expires_at, answered_at, version,
    created_at, updated_at
)
SELECT
    id, turn_id, tool_call_id, mode, prompt_json, choices_json, default_json,
    answer_json, status,
    CASE WHEN status = 'canceled' THEN 'unknown' ELSE NULL END,
    expires_at, answered_at, version, created_at, updated_at
FROM asks_snapshot;

DROP TABLE asks_snapshot;
CREATE INDEX asks_turn_idx ON asks (turn_id, status);
