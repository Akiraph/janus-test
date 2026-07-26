# sessions

Owns Session identity, Turns, messages, timeline projection, checkpoints, uploads,
and attachments. It does **not** own model rounds, tool execution, provider
credentials, or workspace bytes.

## M3 ownership

| Kind | Names |
| --- | --- |
| Tables | `sessions`, `turns`, `messages`, `timeline_items`, `checkpoints`, `uploads`, `attachments`, `message_attachments` (migration `0007_sessions.sql`) |
| Events | `session.changed`, `session.deleted`, `turn.created`, `turn.status_changed`, `timeline.item_created`, `timeline.item_updated`, `checkpoint.created` |
| IDs | `SessionId`, `TurnId`, `MessageId`, `TimelineItemId`, `CheckpointId`, `UploadId`, `AttachmentId` |

## Dependencies

Allowed Module dependencies: `workspace-sync`, `projects`.

- `workspace-sync`: Session copy create/delete, revision handle, diff summary.
- `projects`: Project existence / Main revision for Session create.

Does not depend on `supervisor` or `models` (sessions triggers turn execution via
the supervisor Interface after writing the Turn projection; supervisor pulls
sessions/models as needed).

## Notes

- HTTP Session lifecycle state: `ready | active | deleting`.
- At most one `running` Turn per Session (partial unique index + transaction check).
- User-message Checkpoint is recorded before Round start; Rewind itself is M6.
