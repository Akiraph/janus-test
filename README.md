# Janus

English | [简体中文](./README.zh-CN.md)

Janus is a local-first control plane for AI-assisted software work. A single
Rust process owns projects, workspaces, sessions, turns, terminals, and durable
background operations on top of one MongoDB database, and publishes them over a
versioned HTTP + SSE + WebSocket API. Two clients consume that API: the SolidJS
web app in `apps/web`, and `janus-test`, a black-box CLI that speaks only the
public protocol.

## What it does

- **Single-owner passkey authentication.** WebAuthn registration, login,
  passkey management, recovery codes, and recovery-grant exchange. There is one
  owner per deployment.
- **Projects backed by Git.** Clone a repository, browse and edit its file
  tree, and run staged Git commands (status, diff, log, branches, remotes,
  stage, unstage, commit, fetch, push, update) with explicit conflict records
  for updates that cannot fast-forward.
- **Sessions, turns, and timelines.** Messages route to configured model
  providers with failover, usage accounting, attachments, steering, cancel, and
  model-generated context compaction.
- **Terminals.** WebSocket shells bound to a project workspace, authorized by
  one-use tickets, with scrollback replay after reconnect.
- **Durable operations.** Long work runs as journalled Operations with steps,
  work items, idempotency records, and startup recovery instead of
  fire-and-forget tasks.
- **One retained event stream.** Every state change appends to `public_events`
  and is readable over SSE with opaque cursors, bounded replay, and heartbeats.
- **Optional fork-sync automation.** A signed webhook turns fork-sync conflict
  reports into projects and Supervisor sessions that repair the pull requests.

## Requirements

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | `1.97.0` | pinned by `rust-toolchain.toml` with `clippy` + `rustfmt` |
| Bun | `1.3.14` | pinned by `apps/web/package.json` `packageManager` |
| Git | 2.54 or newer | the source-control adapter shells out to system Git |
| MongoDB | 7.x | single-node replica set for multi-document transactions; reached via `JANUS_MONGODB_URI` |

The workspace forbids `unsafe_code` and denies `dbg!`, `todo!`, and
`unwrap()` through Clippy lints declared in the root `Cargo.toml`.

## Quick start

```text
cargo xtask setup
cargo xtask dev
```

`setup` prints the `rustc`, `cargo`, `bun`, and `git` versions and installs web
dependencies with `--frozen-lockfile`. `dev` starts Vite and the control plane
together: the API listens on `http://127.0.0.1:4317`, the web app on
`http://127.0.0.1:5173`, and Vite proxies `/api` (WebSocket included) and
`/health` to the API. Runtime data lands in `.janus-dev/`; set `JANUS_DATA_ROOT`
to move it.

`dev` also needs a reachable MongoDB replica set. The default `JANUS_MONGODB_URI`
(`mongodb://localhost:27017/?replicaSet=rs0`) targets a local `mongo:7`
instance started with `--replSet rs0`; a single non-replica-set `mongod` will
reject the transaction calls.

`cargo xtask dev` also exports `JANUS_PUBLIC_ORIGIN=http://localhost:<port>` and
`JANUS_WEBAUTHN_RP_ID=localhost`, because WebAuthn rejects an IP address as a
relying-party ID. Reach the app through `localhost`, not `127.0.0.1`.

To run a second web/control-plane pair side by side, set `JANUS_BIND`,
`JANUS_PUBLIC_ORIGIN`, `JANUS_WEB_PORT`, and optionally `JANUS_API_TARGET`
before `cargo xtask dev` — for example `127.0.0.1:4318` and `5174`.

### First sign-in

In development mode `JANUS_DEV_AUTH` defaults to `true`, so requests are
authorized without a passkey. That combination is rejected unless the listener
is loopback, and it is rejected outright in production mode.

For a real deployment, claim ownership with an initialization token:

```text
cargo run -p janus-server --bin janus-admin -- issue-initialization-token
cargo run -p janus-server --bin janus-admin -- issue-recovery-token
```

Both commands read the same environment as the server, print one token to
stdout, and exit. The token is spent by `POST /api/v1/auth/initialize/options`
and `/complete`, which register the owner's first passkey.

## Repository layout

```text
apps/server/          deployment composition root, public control plane
  src/application/    cross-capability workflows, workers, recovery
  src/transport/http/ HTTP, SSE, and WebSocket protocol conversion
  src/adapters/       system Git and runtime/process implementations
  src/bin/            generate_openapi, janus-admin
apps/web/             SolidJS client (Vite, Bun, Biome, Playwright)
crates/               capability modules plus janus-infrastructure
tools/xtask/          setup, dev, check, build, generate entry points
tools/test-cli/       janus-test black-box verification CLI
generated/            openapi.json produced from the compiled routes
docs/aegis/           durable design and planning records
scripts/              image deployment script used by CI
```

## Architecture

### Capability modules

Each module in `crates/` declares its contract in `module.toml`: the public
root, the collections it owns, the events it publishes, and the modules it may
depend on. `cargo xtask check architecture` enforces that file.

| Module | Crate | Owns | Publishes | May depend on |
| --- | --- | --- | --- | --- |
| identity | `janus-identity` | `owners`, `initialization_tokens`, `passkeys`, `ceremonies`, `login_sessions`, `recovery_batches`, `recovery_codes`, `recovery_states` | — | — |
| models | `janus-models` | `model_providers`, `models`, `model_failover`, `model_attempts`, `model_usage_ledger`, `automation_settings` | `model_config.changed` | — |
| runtime | `janus-runtime` | `runtimes`, `log_streams`, `async_tasks`, `terminals`, `runtime_access_tickets` | `runtime.changed`, `async_task.changed`, `terminal.changed` | — |
| workspace | `janus-workspace` | `workspace_copies`, `content_revisions`, `workspace_snapshots`, `workspace_mutation_intents` | — | — |
| notifications | `janus-notifications` | `notification_channels` | `notification_channel.changed` | — |
| source-control | `janus-source-control` | `project_git_state`, `git_update_conflicts`, `git_update_conflict_paths` | `git.state_changed`, `git.update_conflict_changed` | workspace |
| projects | `janus-projects` | `projects`, `github_credentials`, `memories` | `project.changed`, `project.main_revision_changed` | runtime, workspace |
| sessions | `janus-sessions` | `sessions`, `turns`, `messages`, `timeline_items`, `checkpoints`, `uploads`, `attachments`, `message_attachments` | `session.changed`, `session.deleted`, `turn.created`, `turn.status_changed`, `timeline.item_created`, `timeline.item_updated`, `checkpoint.created` | workspace |
| execution | `janus-execution` | `rounds`, `tool_calls`, `plan_versions`, `compact_summaries`, `context_versions` | `round.changed`, `tool_call.created`, `tool_call.changed`, `context.changed` | models, projects, runtime, sessions, workspace |

`janus-infrastructure` sits under every module and contains only generic
technical building blocks: IDs and correlation IDs, clocks, the MongoDB
connection and transaction helpers, the public event log, the Operation
journal, work items, idempotency records, Blob storage, encrypted secrets, and
portable process helpers. It contains no work kinds and no server workflows.
Its collections, plus the Operation and Blob collections, belong to the
`platform` owner:
`public_events`, `projection_cursor`, `operations`, `operation_steps`,
`work_items`, `idempotency_records`, `command_idempotency_records`,
`blob_objects`, `blob_references`, `blob_cleanup_intents`.

### Server layers

- `src/application/` is the only composition boundary for cross-capability
  work: transaction ordering, execution scheduling, background workers,
  startup recovery, and resource cleanup. It owns no business tables.
- `src/transport/http/` converts public protocols into capability or
  application calls. Handlers do not write capability tables, retry business
  work, or schedule turns.
- `src/adapters/` implements deployment specifics — system Git, processes,
  terminals — and never decides Session, Turn, or Project outcomes.
- `AppState` wires composition-root resources and exposes narrow capability
  query getters for transports and system tests. New workflows belong in
  `Application`, not in `AppState`.

### Rules enforced by `cargo xtask check architecture`

- Every module exposes `interface.rs` or `interface/mod.rs`, and cross-module
  references must go through that interface path.
- Module dependencies must be declared in `module.toml` and stay acyclic.
- One owner per collection and one publisher per event name.
- Cross-module collection reads are allowed; cross-module writes are not.
  Production code may only write collections its own module owns.
- Collection ownership is declared in `crates/infrastructure/src/schema.rs` and
  checked against every `module.toml`: each collection is exactly one of indexed
  or indexless, has a single declared owner, and production code must call
  `.collection("...")` with an inline string literal — never a bound handle.
- `apps/server/src/ports/` and `crates/ports/` are forbidden; so is a
  `janus-server` dependency in `tools/test-cli/Cargo.toml`.

### Startup and shutdown contract

The order below is a deployment contract, not background housekeeping.

1. Parse configuration from the environment; invalid input aborts the process.
2. `AppState::initialize` opens the MongoDB database (creating the schema
   catalog's collections and indexes), builds the infrastructure and capability
   interfaces, reattaches orphaned main worktrees, then recovers interrupted
   workspace mutations and execution state.
3. `/health/ready` reports 503 while recovery finishes.
4. Remove incoming Blob leftovers from the previous run.
5. Mark every still-`running` Operation as `needs_attention` with
   `OPERATION_INTERRUPTED`, so clients retry instead of guessing outcomes.
6. Mark recovery complete so `/health/ready` returns 200, then append the
   `SystemStarted` event carrying the crate version.
7. Spawn the operation, auto-compaction, async-task delivery, notification, and
   state workers.
8. Bind the listener and serve.
9. On `Ctrl-C` or `SIGTERM`, stop accepting connections, then stop live runtimes
   within a 10-second bound so local process groups do not leak.

## Persistence

State lives in one MongoDB database — by default `janus` on the replica set at
`JANUS_MONGODB_URI` — alongside workspace copies and Blob storage under
`JANUS_DATA_ROOT`. MongoDB has no SQL migrations; the schema is a Rust catalog,
`crates/infrastructure/src/schema.rs`, that declares every collection and its
owning module:

- `COLLECTIONS` lists all 54 collections as `(name, owner)` pairs.
- `INDEXLESS_COLLECTIONS` lists the collections that carry no index and are
  created explicitly at open time (for example `event_seq`, the event-cursor
  counter singleton, and `owners`).
- `index_specs()` maps each indexed collection to its `IndexModel`s; composite
  primary-key tables from SQLite become `_pk` unique indexes, and status
  `IN (...)` partial filters are expanded to `$or`/`$eq` for MongoDB 5–7.

Opening a fresh database is a per-collection `create_indexes` pass (idempotent)
plus explicit creation of the index-less collections; `Database::open` also
seeds the `event_seq` counter. `SCHEMA_VERSION` stays at 4 — the last SQL
migration number — so `/api/v1/system/info` shows no regression. There is no
data migration: existing SQLite stores are not imported and deployments start
fresh.

## Public API

Everything the clients need is under `/api/v1` plus the two health probes, and
every registered route except the web-client fallback is described in
`generated/openapi.json` (title `janus-server`, version `0.1.0`).

Transport conventions:

- Success bodies are wrapped as `{ "data": ... }`.
- Errors are `application/problem+json` with `type`, `title`, `status`, `code`,
  `detail`, and the `request_id`. This holds for requests that never reach a
  handler too — see [Error codes](#error-codes).
- Commands with side effects that are not safely repeatable require a
  client-generated `Idempotency-Key`; mutating single-resource requests carry
  the resource version in `If-Match`.
- Every response carries `X-Request-Id`; `/api/v1/bootstrap` and
  `/api/v1/system/info` also return the current event cursor in
  `X-Janus-Event-Cursor`.
- Authentication is a login-session cookie plus `x-csrf-token`; freshly
  generated recovery codes are returned once in `x-janus-recovery-codes`.

### Platform

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/health/live` | liveness and version |
| GET | `/health/ready` | readiness; 503 until startup recovery completes |
| GET | `/api/v1/bootstrap` | initial client snapshot with event cursor |
| GET | `/api/v1/system/info` | schema version and retained event bounds |
| GET | `/api/v1/events` | SSE stream; opaque cursors, bounded replay, 15s heartbeat |
| GET | `/api/v1/operations/{id}` | durable Operation projection |

### Authentication and owner

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/v1/auth/initialize/options`, `/complete` | spend an initialization token, register the first passkey |
| POST | `/api/v1/auth/passkey/options`, `/complete` | passkey login |
| POST | `/api/v1/auth/logout` | end the login session |
| POST | `/api/v1/auth/recovery/exchange` | exchange a recovery code for a grant |
| POST | `/api/v1/auth/recovery/passkey/options`, `/complete` | register a passkey under a recovery grant |
| GET | `/api/v1/me` | current owner |
| GET/POST/PATCH/DELETE | `/api/v1/me/passkeys...` | list, add, rename, revoke passkeys |
| POST | `/api/v1/me/recovery-codes/regenerate` | issue a new recovery batch |

### Projects, files, and Git

| Method | Path | Purpose |
| --- | --- | --- |
| GET/POST | `/api/v1/projects` | list, create (clone) projects |
| GET/PATCH/DELETE | `/api/v1/projects/{id}` | project projection, update, delete |
| POST | `/api/v1/projects/{id}/retry` | retry a failed project setup |
| GET | `/api/v1/projects/{id}/files/tree`, `/meta`, `/content` | browse the Main workspace |
| PUT | `/api/v1/projects/{id}/files/text` | save file text |
| POST | `/api/v1/projects/{id}/files/move` | move or rename |
| DELETE | `/api/v1/projects/{id}/files` | delete a path |
| GET | `/api/v1/projects/{id}/git/status`, `/diff`, `/log`, `/branches`, `/remotes` | Git projections |
| POST | `/api/v1/projects/{id}/git/commands/{stage,unstage,commit,fetch,push,update}` | Git commands |
| GET | `/api/v1/projects/{id}/git/update-conflicts`, `/{conflict_id}` | conflicts from a non-fast-forward update |
| POST | `/api/v1/projects/{id}/git/update-conflicts/{conflict_id}/resolve` | resolve one conflict |
| GET/POST/PATCH/DELETE | `/api/v1/github-credentials...` | stored credentials, plus `/probe` |

### Sessions and execution

| Method | Path | Purpose |
| --- | --- | --- |
| GET/POST | `/api/v1/projects/{project_id}/sessions` | list, create sessions |
| GET/DELETE | `/api/v1/sessions/{id}` | session projection, delete |
| POST | `/api/v1/sessions/{id}/messages` | post a message and start a Turn |
| GET | `/api/v1/sessions/{id}/timeline` | paged timeline with opaque cursors |
| GET | `/api/v1/sessions/{id}/queued-turns` | pending Turn queue |
| GET/POST | `/api/v1/sessions/{id}/turns/{turn_id}`, `/cancel` | inspect or cancel a Turn |
| POST | `/api/v1/sessions/{id}/steer` | steer an active interactive Turn |
| GET/POST | `/api/v1/sessions/{id}/context`, `/context/compact` | context window and compaction |
| POST/DELETE | `/api/v1/sessions/{id}/attachments...` | upload and remove attachments |

### Models, terminals, tasks, notifications

| Method | Path | Purpose |
| --- | --- | --- |
| GET/POST/PATCH/DELETE | `/api/v1/model-providers...` | provider credentials and models, plus `/probe` |
| GET/POST | `/api/v1/terminals` | list, create terminals |
| POST | `/api/v1/terminals/{id}/tickets` | one-use, origin-bound access ticket |
| GET | `/api/v1/terminals/{id}/connect` | WebSocket upgrade; replay then live I/O with `input`/`resize`/`signal`/`close` frames |
| GET | `/api/v1/terminals/{id}/scrollback` | scrollback bytes after a cursor |
| POST | `/api/v1/terminals/{id}/resize`, `/signal`, `/close` | terminal control |
| GET | `/api/v1/async-tasks`, `/{id}/log` | background task list and log |
| POST | `/api/v1/async-tasks/{id}/cancel` | cancel a background task |
| GET/POST/PATCH/DELETE | `/api/v1/notification-channels...` | channels, plus `/test` |
| GET | `/api/v1/automations` | automation runs and their sessions |
| GET/PATCH | `/api/v1/automation/settings` | provider, model, and reasoning effort for automation |
| GET | `/api/v1/automation/webhook/config` | whether the webhook intake is enabled |
| POST | `/api/v1/automation/webhook` | signed fork-sync intake (disabled by default) |

When `JANUS_WEB_DIST` is set, unmatched paths fall back to the built web client
so one origin serves the API, the health probes, and the SPA.

## Error codes

`code` is the stable part of a failure and the field clients switch on; `status`
and `title` follow from it through the shared map in
`apps/server/src/transport/http/problem.rs`. `detail` is human-readable and is
scrubbed for `INTERNAL_ERROR`, so a classified failure keeps its reason while an
unclassified one stays opaque.

| Group | Codes |
| --- | --- |
| shared | `RESOURCE_NOT_FOUND` (404), `RESOURCE_VERSION_MISMATCH` (412), `PRECONDITION_REQUIRED` (428), `IDEMPOTENCY_KEY_REUSED` (409), `OPERATION_IN_PROGRESS` (409), `VALIDATION_FAILED` (422), `INTERNAL_ERROR` (500) |
| sessions and turns | `SESSION_NOT_FOUND` (404), `ACTIVE_TURN_EXISTS`, `SESSION_DELETING`, `TURN_NOT_INTERACTIVE`, `TURN_TERMINAL` (409), `TIMELINE_CURSOR_INVALID` (422) |
| models and providers | `PROVIDER_AUTH_FAILED`, `PROVIDER_STREAM_FAILED` (502), `MODEL_NOT_CONFIGURED`, `MODEL_CONFIGURATION_FAULT` (422), `MODEL_CONTEXT_EXCEEDED`, `MODEL_CAPABILITY_MISMATCH` (409), `MODEL_UNAVAILABLE` (503), `RATE_LIMITED` (429) |
| tools and media | `TOOL_NOT_ALLOWED`, `TOOL_PATH_INVALID`, `IMAGE_TOO_LARGE`, `UNSUPPORTED_IMAGE` (422) |
| runtimes and terminals | `RESOURCE_BUSY`, `TERMINAL_NOT_WRITABLE` (409), `RUNTIME_UNAVAILABLE`, `ASYNC_TASK_LOST` (503), `TERMINAL_TICKET_INVALID` (401), `TERMINAL_SCROLLBACK_EXPIRED` (410) |
| framework rejections | `METHOD_NOT_ALLOWED` (405), `PAYLOAD_TOO_LARGE` (413), `UNSUPPORTED_MEDIA_TYPE` (415), `REQUEST_REJECTED` (any other 4xx) |

A request rejected before any handler ran — unparsable body, missing query
parameter, wrong method, oversized payload — would otherwise answer with the
framework's plain-text rejection and no code at all. `client_error_envelope`
rebuilds every non-Problem 4xx into the same envelope, keeping the original
status, keeping the framework's text as `detail` because it names the field at
fault, and re-inserting the router's `Allow` header on a 405. A 400 or 422
becomes `VALIDATION_FAILED` and a 404 becomes `RESOURCE_NOT_FOUND`, so those
paths look the same whether or not a handler produced them.

Git commands and Git-backed Operations carry their own codes from
`GitError::code`, mapped to 409 unless the shared map says otherwise:
`GIT_AUTH_FAILED`, `GIT_REMOTE_UNAVAILABLE`, `GIT_REMOTE_NOT_FOUND`,
`GIT_REPOSITORY_NOT_FOUND`, `GIT_REF_NOT_FOUND`, `GIT_NOTHING_TO_COMMIT`,
`GIT_IDENTITY_UNSET`, `GIT_REPOSITORY_LOCKED`, `GIT_NON_FAST_FORWARD`,
`GIT_DIVERGED`, `GIT_INDEX_NOT_EMPTY`, `GIT_CHECKOUT_CONFLICT`, and
`GIT_UPDATE_CONFLICT`, which also writes the conflict records exposed under
`/git/update-conflicts`. Classification reads stdout as well as stderr, because
`git commit` reports the most common failure — nothing staged — on stdout.

Durable work reports `OPERATION_INTERRUPTED` after a restart, and automation
adds `PROJECT_CLONE_FAILED`, `AUTOMATION_TIMED_OUT`, `OPERATION_LEASE_STALE`,
and `FORK_SYNC_PARTIAL_FAILURE` so a clone that never finished is
distinguishable from a pull request the remote rejected.

Tool calls fail inside the timeline rather than over HTTP, and their outcomes
carry their own codes — `TOOL_ARGUMENTS_INVALID` when a streamed call arrived
truncated (the tool is not run), `TOOL_EXECUTION_FAILED`,
`TOOL_SKIPPED_AFTER_BLOCK`, and the `TOOL_EDIT_*` and `TOOL_ATTACHMENT_*`
families from `crates/execution/src/tools/`.

## Generated contract

```text
Rust routes + utoipa annotations
  -> cargo run -p janus-server --bin generate_openapi
  -> generated/openapi.json
  -> openapi-typescript
  -> apps/web/src/generated/api.ts
```

`cargo xtask generate` runs the whole chain. Both generated files are committed
and must never be hand-edited: change the Rust route or DTO, regenerate, then
review the diff. The Docker web stage relies on the committed
`src/generated/api.ts` so the client bundle builds without a Rust toolchain.

## Configuration

The server reads its configuration from the environment at startup and refuses
to start on invalid input. Most of the surface is parsed and validated in
`apps/server/src/config.rs`.

| Variable | Default | Purpose |
| --- | --- | --- |
| `JANUS_BIND` | `127.0.0.1:4317` | listener socket address |
| `JANUS_MODE` | `development` | `development` or `production` |
| `JANUS_DEV_AUTH` | `true` in development, `false` in production | bypass authentication; loopback-only, forbidden in production |
| `JANUS_PUBLIC_ORIGIN` | `http://<bind>` | absolute http(s) origin with no path, query, or fragment; must be https in production |
| `JANUS_WEBAUTHN_RP_ID` | host of the public origin, else `localhost` | WebAuthn relying-party ID |
| `JANUS_WEBAUTHN_RP_NAME` | `Janus` | relying-party display name |
| `JANUS_DATA_ROOT` | `.janus-dev` | database, workspaces, and blobs; resolved to an absolute path |
| `JANUS_MONGODB_URI` | `mongodb://localhost:27017/?replicaSet=rs0` | replica-set connection string; must be `mongodb://` or `mongodb+srv://` |
| `JANUS_MONGODB_DATABASE` | `janus` | database name for the Janus schema |
| `JANUS_WEB_DIST` | unset | directory of the built web client to serve same-origin |
| `JANUS_MASTER_KEY` | unset | base64url-without-padding key decoding to exactly 32 bytes; encrypts stored secrets. Required in production; development generates and reuses `<data root>/development-master.key` |
| `JANUS_AUTOMATION_WEBHOOK_ENABLED` | `false` | enable `/api/v1/automation/webhook` |
| `JANUS_AUTOMATION_WEBHOOK_SECRET` | unset | required when the webhook is enabled |
| `JANUS_AUTOMATION_GITHUB_TOKEN` | unset | classic PAT for private clones and `gh` pushes |
| `RUST_LOG` | `janus=info` | tracing filter; logs are emitted as JSON |

Rejected combinations include development auth in production mode, development
auth on a non-loopback bind, a non-https public origin in production, and an
enabled webhook without a secret. The SSE heartbeat is fixed at 15 seconds.
`JANUS_MASTER_KEY` is read by `crates/infrastructure/src/secrets.rs` rather than
by `Config`, so a production process without it fails during initialization.

Variables read outside the server process: `JANUS_WEB_PORT` (default `5173`) and
`JANUS_API_TARGET` (default `http://127.0.0.1:4317`) configure the Vite dev
server and its proxy, `JANUS_BASE_URL` (default `http://127.0.0.1:4317`) points
`janus-test` at a running service, and `JANUS_WEB_URL` (default
`http://127.0.0.1:5173`) is the Playwright base URL.

## Web client

`apps/web` is a SolidJS app built with Vite, type-checked by TypeScript, linted
and formatted by Biome, and tested with Playwright. It uses `@solidjs/router`,
`@tanstack/solid-query`, and `@xterm/xterm` for terminals.

```text
src/app/          shell, routing, layout
src/lib/          transport (api.ts), queries, event stream, model stream,
                  WebAuthn, viewport, and shared utilities
src/components/   reusable visuals with no business vocabulary
src/features/     auth, automation, execution, file-editor, models,
                  notifications, projects, security, session, source-control,
                  system, terminal
src/generated/    api.ts, generated from generated/openapi.json
```

Layer rules: `features/` owns business layout and interaction and is grouped by
capability; `components/` holds genuinely reused visuals; `lib/` holds the
transport, cursor, error, and generated-type foundations. Components never
assemble HTTP requests themselves, there is no forwarding `pages` layer, and
domain state machines are not reimplemented in the client.

| Script | Command |
| --- | --- |
| `dev` | `vite --host 127.0.0.1` |
| `build` | `vite build` |
| `preview` | `vite preview --host 127.0.0.1 --port 4173` |
| `typecheck` | `tsc --noEmit` for the app and the E2E project |
| `lint` / `format` | `biome check` / `biome check --write` |
| `generate:types` | `openapi-typescript ../../generated/openapi.json -o src/generated/api.ts` |
| `test:e2e` | `playwright test` |
| `test:e2e:live` | build `janus-server` + `janus-test`, then run `live-execution.spec.ts` |

## Accessibility

The client is meant to be usable with a keyboard and a screen reader, not only
with a pointer. The conventions below are contracts: new UI is expected to
follow them rather than reintroduce the patterns they replaced.

**Focus is always visible.** Every interactive surface — inputs, select
triggers, both textareas, tree rows, buttons, links — carries a real `outline`
on `:focus-visible`, coloured `var(--text)`. `--shadow-focus` still exists but
is elevation, not an indicator: its 8–18% alpha is far below the 3:1 contrast
WCAG 1.4.11 asks for, and `--accent-strong` measures roughly 1.6:1 against
`--surface`, so neither can carry focus alone. Inputs use
`outline-offset: 1px`; textareas and tree rows use a negative offset so the ring
reads inside a borderless surface or a scroll container. Two `outline: none`
declarations remain in the client, each paired with a `:focus-visible` ring
immediately below it.

**Dialogs trap and restore focus.** `components/ui/Dialog.tsx` moves focus into
the dialog on mount and back to the opener on cleanup, wraps Tab and Shift-Tab
inside it, closes on Escape, and wires `aria-labelledby`/`aria-describedby` with
`createUniqueId`. `description` is a required prop, so no dialog can ship
without stating its consequence. Destructive and irreversible actions —
deleting a provider or channel, regenerating recovery codes, pushing to a
remote — go through a dialog that says what will happen and whether Janus can
undo it, never through native `confirm()`.

**Composite widgets have a keyboard model.** The file explorer is a
`role="tree"` with a roving tabindex: ArrowDown and ArrowUp walk the flattened
list of *visible* rows, ArrowRight expands or descends, ArrowLeft collapses or
climbs to the parent, Home and End jump, Enter and Space activate. Depth is
carried by `aria-level` on each row, and collapsing the branch that held focus
falls back to the first row instead of leaving the tree with no tab stop. The
session tab strip and the workspace document tabs are
`role="tablist"`/`tab`/`tabpanel`, also with a roving tabindex. Every
collapsible trigger declares `aria-expanded` and `aria-controls`.

**Status is announced, and never colour alone.** Errors are `role="alert"`,
progress is `role="status"`, and the notification container is
`aria-live="polite"`. Ahead/behind counts, queued messages, and async-task rows
carry a text equivalent next to the glyph. Decorative icons and spinners are
`aria-hidden="true"` so a screen reader reads "Saving" once rather than "Saving
image". The streamed transcript is deliberately `aria-live="off"`:
per-delta announcements would flood a screen reader during a turn.

**In-flight and failed states are legible.** Long-running rows show which
operation is running rather than only looking disabled, triggers are disabled
while their request is open, load failures offer Retry, unsaved buffers save
with Ctrl+S / Cmd+S and warn on `beforeunload`, and a save that lost an
optimistic-concurrency race (`RESOURCE_VERSION_MISMATCH`) says to reopen the
file and redo the edit instead of reporting a generic failure.

`cargo xtask check` covers this only as far as Biome's accessibility rules,
`tsc`, and `vite build` reach; the Playwright suite is not part of the gate and
there is no component test for focus behaviour. Whether focus lands where
intended and whether a screen reader reads what we think it reads is verified by
review and by hand. Note also that `styles/tokens.css` defines a light palette
only — there is no dark theme to contrast-check.

## Verification

`cargo xtask check` is the single quality gate, and it runs in that order:
architecture check, `cargo check --workspace --all-targets --keep-going` (a
type/borrow gate ahead of codegen that reports errors from independent crates
in one run), OpenAPI + client type generation, `cargo fmt --check`,
`cargo clippy --workspace --all-targets --keep-going -- -D warnings`,
`cargo test --workspace --no-fail-fast`, then web `typecheck`, `lint`, and
`build`.
`cargo xtask` is the alias for `cargo run --package xtask --` declared in
`.cargo/config.toml`.

| Purpose | Command |
| --- | --- |
| verify toolchain, install web deps | `cargo xtask setup` |
| run server + web together | `cargo xtask dev` |
| architecture boundaries only | `cargo xtask check architecture` |
| regenerate OpenAPI and client types | `cargo xtask generate` |
| full quality gate | `cargo xtask check` |
| release build (workspace + web bundle) | `cargo xtask build` |
| single crate tests | `cargo test -p <crate>` |
| browser end-to-end | `bun run --cwd apps/web test:e2e` |
| whitespace check | `git diff --check` |

### Black-box CLI

`janus-test` verifies a running service through the public HTTP, SSE, and
WebSocket surface only. It never opens the MongoDB data store and must never
depend on `janus-server`; the architecture check enforces that. Point it
somewhere with `--base-url` or `JANUS_BASE_URL`.

```text
cargo run -p janus-test -- health
cargo run -p janus-test -- request GET /api/v1/system/info
cargo run -p janus-test -- events follow --count 1
```

| Subcommand | Notable arguments |
| --- | --- |
| `health` | — |
| `request <METHOD> <PATH>` | `--json <file>`, `-H "Name: value"` |
| `events follow` | `--after <cursor>`, `--count <n>` |
| `events range` | `--after`, `--until`, `--limit` (default 256) |
| `projects list \| create \| get \| git-status` | `--name`, `--url`, `--branch`, `--idempotency-key` |
| `sessions list \| create \| get \| delete \| post-message \| timeline \| get-turn \| steer \| cancel` | `--expected-version`, `--idempotency-key`, `--before/--after/--limit`, `--reason` |
| `terminal create \| list \| ticket \| scrollback \| resize \| signal \| close` | `--after`, `--limit`, `cols rows`, `ctrl_c \| terminate` |
| `operations get \| wait` | `--timeout-seconds` (120), `--poll-millis` (250) |

Ordinary runs use a deterministic test provider. Real providers, credentials,
streaming, retries, failover, latency, and cost belong to an explicit smoke
flow; external tokens must never leak into ordinary tests, logs, or commits.

## Continuous integration

| Workflow | Trigger | Jobs |
| --- | --- | --- |
| `.github/workflows/quality.yml` | push to `main`, every pull request | pin Rust `1.97.0` + Bun `1.3.14`, start a `mongo:7` replica-set service container, `bun install --frozen-lockfile`, set a git identity for tests that commit, then `cargo xtask check` |
| `.github/workflows/ci.yml` | pull requests, pushes to `main`/`master`/`dev`, manual | build the Docker image for verification on PRs; on pushes by allowed actors, publish `linux/amd64` tags to GHCR and run the deployment script |

Published tags are `<ref>-amd64` and `<short-sha>-amd64` under
`ghcr.io/<owner>/<repo>`, with a `:cache` tag for the buildx registry cache.
`scripts/deploy_image.js` then pulls a tag onto the target host over SSH and
recreates each container named in `CONTAINER_NAMES`, inheriting its previous
`docker inspect` configuration. It reads the `SERVER_ADDRESS`, `USERNAME`,
`PORT`, `PRIVATE_KEY`, `CONTAINER_NAMES`, and `ADMIN_PASSWORD` secrets, and
redacts key material from its own logs. Commits whose message contains `deps):`
are skipped.

GitHub Actions is the authority on correctness: check `gh run list` and
`gh run view --log-failed` rather than treating a local pass as a green build.

## Container deployment

The `Dockerfile` produces one image that runs frontend and backend as a single
process: stage 1 builds the web bundle with Bun, stage 2 builds `janus-server`
with `rust:1.97.0-bookworm`, and the Debian slim runtime keeps `git` (for the
source-control adapter) and `tini` (to reap spawned session and terminal
processes).

```text
docker build -t janus:local .
docker run --rm -p 4317:4317 -v janus-data:/data \
  -e JANUS_MONGODB_URI=mongodb://host.docker.internal:27017/?replicaSet=rs0 \
  janus:local
```

Image defaults are `JANUS_BIND=0.0.0.0:4317`, `JANUS_DEV_AUTH=false`,
`JANUS_DATA_ROOT=/data`, and `JANUS_WEB_DIST=/app/web`; `/data` is a volume,
the process runs as the unprivileged `janus` user, and port 4317 serves the API,
the health probes, and the web client. A reachable MongoDB replica set is
required — point `JANUS_MONGODB_URI` at it (`host.docker.internal` reaches a
replica set running on the host). A real deployment additionally sets
`JANUS_MODE=production` and an https `JANUS_PUBLIC_ORIGIN` matching the public
hostname.

`janus-admin` ships in the image next to `janus-server`, so administration
tokens are issued from the deployed image rather than from a checkout. It opens
the data root exclusively, so run it as a one-off container against the same
volume while the server container is stopped, with the same environment the
server gets — `Config::from_env` validates the whole configuration, so a
production run still needs `JANUS_MASTER_KEY` and an https
`JANUS_PUBLIC_ORIGIN`.

```text
docker run --rm -v janus-data:/data \
  -e JANUS_MODE=production \
  -e JANUS_PUBLIC_ORIGIN=https://janus.example.com \
  -e JANUS_MASTER_KEY="$JANUS_MASTER_KEY" \
  janus:local janus-admin issue-initialization-token
```

## Fork-sync automation

The webhook intake is disabled by default. Enable it with
`JANUS_AUTOMATION_WEBHOOK_ENABLED=true` and a non-empty
`JANUS_AUTOMATION_WEBHOOK_SECRET`; startup fails if the secret is missing.

`POST /api/v1/automation/webhook` requires `Content-Type: application/json` and
the secret in either `X-Janus-Webhook-Secret` or `Authorization: Bearer ...`,
compared in constant time. The body is the fixed `fork_sync_conflict` contract:
`event`, `timestamp`, `summary`, an optional `github_credential_id`, and a
non-empty `conflicts` array whose items carry `fullName`, `htmlUrl`, `prUrl`,
`defaultBranch`, `parentDefaultBranch`, and optionally `parentFullName` and
`message`. Repository and pull-request URLs are canonicalized to `github.com`
and anything else is rejected with `422`.

A valid request is accepted with `202` and an Operation projection. The
Operation clones each repository, creates a project and a Supervisor session,
and processes the report's repositories serially under one lease, with
deterministic child idempotency keys so a reclaimed work item resumes safely
after a restart. No email body or token is persisted as executable prompt
input.

For private repositories and `gh`-based pushes, set
`JANUS_AUTOMATION_GITHUB_TOKEN` to a GitHub classic PAT. It is stored only as an
encrypted project credential and exposed to the managed turn as
`GH_TOKEN`/`GITHUB_TOKEN`, and the injected task runs `gh auth setup-git`
before pushing. Leave it unset for public repositories or supply an existing
project credential id in the payload. The provider, model, and reasoning effort
used by automation sessions are owner settings under
`/api/v1/automation/settings`, defaulting to `high` reasoning effort.

## Documentation

Directory READMEs are the primary reference for each boundary:
`crates/README.md` for module ownership, `apps/server/README.md` and the
`src/*/README.md` files for the server layers, and `apps/web/src/*/README.md`
for the client layers. Durable design and planning decisions live in
`docs/aegis/`, indexed by `docs/aegis/INDEX.md` with baselines under
`baseline/` and designs under `specs/`.

## Conventions

- Change the source, then regenerate: never hand-edit `generated/openapi.json`
  or `apps/web/src/generated/api.ts`.
- Keep capability logic behind the owning module's `interface.rs`, and put
  cross-capability behavior in `Application` rather than in handlers or
  `AppState`.
- Prefer three repeated lines over an unproven repository, service locator, or
  global event bus, and do not silence lint or boundary failures with blanket
  attributes.
- Commit messages follow Conventional Commits and explain why the change was
  made.

## License

AGPL-3.0-or-later. See `LICENSE`.
