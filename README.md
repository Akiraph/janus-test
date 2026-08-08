# Janus

Janus is a local-first control plane for AI-assisted software work. The
repository provides a Rust control plane, a persistent SQLite event stream,
public health and system APIs, an independent test CLI, and a SolidJS shell.

## Requirements

- Rust 1.97
- Bun 1.3
- Git 2.54 or newer

## Run locally

```text
cargo xtask setup
cargo xtask dev
```

The control plane listens on `http://127.0.0.1:4317` and the web app on
`http://127.0.0.1:5173`. Runtime data defaults to `.janus-dev/`; set
`JANUS_DATA_ROOT` to use another directory.

To run a second web/control-plane pair alongside a Tauri client, set
`JANUS_BIND`, `JANUS_PUBLIC_ORIGIN`, `JANUS_WEB_PORT`, and optionally
`JANUS_API_TARGET` before `cargo xtask dev`. For example, use
`127.0.0.1:4318` for the control plane and `5174` for the web app.

## Verify

```text
cargo xtask check
cargo xtask build
cargo run -p janus-test -- health
cargo run -p janus-test -- request GET /api/v1/system/info
cargo run -p janus-test -- events follow --count 1
```

Public API schema is generated to `generated/openapi.json`, then converted to
frontend types in `apps/web/src/generated/api.ts`.

## Crates

Rust code is split along the dependency graph. `janus-infrastructure` contains
generic IDs, clocks, SQLite, transactions, public events, operation journals,
Blob storage, encrypted secrets, and portable process helpers. The server
composition root owns the ordered migration set and deployment policy;
infrastructure contains no concrete work kinds or server workflows.

`janus-workspace` owns workspace copies, content revisions, snapshots,
manifests, diffs, and controlled file mutations. `janus-source-control` owns
the Git port and normalized protocol values. The remaining capability crates
own identity, models, runtime, projects, sessions, and execution projections
as described in each crate README.

Cross-capability transactions, scheduling, recovery, and composition remain in
`apps/server/application/`. Public HTTP types are generated from the compiled
server OpenAPI document; handlers do not expose capability implementation
types directly.
