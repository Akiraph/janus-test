# Workspace

## Mission

`janus-workspace` owns Main and Session workspace copies, content revisions,
snapshots, manifests, diffs, propagation state, and controlled file mutations.
New callers use `interface.rs`; Project, Session, and Execution do not read
Workspace tables.
Project editor writes use the same guarded mutation path as Session tool writes.

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
- Diff summaries expose whether Main-to-Session or Session-to-Main propagation
  is possible. Propagation copies filesystem changes without creating a Git
  commit and returns a structured conflict when both sides changed one path.
- A propagation conflict remains durable until the Session is edited and a
  subsequent Apply succeeds, so a failed HTTP request or process restart does
  not lose the files that still need resolution.
- Apply/Sync records a durable path intent before filesystem transfer. Startup
  replays an interrupted intent only while both source and target paths still
  match their recorded preimages; an unexpected edit becomes a durable
  propagation conflict instead of being overwritten.
- Workspace-relative paths reject absolute roots, drive or UNC prefixes, NULs,
  and `..` traversal. `.git` paths are not editable through file mutations.

## Invariants

- `workspace_copies`, `content_revisions`, `workspace_snapshots`,
  `propagation_links`, `workspace_propagation_conflicts`, and
  `workspace_mutation_intents` are owned here.
  Historical table names, event strings, and migration ownership remain
  unchanged during the crate move.
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
compose these capabilities and enforce Session lifecycle/event ordering; HTTP
only maps public requests and responses.

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
- The three-way propagation baseline is a persisted manifest outside the Git
  worktree. New Sessions store it beside the propagation cursors instead of
  copying the entire tree; legacy filesystem baselines are read once and
  migrated lazily. It advances only where Main and Session agree after a
  successful propagation.
- A per-project mutation guard serializes Main edits, Session writes, copy
  lifecycle, diff scans, and propagation inside one process. The revision
  precondition and durable propagation intent remain the cross-process safety
  boundary.
