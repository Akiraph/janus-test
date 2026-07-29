# workspace-sync

Owns physical workspace copies (`workspace_copies`), immutable Content Revision
identity (`content_revisions`), Session Merkle snapshots (`workspace_snapshots`),
and bidirectional propagation cursor rows (`propagation_links`). It does **not**
own user Git operations, Session messages, or Apply/Sync execution.

## Ownership

| Kind | Names |
| --- | --- |
| Tables | `workspace_copies`, `content_revisions` (migration `0005`); `workspace_snapshots`, `propagation_links` (migration `0006_workspace_sync_session.sql`) |
| Events | `session.revision_changed`, `workspace.diff_changed` |
| IDs | `RevisionId` (platform), `SnapshotId` (snapshot rows) |

## Dependencies

Allowed Module dependency: `projects` (restricted Main queries for copy source /
diff base).

## Notes

- Session directory: `data/workspaces/sessions/<session-id>/repo/`.
- Handle: `session:<session-id>` (symmetric to Main `main:<project-id>`).
- A Session is a Git worktree seeded from Main's current tracked changes,
  deletions, and non-ignored untracked files; ignored files are excluded.
- `content_revisions.manifest_root_hash` records Managed Content identity on
  Session creation and managed tool writes; a recoverable manifest/blob graph
  is not yet persisted.
- Diff uses Git to discover candidate paths and compares their current bytes;
  it does not persist request-time snapshots.
- Diff summary is read-only; Apply/Sync execution is not implemented here.
