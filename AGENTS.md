# Janus Repository Instructions

## Public repository boundary

- This repository must be understandable on its own. Do not link to files, specifications, or directories that are not committed here.
- Do not expose internal planning labels, milestone names, task IDs, or temporary implementation codenames in source code, public documentation, generated contracts, API payloads, or UI copy.
- Prefer durable capability names such as `platform`, `events`, `bootstrap`, and `workspace` over delivery-phase labels.
- A `module.toml` field may remain empty when its authoritative source is not present in this repository. Do not invent references to satisfy a checker.

## Architecture

- Frontend ownership uses `styles/` for global tokens/base, `components/` for reusable primitives, `features/` for complete business UI/styles, and `pages/` for route composition and macro layout.
- The backend is one Rust server crate containing seven domain Modules: identity, models, projects, sessions, supervisor, runtime, and workspace-sync.
- Each Module exposes only `interface.rs`, includes a short `README.md` and `module.toml`, and does not import another Module's private implementation.
- Cross-module workflows belong in `application/`; shared persistence, IDs, clock, events, and secrets belong in `platform/`; external implementations belong in `adapters/`; HTTP/SSE belongs in `transport/`.
- Cross-module writes use short Unit of Work transactions through owner-provided commands; external file, process, Provider, and network work stays outside those transactions.
- Turn execution, wake-up, Tool result projection, and blocker reconciliation use the application Turn runner; Ask, Job, retry, and Cancel paths must not coordinate them independently.
- Do not introduce a central ports crate, generic repository abstraction, global event bus, or service locator.

## Product scope

- Janus has one deployment Owner; do not add User, Membership, RBAC, sharing, or multi-user UI abstractions.
- A new Session starts from Main's current Git-managed content, including tracked changes, deletions, and non-ignored untracked files; ignored files remain outside the Session.
- Attachments are arbitrary bounded Session resources: drafts become model-visible only after message binding, and later Turns may list, read, or save the same resource through Supervisor tools.
- Session workspaces are user read-only; do not expose a Session Terminal or ordinary Session file writes.
- Project Runtimes host Main Workspace Terminals; Session Runtimes host Supervisor Jobs and Services.

## Quality

- Rust must pass formatting, Clippy with warnings denied, and workspace tests.
- Frontend code must pass TypeScript, Biome, build, accessibility, responsive, and low-end performance checks; keep motion inexpensive and do not maintain a separate reduced-motion implementation.
- Public HTTP types originate in Rust and generate OpenAPI and TypeScript. The test CLI uses only public network interfaces.
- Applied SQLx migration files are byte-immutable, including comments; schema changes use a new migration.
- Functional acceptance uses the compiled server, real SQLite, public HTTP/SSE/CLI, and browser or real process behavior; mocks and direct Module tests are not acceptance evidence.
- Use one Bun lockfile. Do not add npm, pnpm, or Yarn lockfiles.
