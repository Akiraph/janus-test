-- janus-module: projects
-- janus-module: models
-- janus-module: sessions
-- janus-module: supervisor
-- janus-module: runtime
-- recovery: forward-only; active external work is never replayed

CREATE TABLE project_runtime_configs (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    executor_kind TEXT NOT NULL DEFAULT 'container' CHECK (executor_kind IN ('local', 'container')),
    allow_insecure_local_executor INTEGER NOT NULL DEFAULT 0,
    variables_json TEXT NOT NULL DEFAULT '{}',
    default_limits_json TEXT NOT NULL,
    network_policy TEXT NOT NULL DEFAULT 'deny_all' CHECK (network_policy IN ('deny_all', 'project_rules')),
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE project_runtime_secrets (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value_ciphertext BLOB NOT NULL,
    value_fingerprint TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE TABLE project_egress_rules (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    scheme TEXT NOT NULL CHECK (scheme IN ('http', 'https')),
    host TEXT NOT NULL,
    port_start INTEGER NOT NULL CHECK (port_start BETWEEN 1 AND 65535),
    port_end INTEGER NOT NULL CHECK (port_end BETWEEN port_start AND 65535),
    purpose TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (project_id, scheme, host, port_start, port_end)
);

CREATE TABLE project_cli_configs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('claude_code', 'codex')),
    enabled INTEGER NOT NULL DEFAULT 0,
    secret_id TEXT REFERENCES project_runtime_secrets(id) ON DELETE SET NULL,
    options_json TEXT NOT NULL DEFAULT '{}',
    observed_version TEXT,
    capability_state TEXT NOT NULL DEFAULT 'unconfigured'
        CHECK (capability_state IN ('ready', 'degraded', 'unconfigured', 'unsupported')),
    capability_reason TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (project_id, kind)
);

CREATE TABLE models (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    upstream_model_id TEXT NOT NULL,
    context_limit INTEGER NOT NULL DEFAULT 200000 CHECK (context_limit IN (200000, 1000000)),
    supports_images INTEGER NOT NULL DEFAULT 0,
    supports_tools INTEGER NOT NULL DEFAULT 1,
    parameters_json TEXT NOT NULL DEFAULT '{}',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (provider_id, display_name),
    UNIQUE (provider_id, upstream_model_id)
);

WITH model_rows AS MATERIALIZED (
    SELECT
        lower(hex(randomblob(16))) AS raw_id,
        provider.id AS provider_id,
        json_extract(item.value, '$.display_name') AS display_name,
        json_extract(item.value, '$.upstream_model_id') AS upstream_model_id,
        CASE WHEN coalesce(json_extract(item.value, '$.supports_1m'), 0) = 1
            THEN 1000000 ELSE 200000 END AS context_limit,
        coalesce(json_extract(item.value, '$.supports_images'), 0) AS supports_images,
        coalesce(json_extract(item.value, '$.enabled'), 1) AS enabled,
        provider.created_at AS created_at,
        provider.updated_at AS updated_at
    FROM model_providers AS provider, json_each(provider.models_json) AS item
    WHERE json_valid(provider.models_json)
      AND json_type(item.value) = 'object'
      AND trim(coalesce(json_extract(item.value, '$.display_name'), '')) <> ''
      AND trim(coalesce(json_extract(item.value, '$.upstream_model_id'), '')) <> ''
)
INSERT INTO models (
    id, provider_id, display_name, upstream_model_id, context_limit,
    supports_images, supports_tools, parameters_json, enabled, created_at, updated_at
)
SELECT
    substr(raw_id, 1, 8) || '-' || substr(raw_id, 9, 4) || '-7' || substr(raw_id, 14, 3) ||
        '-a' || substr(raw_id, 18, 3) || '-' || substr(raw_id, 21, 12),
    provider_id, trim(display_name), trim(upstream_model_id), context_limit,
    supports_images, 1, '{}', enabled, created_at, updated_at
FROM model_rows;

CREATE TABLE model_failover (
    primary_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    candidate_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    created_at TEXT NOT NULL,
    PRIMARY KEY (primary_model_id, candidate_model_id),
    UNIQUE (primary_model_id, ordinal),
    CHECK (primary_model_id <> candidate_model_id)
);

CREATE TABLE model_recovery_cooldowns (
    primary_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    candidate_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    next_probe_at TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (primary_model_id, candidate_model_id),
    CHECK (primary_model_id <> candidate_model_id)
);

PRAGMA legacy_alter_table = ON;

ALTER TABLE turns RENAME TO turns_legacy;
CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN (
        'queued', 'running', 'waiting_for_job', 'waiting_for_ask', 'waiting_for_model',
        'canceling', 'completed', 'failed', 'canceled', 'interrupted', 'handed_off'
    )),
    input_message_id TEXT,
    model_snapshot_json TEXT NOT NULL,
    predecessor_turn_id TEXT,
    handoff_from_turn_id TEXT,
    handoff_to_turn_id TEXT,
    completion_summary_json TEXT,
    completion_reason TEXT,
    cancellation_reason TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (session_id, sequence)
);
INSERT INTO turns (
    id, session_id, sequence, status, input_message_id, model_snapshot_json,
    completion_summary_json, completion_reason, input_tokens, output_tokens,
    version, created_at, updated_at
)
SELECT
    id, session_id, sequence,
    CASE status WHEN 'running' THEN 'interrupted' ELSE status END,
    input_message_id, model_snapshot_json, completion_summary_json,
    CASE WHEN status = 'running' THEN 'control_plane_restart' ELSE completion_reason END,
    input_tokens, output_tokens, version, created_at, updated_at
FROM turns_legacy;
DROP TABLE turns_legacy;
CREATE INDEX turns_session_idx ON turns (session_id, sequence);
CREATE UNIQUE INDEX turns_one_active_per_session ON turns (session_id)
WHERE status IN ('running', 'waiting_for_job', 'waiting_for_ask', 'waiting_for_model', 'canceling');
CREATE INDEX turns_queued_idx ON turns (session_id, sequence) WHERE status = 'queued';
UPDATE sessions SET active_turn_id = NULL, state = 'ready'
WHERE active_turn_id IS NOT NULL;

-- Preserve dependent Tool Calls before rebuilding rounds. SQLite retargets
-- their foreign key to rounds_legacy on rename and would otherwise cascade
-- delete them when that table is dropped.
CREATE TEMP TABLE tool_calls_legacy AS SELECT * FROM tool_calls;
DROP TABLE tool_calls;

ALTER TABLE rounds RENAME TO rounds_legacy;
CREATE TABLE rounds (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    context_version TEXT,
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'canceled', 'interrupted')),
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
INSERT INTO rounds
SELECT id, turn_id, sequence, context_version,
    CASE status WHEN 'running' THEN 'interrupted' ELSE status END,
    candidate_snapshot_json, final_attempt_id, output_summary_json,
    input_tokens, output_tokens,
    CASE WHEN status = 'running' THEN 'control_plane_restart' ELSE stop_reason END,
    version, created_at, updated_at
FROM rounds_legacy;
DROP TABLE rounds_legacy;
CREATE INDEX rounds_turn_idx ON rounds (turn_id, sequence);

CREATE TABLE tool_calls (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL REFERENCES rounds(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    input_json TEXT NOT NULL,
    result_summary_json TEXT,
    result_metadata_json TEXT,
    status TEXT NOT NULL CHECK (status IN ('requested', 'running', 'waiting', 'succeeded', 'failed', 'canceled', 'lost')),
    actor_json TEXT NOT NULL,
    error_code TEXT,
    runtime_id TEXT,
    job_id TEXT,
    service_id TEXT,
    terminal_id TEXT,
    log_stream_id TEXT,
    correlation_id TEXT,
    started_at TEXT,
    ended_at TEXT,
    version TEXT NOT NULL,
    UNIQUE (round_id, ord)
);
INSERT INTO tool_calls (
    id, round_id, ord, tool_name, schema_version, input_json, result_summary_json,
    status, actor_json, error_code, started_at, ended_at, version
)
SELECT id, round_id, ord, tool_name, schema_version, input_json, result_summary_json,
    CASE status WHEN 'running' THEN 'lost' WHEN 'requested' THEN 'canceled' ELSE status END,
    actor_json,
    CASE WHEN status IN ('running', 'requested') THEN 'RUNTIME_UNAVAILABLE' ELSE error_code END,
    started_at, ended_at, version
FROM tool_calls_legacy;
DROP TABLE tool_calls_legacy;
CREATE INDEX tool_calls_round_idx ON tool_calls (round_id, ord);

ALTER TABLE model_attempts RENAME TO model_attempts_legacy;
CREATE TABLE model_attempts (
    id TEXT PRIMARY KEY,
    round_id TEXT NOT NULL,
    candidate_order INTEGER NOT NULL DEFAULT 0,
    attempt_number INTEGER NOT NULL DEFAULT 0 CHECK (attempt_number BETWEEN 0 AND 5),
    provider_id TEXT NOT NULL,
    model_id TEXT REFERENCES models(id) ON DELETE SET NULL,
    upstream_model_id TEXT NOT NULL,
    attempt_type TEXT NOT NULL DEFAULT 'normal' CHECK (attempt_type IN ('normal', 'recovery_probe', 'compact')),
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'canceled', 'interrupted')),
    retryable INTEGER,
    classification TEXT,
    retry_after_ms INTEGER,
    normalized_error_json TEXT,
    upstream_request_id TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    created_at TEXT NOT NULL,
    ended_at TEXT
);
INSERT INTO model_attempts (
    id, round_id, candidate_order, provider_id, upstream_model_id, attempt_type,
    status, normalized_error_json, upstream_request_id, input_tokens, output_tokens,
    created_at, ended_at
)
SELECT id, round_id, candidate_order, provider_id, upstream_model_id, attempt_type,
    CASE status WHEN 'running' THEN 'interrupted' ELSE status END,
    normalized_error_json, upstream_request_id, input_tokens, output_tokens,
    created_at, ended_at
FROM model_attempts_legacy;
DROP TABLE model_attempts_legacy;
CREATE INDEX model_attempts_round_idx ON model_attempts (round_id, candidate_order, attempt_number);

PRAGMA legacy_alter_table = OFF;

CREATE TABLE asks (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    tool_call_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('blocking', 'best_effort')),
    prompt_json TEXT NOT NULL,
    choices_json TEXT NOT NULL,
    default_json TEXT,
    answer_json TEXT,
    status TEXT NOT NULL CHECK (status IN ('open', 'answered', 'expired', 'canceled')),
    expires_at TEXT,
    answered_at TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX asks_turn_idx ON asks (turn_id, status);

CREATE TABLE plan_versions (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    plan_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    UNIQUE (turn_id, sequence)
);

CREATE TABLE compact_summaries (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    source_first_timeline_id TEXT,
    source_last_timeline_id TEXT NOT NULL,
    summary_json TEXT NOT NULL,
    model_attempt_id TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX compact_summaries_session_idx ON compact_summaries (session_id, created_at);

CREATE TABLE context_versions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    sequence INTEGER NOT NULL,
    compact_summary_id TEXT REFERENCES compact_summaries(id) ON DELETE SET NULL,
    system_prefix_version TEXT NOT NULL,
    estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
    context_limit INTEGER NOT NULL DEFAULT 200000,
    compact_status TEXT NOT NULL DEFAULT 'not_needed'
        CHECK (compact_status IN ('not_needed', 'scheduled', 'running', 'succeeded', 'failed')),
    selection_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (session_id, sequence)
);
CREATE INDEX context_versions_session_idx ON context_versions (session_id, sequence);

CREATE TABLE stream_diagnostics (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    summary_json TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX stream_diagnostics_attempt_idx ON stream_diagnostics (attempt_id, created_at);

CREATE TABLE runtimes (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    executor_kind TEXT NOT NULL CHECK (executor_kind IN ('local', 'container')),
    executor_identity TEXT,
    executor_nonce TEXT NOT NULL,
    limits_json TEXT NOT NULL,
    capability_snapshot_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('starting', 'ready', 'stopping', 'stopped', 'failed', 'lost')),
    stop_reason TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    stopped_at TEXT
);
CREATE UNIQUE INDEX runtimes_one_current_per_session ON runtimes (session_id)
WHERE status IN ('starting', 'ready', 'stopping');

CREATE TABLE log_streams (
    id TEXT PRIMARY KEY,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('job', 'service', 'terminal', 'sync')),
    owner_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    first_cursor INTEGER NOT NULL DEFAULT 0,
    next_cursor INTEGER NOT NULL DEFAULT 0,
    retained_bytes INTEGER NOT NULL DEFAULT 0,
    total_bytes INTEGER NOT NULL DEFAULT 0,
    truncated INTEGER NOT NULL DEFAULT 0,
    closed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX log_streams_owner_idx ON log_streams (owner_kind, owner_id);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    runtime_id TEXT NOT NULL REFERENCES runtimes(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    initiated_by_tool_call_id TEXT NOT NULL,
    controlling_turn_id TEXT NOT NULL,
    cli_kind TEXT CHECK (cli_kind IN ('claude_code', 'codex')),
    cli_session_id TEXT,
    command_summary TEXT NOT NULL,
    executor_process_identity TEXT,
    executor_nonce TEXT NOT NULL,
    log_stream_id TEXT NOT NULL REFERENCES log_streams(id),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'canceled', 'lost')),
    exit_json TEXT,
    usage_json TEXT NOT NULL DEFAULT '{"cpu_millis":0,"peak_memory_bytes":0}',
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT
);
CREATE INDEX jobs_session_idx ON jobs (session_id, created_at);
CREATE INDEX jobs_turn_idx ON jobs (controlling_turn_id, status);

CREATE TABLE services (
    id TEXT PRIMARY KEY,
    runtime_id TEXT NOT NULL REFERENCES runtimes(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    initiated_by_tool_call_id TEXT NOT NULL,
    impact TEXT NOT NULL CHECK (impact IN ('read_only', 'ignored_output', 'source_writing')),
    command_summary TEXT NOT NULL,
    health_json TEXT,
    executor_process_identity TEXT,
    executor_nonce TEXT NOT NULL,
    log_stream_id TEXT NOT NULL REFERENCES log_streams(id),
    status TEXT NOT NULL CHECK (status IN ('starting', 'running', 'unhealthy', 'stopping', 'stopped', 'stopped_after_restart', 'failed')),
    exit_json TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT
);
CREATE INDEX services_session_idx ON services (session_id, status);

CREATE TABLE runtime_ports (
    id TEXT PRIMARY KEY,
    service_id TEXT NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    protocol TEXT NOT NULL CHECK (protocol IN ('http', 'https', 'tcp')),
    internal_port INTEGER NOT NULL CHECK (internal_port BETWEEN 1 AND 65535),
    health_path TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (service_id, name),
    UNIQUE (service_id, internal_port)
);

CREATE TABLE terminals (
    id TEXT PRIMARY KEY,
    runtime_id TEXT NOT NULL REFERENCES runtimes(id) ON DELETE CASCADE,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('project', 'session')),
    owner_id TEXT NOT NULL,
    executor_pty_identity TEXT,
    executor_nonce TEXT NOT NULL,
    cols INTEGER NOT NULL CHECK (cols BETWEEN 1 AND 1000),
    rows INTEGER NOT NULL CHECK (rows BETWEEN 1 AND 1000),
    scrollback_stream_id TEXT NOT NULL REFERENCES log_streams(id),
    status TEXT NOT NULL CHECK (status IN ('starting', 'running', 'closing', 'exited', 'failed', 'lost')),
    exit_json TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX terminals_owner_idx ON terminals (owner_kind, owner_id, created_at);

CREATE TABLE runtime_access_tickets (
    id TEXT PRIMARY KEY,
    terminal_id TEXT NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    actor_id TEXT NOT NULL,
    origin TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    revoked_at TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX runtime_access_tickets_terminal_idx ON runtime_access_tickets (terminal_id, expires_at);
