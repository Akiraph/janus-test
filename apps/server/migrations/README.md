# Database Migrations

This directory is the single ordered SQLx migration sequence for the deployed
control plane. Applied SQL files are immutable: add a new migration instead of
rewriting history, and keep each file's owner declaration aligned with the
table-ownership checker.

`0001_initial.sql` is a single squash of the entire pre-release migration
history. Janus has not shipped to a public platform, so there is no deployed
database to upgrade: a fresh install runs the one file and is done. The squash
also dropped three tables that no Rust code ever read or wrote
(`runtime_ports`, `model_recovery_cooldowns`, `stream_diagnostics`) and removed
the rebuild/backfill/`ALTER` machinery that only existed to evolve an organic
prototype schema.

The server composition root injects this migration set into
`janus-infrastructure`. Schema ownership and business rules remain with the
corresponding capability crate. Each migration must declare, in its leading
comment block, every module that owns a table it touches
(`-- janus-module: <module>`); `tools/xtask validate_migration_ownership`
enforces that a migration only mutates tables owned by the modules it declares.
