# Database Migrations

This directory is the single ordered SQLx migration sequence for the deployed control plane. Applied SQL files are immutable: add a new migration instead of rewriting history, and keep each file's owner declaration aligned with the table-ownership checker.

The server composition root injects this migration set into `janus-infrastructure`. Schema ownership and business rules remain with the corresponding capability crate, while historical table names, event names, and migration-owner normalization remain compatible during extraction.
