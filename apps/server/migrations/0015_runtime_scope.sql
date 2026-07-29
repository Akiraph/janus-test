-- janus-module: runtime
-- recovery: forward-only; existing Runtime rows retain their Session identity.

ALTER TABLE runtimes RENAME COLUMN session_id TO scope_id;
ALTER TABLE runtimes ADD COLUMN scope_kind TEXT NOT NULL DEFAULT 'session'
    CHECK (scope_kind IN ('project', 'session'));

DROP INDEX runtimes_one_current_per_session;
CREATE UNIQUE INDEX runtimes_one_current_per_scope ON runtimes (scope_kind, scope_id)
WHERE status IN ('starting', 'ready', 'stopping');
