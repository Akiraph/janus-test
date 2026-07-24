-- janus-module: workspace-sync
-- recovery: append-only; safe to retry through SQLx migrations

-- A physical workspace copy: Main (one per project) or session. `handle` is an
-- opaque path relative to the data root; Main resolves to workspaces/main/<id>/repo/.
-- M2 only populates Main copies; session copies arrive in M3.
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
CREATE INDEX workspace_copies_project_idx ON workspace_copies (project_id);
CREATE INDEX workspace_copies_session_idx ON workspace_copies (session_id);

-- Immutable Content Revision identity. M2 records sequence + a revision_id that
-- serves as the monotone content identity required by WS-003 (sequence + content
-- hash). manifest_root_hash is left NULL until the Merkle manifest collection
-- lands in M3; the revision_id is still unique and monotone.
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
CREATE INDEX content_revisions_handle_idx ON content_revisions (workspace_handle, sequence);
