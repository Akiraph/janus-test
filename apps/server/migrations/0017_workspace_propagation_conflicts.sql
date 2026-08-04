-- janus-module: workspace-sync
-- recovery: one pending row per Session; retrying a propagation replaces the row

-- A conflict is kept until the Session workspace has been edited and Apply
-- succeeds. The file snapshot itself remains outside the database under the
-- Session's managed data root; this row only carries the hashes needed to
-- recognize a post-resolution edit.
CREATE TABLE workspace_propagation_conflicts (
    session_id TEXT PRIMARY KEY,
    direction TEXT NOT NULL CHECK (direction IN ('sync', 'apply')),
    session_revision_id TEXT NOT NULL,
    main_revision_id TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX workspace_propagation_conflicts_updated_idx
    ON workspace_propagation_conflicts (updated_at);
