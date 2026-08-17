# Sessions

## Mission

Sessions owns session, turn, message, timeline, checkpoint, upload, and
attachment state. Its public behavior is exposed through `interface.rs`; the
server supplies the Main Workspace implementation and composes execution
workflows.

## Observable behavior

- Session and turn mutations use optimistic versions and active-turn guards, so
  concurrent requests cannot silently replace the current interactive Turn.
- Message and timeline projections are written together with their owning
  session mutation. Timeline cursors therefore describe a stable ordered view
  rather than an independent event stream.
- Uploads become attached only when a message references them. Unreferenced
  uploads and attachment rows can be removed without changing message history.
- Turn recovery and cancellation update Session-owned rows, while execution
  and runtime work are reconciled by the server application workflow.

## Boundaries

`janus-sessions` writes only the historical Session tables: `sessions`,
`turns`, `messages`, `timeline_items`, `checkpoints`, `uploads`,
`attachments`, and `message_attachments`. The server owns migration order and
must preserve those table names and published event names.

`janus-workspace` owns workspace bytes and revision handles. Model rounds,
tool calls, attempts, async_tasks, and runtime state belong to their capability or to
the application workflow that composes them. This crate does not execute model
or runtime work.

## Design decision

Session persistence is a standalone capability. Application code calls its
narrow interface without importing server modules, while cross-capability
creation, deletion, recovery, and execution orchestration remain in
`apps/server/application/`. Sessions never create a private Workspace.
