# supervisor

Owns rounds, tool calls, context assembly, the tool registry, and Turn completion
decisions. It does **not** own messages, processes/Runtime, or provider credentials.

## M3 ownership

| Kind | Names |
| --- | --- |
| Tables | `rounds`, `tool_calls` (migration `0008_supervisor.sql`) |
| Events | `round.changed`, `tool_call.created`, `tool_call.changed` |
| IDs | `RoundId`, `ToolCallId` |

## Dependencies

Allowed Module dependencies: `models`, `projects`, `runtime`, `sessions`, `workspace-sync`.

- `sessions`: Turn/message projection, completion write-back.
- `models`: Provider stream (`stream_completion`).
- `workspace-sync`: Session file mutations from tools (`fs.write` / patch / remove).
- `projects`: Limited Main metadata when needed for context.
- `runtime`: Interface only in M3 (empty; no process tools).

## Notes

- M3 tools: `fs.list`, `fs.read`, `fs.write`, `fs.patch`, `fs.remove`, `git.inspect`, `finish`.
- No Main handle parameters, no Git write tools.
- Failed stream attempts must not execute Tool Calls.
