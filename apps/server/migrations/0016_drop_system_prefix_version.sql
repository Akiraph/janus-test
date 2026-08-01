-- janus-module: runtime
-- recovery: forward-only; drops the redundant system_prefix_version column.
-- The system prompt is a code constant whose history is tracked by git, so the
-- persisted per-row version tag served no purpose beyond what git already records.

ALTER TABLE context_versions DROP COLUMN system_prefix_version;
