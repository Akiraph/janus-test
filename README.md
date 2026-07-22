# Janus

Janus is a local-first control plane for AI-assisted software work. The repository currently provides the executable platform foundation: a Rust control plane, persistent SQLite event stream, public health and system APIs, an independent test CLI, and a SolidJS workspace shell.

## Requirements

- Rust 1.97
- Bun 1.3
- Git 2.54 or newer

## Run locally

```text
cargo xtask setup
cargo xtask dev
```

The control plane listens on `http://127.0.0.1:4317` and the web app on `http://127.0.0.1:5173`. Runtime data defaults to `.janus-dev/`; set `JANUS_DATA_ROOT` to use another directory.

## Verify

```text
cargo xtask check
cargo xtask build
cargo run -p janus-test -- health
cargo run -p janus-test -- request GET /api/v1/system/info
cargo run -p janus-test -- events follow --count 1
```

Public API schema is generated to `generated/openapi.json`, then converted to frontend types in `apps/web/src/generated/api.ts`.
