# runtime

Owns Runtime, finite Job, Session Service, Terminal, port registration, access-ticket, and bounded log-stream lifecycles. It defines executor-neutral specifications and projections while Local and Linux Container behavior lives in adapters.

Runtime owns process resources and recovery state. It does not decide Turn completion, inspect model credentials, or own workspace propagation state.

## Terminal Backend (M4 Stage 3)

Terminals use a **pipe backend**, not a PTY/ConPTY backend. The Local executor
spawns `bash` (git bash on Windows, `/bin/bash` elsewhere) with stdin/stdout
/stderr as plain pipes and `TERM=dumb`, then streams stdout/stderr into a
bounded `LogStore` scrollback stream (`LogRetention::TERMINAL`).

- There is no ConPTY, no Win32 console host, and no unsafe FFI. The cost is
  that `Ctrl-C` is delivered as the byte `0x03` to stdin and resize cannot
  propagate to a non-tty shell — the durable projection still records the new
  size. This keeps the cross-platform contract identical.
- Access tickets are 32-byte random URL-safe tokens; only a SHA-256
  `purpose_hash` digest is persisted. Tickets are bound to the issuing actor
  id and the `Origin` header, expire in 30 seconds, and are consumed
  atomically. The raw token is returned once and never stored.
- A WebSocket connect replays scrollback from the requested cursor, then polls
  the scrollback projection to ship live bytes. Binary frames carry PTY output
  and raw input; JSON control frames carry `input`/`resize`/`signal`/`close`.
- Session Terminal writes do not auto-advance `workspace_revision` yet; the
  dirty/reconcile wiring ships with the M4/M5 Apply/Sync work that owns
  propagation cursors.

## Recovery And Deletion

- `recover_uncertain` (called from `AppState::initialize` before readiness) marks
  every in-flight Runtime/Job/Service/Terminal `lost` / `stopped_after_restart`
  and revokes outstanding access tickets. It never restarts process groups or
  re-runs model/Bash/CLI work — restart recovery is terminal, not a replay.
- Session/Project deletion stops Runtime resources first via
  `application::lifecycle` (cancel Jobs, stop Services, close Terminals, stop
  Runtime) and only then removes durable rows / workspace copies. Project
  deletion also closes Project-owned (Main) Terminals before the Main workspace
  is removed.
- Graceful process shutdown (`application::lifecycle::graceful_shutdown`) walks
  live Runtimes with a wall-clock deadline so Local process groups do not leak
  across control-plane exits.

