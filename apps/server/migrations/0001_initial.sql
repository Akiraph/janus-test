-- janus-module: platform
-- janus-module: identity
-- janus-module: models
-- janus-module: projects
-- janus-module: source-control
-- janus-module: runtime
-- janus-module: sessions
-- janus-module: execution
-- janus-module: workspace
-- janus-module: notifications
--
-- Janus initial schema.
--
-- This file is the single squash of the pre-release migration history
-- (formerly 0001..0024, including several table rebuilds and ALTER chains that
-- only existed to evolve an organic prototype schema). Janus has not shipped to
-- a public platform and no deployed database exists, so a fresh install creates
-- this schema directly; there is no version 0 database to upgrade.
--
-- Applied SQL files are immutable. To evolve the schema, add a new migration
-- with the next version and declare every module that owns a table it touches;
-- tools/xtask validates that a migration only mutates tables owned by the
-- modules it declares (canonical names: supervisor -> execution,
-- workspace-sync -> workspace).
--
-- Dead tables removed by the squash (never read or written by Rust code):
--   runtime_ports, model_recovery_cooldowns, stream_diagnostics.
-- Tables are ordered so that referenced tables precede their referencers.
--

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

CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('upload', 'workspace')),
    upload_id TEXT,
    workspace_path TEXT,
    content_revision_id TEXT,
    name TEXT NOT NULL,
    mime TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    blob_sha TEXT,
    lifecycle TEXT NOT NULL DEFAULT 'draft' CHECK (lifecycle IN ('draft', 'attached')),
    version TEXT NOT NULL,
    created_at TEXT NOT NULL
);

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

CREATE TABLE blob_objects (
    sha256 TEXT PRIMARY KEY,
    byte_size INTEGER NOT NULL,
    storage_state TEXT NOT NULL CHECK (storage_state IN ('incoming', 'present', 'trash')),
    first_written_at TEXT NOT NULL,
    last_verified_at TEXT
);

CREATE TABLE blob_references (
    owner_module TEXT NOT NULL,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    blob_sha TEXT NOT NULL REFERENCES blob_objects(sha256) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (owner_module, owner_type, owner_id, purpose)
);

CREATE TABLE ceremonies (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    state_json TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('pre_turn', 'stable')),
    timeline_position INTEGER,
    workspace_revision_id TEXT NOT NULL,
    source_message_id TEXT,
    source_turn_id TEXT,
    created_at TEXT NOT NULL
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

CREATE TABLE content_revisions (
    revision_id TEXT PRIMARY KEY,
    workspace_handle TEXT NOT NULL REFERENCES workspace_copies(handle) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    manifest_root_hash TEXT,
    cause TEXT NOT NULL,
    actor_json TEXT NOT NULL,
    prev_revision_id TEXT,
    stable INTEGER NOT NULL DEFAULT 1,
    occurred_at TEXT NOT NULL,
    UNIQUE (workspace_handle, sequence)
);

CREATE TABLE context_versions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    sequence INTEGER NOT NULL,
    compact_summary_id TEXT REFERENCES compact_summaries(id) ON DELETE SET NULL,
    estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
    context_limit INTEGER NOT NULL DEFAULT 200000,
    compact_status TEXT NOT NULL DEFAULT 'not_needed'
        CHECK (compact_status IN ('not_needed', 'scheduled', 'running', 'succeeded', 'failed')),
    selection_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (session_id, sequence)
);

CREATE TABLE git_update_conflict_paths (
    conflict_id TEXT NOT NULL REFERENCES git_update_conflicts(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('text', 'binary', 'added', 'deleted', 'mode')),
    base_hash TEXT,
    remote_hash TEXT,
    main_hash TEXT,
    choice TEXT CHECK (choice IN ('main', 'remote', 'delete', 'edited_text')),
    edited_blob_sha TEXT,
    version TEXT NOT NULL,
    PRIMARY KEY (conflict_id, path)
);

CREATE TABLE git_update_conflicts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    base_tree TEXT NOT NULL,
    remote_tree TEXT NOT NULL,
    main_tree TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'applying', 'resolved', 'superseded')),
    operation_id TEXT NOT NULL,
    prev_conflict_id TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE github_credentials (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    github_host TEXT NOT NULL,
    pat_ciphertext BLOB,
    pat_fingerprint TEXT,
    probe_summary_json TEXT,
    state TEXT NOT NULL DEFAULT 'ready',
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE idempotency_records (
    key TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    method TEXT NOT NULL,
    normalized_route TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    status TEXT NOT NULL,
    response_ref TEXT,
    operation_id TEXT,
    expires_at TEXT NOT NULL
);

CREATE TABLE initialization_tokens (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    used_at TEXT,
    created_at TEXT NOT NULL
);

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
    ended_at TEXT,
    cancellation_requested_at TEXT
);

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

CREATE TABLE login_sessions (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE message_attachments (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    PRIMARY KEY (message_id, attachment_id)
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
    actor_json TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'assistant', 'system', 'tool_result_ref')),
    body_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded')),
    timeline_sequence INTEGER,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL
);

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

CREATE TABLE model_failover (
    primary_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    candidate_model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    created_at TEXT NOT NULL,
    PRIMARY KEY (primary_model_id, candidate_model_id),
    UNIQUE (primary_model_id, ordinal),
    CHECK (primary_model_id <> candidate_model_id)
);

CREATE TABLE model_providers (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    display_name TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key_ciphertext BLOB,
    api_key_fingerprint TEXT,
    api_key_preview TEXT,
    models_json TEXT NOT NULL DEFAULT '[]',
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    client TEXT NOT NULL DEFAULT 'supervisor'
);

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

CREATE TABLE notification_channels (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('webhook', 'qqbot')),
    display_name TEXT NOT NULL,
    endpoint_url TEXT NOT NULL,
    secret_ciphertext BLOB,
    target_json TEXT NOT NULL DEFAULT '{}',
    events_json TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE operation_steps (
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    step_key TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    input_summary TEXT NOT NULL DEFAULT '{}',
    external_ref TEXT,
    compensation_json TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (operation_id, step_key)
);

CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    actor_json TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT,
    status TEXT NOT NULL CHECK (status IN (
        'queued', 'running', 'succeeded', 'failed', 'canceled', 'needs_attention'
    )),
    current_step TEXT,
    conditions_json TEXT NOT NULL,
    result_json TEXT,
    problem_json TEXT,
    correlation_id TEXT NOT NULL,
    lease_nonce TEXT,
    lease_expires_at TEXT,
    progress_json TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE owners (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE passkeys (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    credential_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE TABLE plan_versions (
    id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    plan_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    UNIQUE (turn_id, sequence)
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

CREATE TABLE project_git_state (
    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    git_state_version TEXT NOT NULL,
    head_sha TEXT,
    branch TEXT,
    ahead INTEGER NOT NULL DEFAULT 0,
    behind INTEGER NOT NULL DEFAULT 0,
    last_scan_at TEXT NOT NULL,
    version TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

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

CREATE TABLE projection_cursor (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    cursor INTEGER NOT NULL
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('creating', 'ready', 'error', 'deleting')),
    repo_access TEXT NOT NULL CHECK (repo_access IN ('public_https', 'github_private')),
    repo_url TEXT NOT NULL,
    repo_branch TEXT,
    github_credential_id TEXT,
    default_model_id TEXT,
    main_workspace_handle TEXT,
    clone_error TEXT,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_activity_at TEXT NOT NULL
);

CREATE TABLE propagation_links (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL PRIMARY KEY,
    source_branch TEXT NOT NULL,
    initial_main_revision_id TEXT NOT NULL,
    main_to_session_cursor_revision_id TEXT NOT NULL,
    session_to_main_cursor_revision_id TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    baseline_manifest_json TEXT,
    recovery_state TEXT NOT NULL DEFAULT 'idle' CHECK (recovery_state IN ('idle', 'transferring')),
    recovery_intent_json TEXT,
    recovery_error TEXT
);

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

CREATE TABLE recovery_batches (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE recovery_codes (
    id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL REFERENCES recovery_batches(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    used_at TEXT
);

CREATE TABLE recovery_states (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    used_at TEXT
);

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

CREATE TABLE runtimes (
    id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
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
    stopped_at TEXT,
    scope_kind TEXT NOT NULL DEFAULT 'session' CHECK (scope_kind IN ('project', 'session'))
);

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

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'regular' CHECK (kind IN ('regular', 'resolver')),
    parent_session_id TEXT,
    forked_from_checkpoint_id TEXT,
    resolver_conflict_id TEXT,
    title TEXT,
    state TEXT NOT NULL CHECK (state IN ('ready', 'active', 'deleting')),
    workspace_handle TEXT NOT NULL,
    next_model_ref TEXT,
    active_turn_id TEXT,
    source_main_revision_id TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_activity_at TEXT NOT NULL
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

CREATE TABLE timeline_items (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT,
    kind TEXT NOT NULL,
    source_resource_id TEXT,
    display_order INTEGER NOT NULL,
    projection_json TEXT NOT NULL,
    status TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

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
    provider_call_id TEXT,
    UNIQUE (round_id, ord)
);

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

CREATE TABLE uploads (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    original_name TEXT NOT NULL,
    mime TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    blob_sha TEXT,
    scan_status TEXT NOT NULL DEFAULT 'pending',
    expires_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE work_items (
    id TEXT PRIMARY KEY,
    handler_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    not_before TEXT NOT NULL,
    lease_nonce TEXT,
    lease_expires_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    dead INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE workspace_copies (
    handle TEXT PRIMARY KEY,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    session_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('main', 'session')),
    managed_dir TEXT NOT NULL,
    current_revision_id TEXT,
    observation_generation INTEGER NOT NULL DEFAULT 0,
    dirty INTEGER NOT NULL DEFAULT 0,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workspace_mutation_intents (
    id TEXT PRIMARY KEY,
    workspace_handle TEXT NOT NULL REFERENCES workspace_copies(handle) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    expected_revision_id TEXT,
    cause TEXT NOT NULL,
    actor_json TEXT NOT NULL,
    pre_manifest_json TEXT NOT NULL,
    event_json TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'awaiting_event', 'completed', 'needs_attention')),
    observed_manifest_root_hash TEXT,
    revision_id TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workspace_propagation_conflicts (
    session_id TEXT PRIMARY KEY,
    direction TEXT NOT NULL CHECK (direction IN ('sync', 'apply')),
    session_revision_id TEXT NOT NULL,
    main_revision_id TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE workspace_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL UNIQUE REFERENCES content_revisions(revision_id) ON DELETE CASCADE,
    manifest_root_hash TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN (
        'session_create', 'tool_write', 'checkpoint', 'external_scan'
    )),
    integrity_state TEXT NOT NULL CHECK (integrity_state IN ('complete', 'incomplete')),
    created_at TEXT NOT NULL
);

CREATE INDEX asks_turn_idx ON asks (turn_id, status);

CREATE INDEX attachments_session_idx ON attachments (session_id);

CREATE INDEX blob_cleanup_intents_due_idx
    ON blob_cleanup_intents (next_attempt_at, updated_at);

CREATE INDEX blob_objects_state_idx ON blob_objects (storage_state);

CREATE INDEX blob_references_blob_idx ON blob_references (blob_sha);

CREATE INDEX checkpoints_session_idx ON checkpoints (session_id, created_at);

CREATE INDEX compact_summaries_session_idx ON compact_summaries (session_id, created_at);

CREATE INDEX content_revisions_handle_idx ON content_revisions (workspace_handle, sequence);

CREATE INDEX context_versions_session_idx ON context_versions (session_id, sequence);

CREATE INDEX git_update_conflicts_project_idx ON git_update_conflicts (project_id, state);

CREATE UNIQUE INDEX github_credential_name_idx ON github_credentials (owner_id, name);

CREATE INDEX idempotency_records_owner_idx ON idempotency_records (owner_id, expires_at);

CREATE INDEX jobs_session_idx ON jobs (session_id, created_at);

CREATE INDEX jobs_turn_idx ON jobs (controlling_turn_id, status);

CREATE UNIQUE INDEX log_streams_owner_idx ON log_streams (owner_kind, owner_id);

CREATE INDEX message_attachments_attachment_idx ON message_attachments (attachment_id);

CREATE INDEX messages_session_idx ON messages (session_id, created_at);

CREATE INDEX messages_turn_idx ON messages (turn_id);

CREATE INDEX model_attempts_round_idx ON model_attempts (round_id, candidate_order, attempt_number);

CREATE UNIQUE INDEX model_provider_client_name_idx
    ON model_providers(owner_id, client, display_name);

CREATE INDEX model_usage_ledger_attempt_idx ON model_usage_ledger (attempt_id);

CREATE INDEX model_usage_ledger_project_idx ON model_usage_ledger (project_id, occurred_at);

CREATE INDEX model_usage_ledger_session_idx ON model_usage_ledger (session_id, occurred_at);

CREATE INDEX notification_channels_owner_idx
    ON notification_channels (owner_id, enabled, display_name);

CREATE INDEX operations_status_idx ON operations (status);

CREATE INDEX operations_target_idx ON operations (target_kind, target_id);

CREATE UNIQUE INDEX passkeys_credential_idx ON passkeys(credential_json);

CREATE INDEX projects_owner_idx ON projects (owner_id, last_activity_at);

CREATE INDEX projects_state_idx ON projects (state);

CREATE INDEX propagation_links_project_idx ON propagation_links (project_id);

CREATE INDEX public_events_type_cursor_idx
    ON public_events (event_type, cursor);

CREATE INDEX rounds_turn_idx ON rounds (turn_id, sequence);

CREATE INDEX runtime_access_tickets_terminal_idx ON runtime_access_tickets (terminal_id, expires_at);

CREATE UNIQUE INDEX runtimes_one_current_per_scope ON runtimes (scope_kind, scope_id)
WHERE status IN ('starting', 'ready', 'stopping');

CREATE INDEX services_session_idx ON services (session_id, status);

CREATE INDEX sessions_project_idx ON sessions (project_id, last_activity_at);

CREATE INDEX sessions_state_idx ON sessions (state);

CREATE INDEX terminals_owner_idx ON terminals (owner_kind, owner_id, created_at);

CREATE INDEX timeline_items_session_order_idx
    ON timeline_items (session_id, display_order);

CREATE INDEX timeline_items_turn_idx ON timeline_items (turn_id);

CREATE UNIQUE INDEX tool_calls_provider_call_idx
ON tool_calls (round_id, provider_call_id)
WHERE provider_call_id IS NOT NULL;

CREATE INDEX tool_calls_round_idx ON tool_calls (round_id, ord);

CREATE UNIQUE INDEX turns_one_active_per_session ON turns (session_id)
WHERE status IN ('running', 'waiting_for_job', 'waiting_for_ask', 'waiting_for_model', 'canceling');

CREATE INDEX turns_queued_idx ON turns (session_id, sequence) WHERE status = 'queued';

CREATE INDEX turns_session_idx ON turns (session_id, sequence);

CREATE INDEX uploads_owner_idx ON uploads (owner_id, created_at);

CREATE INDEX work_items_claimable_idx ON work_items (dead, not_before) WHERE lease_expires_at IS NULL;

CREATE INDEX workspace_copies_project_idx ON workspace_copies (project_id);

CREATE INDEX workspace_copies_session_idx ON workspace_copies (session_id);

CREATE INDEX workspace_mutation_intents_handle_idx
    ON workspace_mutation_intents (workspace_handle, state);

CREATE INDEX workspace_mutation_intents_recovery_idx
    ON workspace_mutation_intents (state, updated_at);

CREATE INDEX workspace_propagation_conflicts_updated_idx
    ON workspace_propagation_conflicts (updated_at);

CREATE INDEX workspace_snapshots_revision_idx ON workspace_snapshots (revision_id);

