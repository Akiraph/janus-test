# workspace-sync

Owns physical workspace copies (`workspace_copies`), immutable Content Revision
identity (`content_revisions`, migration `0005_workspace_sync.sql`), and (in
later milestones) the three-way Apply/Sync, propagation cursors, conflicts and
atomic file recovery. It does not own user Git operations or Session messages.

M2 scope: only Main copies and the revision identity record (sequence +
revision_id, no manifest yet). Session copies, Merkle manifest collection,
Apply/Sync and Checkpoints arrive in M3/M5/M6.

Allowed Module dependency: `projects`.
