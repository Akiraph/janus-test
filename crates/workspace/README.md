# Workspace

## Mission

`janus-workspace` owns the Main workspace copy, content revisions, snapshots,
manifests, and controlled file mutations. New callers use `interface.rs`;
Project, Session, and Execution do not read Workspace tables.
Project editor writes use the same guarded mutation path as tool writes.

## Observable behavior

- Main handles are opaque strings with a stable `main:` prefix. Project
  identifiers remain outside this crate's type dependencies.
- The Main copy is created by the application source-control workflow and its
  first revision is recorded by this capability.
- Manifest walks skip symlinks and other special filesystem entries.
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
  `workspace_mutation_intents` are owned here.
  Historical table names, event strings, and migration ownership remain
  unchanged during the crate move.
- Manifest identity excludes symlink targets and special filesystem entries;
  callers must not rely on those entries being materialized as regular files.
- Revision changes and their manifest roots are written through the Workspace
  interface. Cross-module event composition remains in the server workflow.
- Main cleanup is coordinated by the application source-control workflow;
  orphaned clone directories are removed during startup recovery.

## Boundaries

Project owns project metadata, credentials, and runtime policy. Source Control
owns Git status, fetch, update, conflict, and commit protocol. Session and
Execution own their conversations and lifecycle state. Application workflows
compose these capabilities and enforce Session lifecycle/event ordering; HTTP
only maps public requests and responses.

The managed filesystem materializes the Main clone. That mechanism is private
to the Workspace implementation; Git protocol behavior and cross-capability
decisions remain outside this crate.

## Design decisions

- Merkle manifests are collected from managed files and use the infrastructure
  BlobStore for content-addressed blobs.
- File mutation currently performs a full manifest rescan. This keeps revision
  roots correct while leaving incremental rehashing as an implementation
  optimization rather than a caller-visible contract.
- A per-project mutation guard serializes Main edits and tool writes inside one
  process. The revision precondition remains the cross-process safety boundary.
