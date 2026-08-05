-- janus-module: workspace-sync
-- recovery: append-only; safe to retry through SQLx migrations

-- Durable journal for filesystem mutations. The effect runs only after the
-- pending row commits; finalization records the revision and clears the row's
-- recovery state in the same short transaction as any caller-owned event.
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
CREATE INDEX workspace_mutation_intents_recovery_idx
    ON workspace_mutation_intents (state, updated_at);
CREATE INDEX workspace_mutation_intents_handle_idx
    ON workspace_mutation_intents (workspace_handle, state);
