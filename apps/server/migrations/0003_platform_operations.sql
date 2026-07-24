-- janus-module: platform
-- recovery: append-only; safe to retry through SQLx migrations

-- Durable Operation journal: long-running commands (clone, git fetch/update/push,
-- project delete) survive HTTP disconnects and process restarts. See DAT-OP-01/02.
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
CREATE INDEX operations_status_idx ON operations (status);
CREATE INDEX operations_target_idx ON operations (target_kind, target_id);

-- Idempotent step records inside an operation. Stable step keys let handlers
-- re-enter after a crash and skip already-succeeded steps.
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

-- Persistent work queue backed by SQLite (no Redis/scheduler). handler_kind is
-- one of: clone, fetch, update, push, delete_project, gc_objects.
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
CREATE INDEX work_items_claimable_idx ON work_items (dead, not_before) WHERE lease_expires_at IS NULL;

-- Idempotency for POST/DELETE commands with external side effects. Key is scoped
-- by owner, HTTP method and normalized route; same key + same digest returns the
-- original resource/operation, same key + different digest returns a 409.
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
CREATE INDEX idempotency_records_owner_idx ON idempotency_records (owner_id, expires_at);

-- Content-addressed object store. blob_objects is the physical bytes (one row
-- per unique SHA-256); blob_references tracks logical owners so GC is mark-and-sweep
-- over a real graph, not a refcount that lies on crash.
CREATE TABLE blob_objects (
    sha256 TEXT PRIMARY KEY,
    byte_size INTEGER NOT NULL,
    storage_state TEXT NOT NULL CHECK (storage_state IN ('incoming', 'present', 'trash')),
    first_written_at TEXT NOT NULL,
    last_verified_at TEXT
);
CREATE INDEX blob_objects_state_idx ON blob_objects (storage_state);

CREATE TABLE blob_references (
    owner_module TEXT NOT NULL,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    purpose TEXT NOT NULL,
    blob_sha TEXT NOT NULL REFERENCES blob_objects(sha256) ON DELETE RESTRICT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (owner_module, owner_type, owner_id, purpose)
);
CREATE INDEX blob_references_blob_idx ON blob_references (blob_sha);
