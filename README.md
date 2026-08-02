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

## Crates

Rust code is being split along the dependency graph. `janus-infrastructure` contains independently testable generic IDs, clocks, SQLite, transactions, public events, operation journals, and Blob storage. The server composition root owns the ordered migration set and deployment policy; infrastructure contains no concrete work kinds or server workflows.

`janus-workspace` is the first capability crate. It owns workspace copies, content revisions, snapshots, manifests, diffs, and controlled file mutations while keeping Project/Session identifiers opaque to the crate.

基础设施 crate 可以单独快速验证：

```text
cargo test -p janus-infrastructure
```

## 开发入口

架构目标和依赖方向见 [ARCHITECTURE.md](ARCHITECTURE.md)，术语见 [CONTEXT.md](CONTEXT.md)。修改某个目录前先读该目录的 README；它描述需求边界和禁区，不维护文件目录索引。
