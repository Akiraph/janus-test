-- janus-module: workspace-sync
-- recovery: append-only; safe to retry through SQLx migrations

-- Immutable snapshot of a Content Revision's Merkle root. M3 fills
-- content_revisions.manifest_root_hash on Session create and each managed write;
-- workspace_snapshots records purpose + integrity for each collected root.
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
CREATE INDEX workspace_snapshots_revision_idx ON workspace_snapshots (revision_id);

-- Bidirectional propagation cursors between Project Main and a Session copy.
-- M3 only initializes rows (cursors equal the Session create pair); Apply/Sync
-- advancement is M5. PK is session_id (one link per Session).
CREATE TABLE propagation_links (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL PRIMARY KEY,
    source_branch TEXT NOT NULL,
    initial_main_revision_id TEXT NOT NULL,
    main_to_session_cursor_revision_id TEXT NOT NULL,
    session_to_main_cursor_revision_id TEXT NOT NULL,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX propagation_links_project_idx ON propagation_links (project_id);
