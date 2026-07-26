-- janus-module: supervisor
-- recovery: append-only; safe to retry through SQLx migrations

-- One model Round inside a Turn. turn_id is a sessions-owned identity; no cross-
-- module FK enforcement so sessions can cascade-delete without supervisor coupling.
CREATE TABLE rounds (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    context_version TEXT,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
    candidate_snapshot_json TEXT,
    final_attempt_id TEXT,
    output_summary_json TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    stop_reason TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (turn_id, sequence)
);
CREATE INDEX rounds_turn_idx ON rounds (turn_id, sequence);

-- Tool Call requested by a Round. Status: requested → running → succeeded | failed.
CREATE TABLE tool_calls (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    input_json TEXT NOT NULL,
    result_summary_json TEXT,
    status TEXT NOT NULL CHECK (status IN ('requested', 'running', 'succeeded', 'failed')),
    actor_json TEXT NOT NULL,
    error_code TEXT,
    started_at TEXT,
    ended_at TEXT,
    version TEXT NOT NULL,
    UNIQUE (round_id, ord)
);
CREATE INDEX tool_calls_round_idx ON tool_calls (round_id, ord);
