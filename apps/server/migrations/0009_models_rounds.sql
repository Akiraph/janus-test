-- janus-module: models
-- recovery: append-only; safe to retry through SQLx migrations

-- One real Provider HTTP attempt for a Round. M3 does not write failover rows;
-- attempt_type stays 'normal'. round_id is supervisor-owned (no FK).
CREATE TABLE model_attempts (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL,
    candidate_order INTEGER NOT NULL DEFAULT 0,
    provider_id TEXT NOT NULL,
    upstream_model_id TEXT NOT NULL,
    attempt_type TEXT NOT NULL DEFAULT 'normal',
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
    normalized_error_json TEXT,
    upstream_request_id TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    created_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX model_attempts_round_idx ON model_attempts (round_id, candidate_order);

-- Append-only Token usage ledger keyed by attempt. project/session/turn/round ids
-- are denormalized for reporting; they are not FKs.
CREATE TABLE model_usage_ledger (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    round_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    upstream_model_id TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_tokens INTEGER NOT NULL DEFAULT 0,
    attempt_result TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);
CREATE INDEX model_usage_ledger_attempt_idx ON model_usage_ledger (attempt_id);
CREATE INDEX model_usage_ledger_session_idx ON model_usage_ledger (session_id, occurred_at);
CREATE INDEX model_usage_ledger_project_idx ON model_usage_ledger (project_id, occurred_at);
