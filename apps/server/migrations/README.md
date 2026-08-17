# Database Migrations

`0001_initial.sql` is the one and only migration. Janus has not shipped to a
public platform and no deployed database exists, so the schema is a single
squash: a fresh install runs the one file and is done.

This project does **not** preserve compatibility shims or migration history for
features that are no longer supported. When a feature is removed (or its schema
shape changes), the migration is folded back into `0001_initial.sql` directly
and any obsolete migration files are deleted. There are no "next version"
migration files.

Each table still declares its owning module in the leading comment block
(`-- janus-module: <module>`); `tools/xtask validate_migration_ownership`
enforces that the migration only mutates tables owned by the modules it
declares. The server composition root injects this migration set into
`janus-infrastructure`; schema ownership and business rules remain with the
corresponding capability crate.