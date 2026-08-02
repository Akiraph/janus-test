# Workspace

## Mission

`janus-workspace` owns Main and Session workspace copies, content revisions,
snapshots, manifests, diffs, and controlled file mutations. New callers use
`interface.rs`; Project, Session, and Execution do not read Workspace tables.
The remaining Project editor write path is a compatibility boundary that still
needs to move its filesystem mutation behind this interface.

## Observable behavior

- Main and Session handles are opaque strings with stable `main:` and `session:`
  prefixes. Project and Session identifiers remain outside this crate's type
  dependencies.
- A Session copy is created from the current Main Git-managed tree. If Main has
  uncommitted or untracked content, that content is seeded into the Session
  copy before its first manifest is recorded.
- Session seeding recreates symlinks instead of following them. Manifest walks
  skip symlinks and other special filesystem entries.
- Every accepted content mutation creates a new revision identity. Returning to
  an earlier byte set still creates a distinct revision; the manifest hash may
  repeat.
- An expected revision is checked again when the revision transaction runs. A
  mismatch returns an error and does not advance the revision identity; callers
  must re-read before retrying.
- Workspace-relative paths reject absolute roots, drive or UNC prefixes, NULs,
  and `..` traversal. `.git` paths are not editable through file mutations.

## Invariants

- `workspace_copies`, `content_revisions`, `workspace_snapshots`, and
  `propagation_links` are owned here. Historical table names, event strings,
  and migration ownership remain unchanged during the crate move.
- Manifest identity excludes symlink targets and special filesystem entries;
  callers must not rely on those entries being materialized as regular files.
- Revision changes and their manifest roots are written through the Workspace
  interface. Cross-module event composition remains in the server workflow.
- Session cleanup is idempotent and removes both the registered copy and its
  managed directory without touching Main or Runtime state.

## Boundaries

Project owns project metadata, credentials, and runtime policy. Source Control
owns Git status, fetch, update, conflict, and commit protocol. Session and
Execution own their conversations and lifecycle state. Application workflows
compose these capabilities; HTTP only maps public requests and responses.

The current implementation uses Git worktree lifecycle operations and the
managed filesystem to materialize copies. That mechanism is private to the
Workspace implementation; Git protocol behavior and cross-capability decisions
must remain outside this crate.

## Design decisions

- Merkle manifests are collected from managed files and use the infrastructure
  BlobStore for content-addressed blobs. Session creation may use a cached Main
  HEAD manifest, but dirty Main paths are rehashed after seeding.
- File mutation currently performs a full manifest rescan. This keeps revision
  roots correct while leaving incremental rehashing as an implementation
  optimization rather than a caller-visible contract.
