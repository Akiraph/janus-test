-- janus-module: projects
-- recovery: append-only; safe to retry through SQLx migrations

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES owners(id) ON DELETE CASCADE,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
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
CREATE INDEX projects_owner_idx ON projects (owner_id, last_activity_at);
CREATE INDEX projects_state_idx ON projects (state);

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
CREATE UNIQUE INDEX github_credential_name_idx ON github_credentials (owner_id, name);

-- Recomputable current Git projection (HEAD/branch/ahead-behind). git_state_version
-- is the opaque version clients send back as conditions; it advances on any Git
-- mutation so stale previews are rejected.
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

-- Persistent Git Update Conflict (WS-GIT-06/08). Owned by projects, not the
-- session Resolver flow. State: open -> applying -> resolved; superseded when
-- the fixed base/main inputs change before completion.
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
CREATE INDEX git_update_conflicts_project_idx ON git_update_conflicts (project_id, state);

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
