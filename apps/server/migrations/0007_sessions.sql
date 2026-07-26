-- janus-module: sessions
-- recovery: append-only; safe to retry through SQLx migrations

-- Session projection (regular kind only in M3; resolver arrives in M6).
-- state is the docs/03 lifecycle: ready | active | deleting.
-- active_turn_id is a soft pointer (no FK — turns already reference sessions).
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
CREATE INDEX sessions_project_idx ON sessions (project_id, last_activity_at);
CREATE INDEX sessions_state_idx ON sessions (state);

-- Turn within a Session. M3 statuses: running | completed | failed.
-- Partial unique index enforces at most one running Turn per Session.
CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    input_message_id TEXT,
    model_snapshot_json TEXT NOT NULL,
    completion_summary_json TEXT,
    completion_reason TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (session_id, sequence)
);
CREATE INDEX turns_session_idx ON turns (session_id, sequence);
CREATE UNIQUE INDEX turns_one_running_per_session
    ON turns (session_id) WHERE status = 'running';

-- Message projection. body_json holds structured content parts; actor_json is the
-- actor envelope (owner / system / tool).
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
CREATE INDEX messages_session_idx ON messages (session_id, created_at);
CREATE INDEX messages_turn_idx ON messages (turn_id);

-- Unified timeline projection for the Session UI (messages / rounds / tool_calls
-- and other display items). display_order supports before/after cursor pages.
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
CREATE INDEX timeline_items_session_order_idx
    ON timeline_items (session_id, display_order);
CREATE INDEX timeline_items_turn_idx ON timeline_items (turn_id);

-- Checkpoint identity before a user message / Turn (M3 records only; Rewind is M6).
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
CREATE INDEX checkpoints_session_idx ON checkpoints (session_id, created_at);

-- Transient upload staging (image attachments before attach to a Session message).
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
CREATE INDEX uploads_owner_idx ON uploads (owner_id, created_at);

-- Session-scoped attachment identity (from upload or workspace path).
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
CREATE INDEX attachments_session_idx ON attachments (session_id);

-- Join table: which attachments ride on a message, in order.
CREATE TABLE message_attachments (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    PRIMARY KEY (message_id, attachment_id)
);
CREATE INDEX message_attachments_attachment_idx ON message_attachments (attachment_id);
