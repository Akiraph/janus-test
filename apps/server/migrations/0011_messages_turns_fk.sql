-- janus-module: sessions
-- recovery: forward-only; rebuilds the `messages` table so its `turn_id`
-- foreign key once again references `turns(id)`.
--
-- Background: migration 0010 renamed `turns` to `turns_legacy` (under
-- `PRAGMA legacy_alter_table = ON`) before rebuilding it. SQLite's legacy
-- rename rewrote every *referencing* foreign key to follow the renamed
-- table, so `messages.turn_id` ended up pointing at the transient
-- `turns_legacy` that 0010 dropped. Any later statement that exercises the
-- `messages.turn_id` foreign key (e.g. `DELETE FROM sessions` cascading
-- into turns/messages) fails with "no such table: main.turns_legacy".
--
-- This migration rebuilds `messages` with the correct `turns` reference and
-- preserves all rows and indexes. `turns` here is the post-0010 rebuilt table.

CREATE TEMP TABLE messages_legacy AS SELECT * FROM messages;
DROP TABLE messages;

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
    actor_json TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'assistant', 'system', 'tool_result_ref')),
    body_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded')),
    timeline_sequence INTEGER,
    version TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX messages_session_idx ON messages (session_id, created_at);
CREATE INDEX messages_turn_idx ON messages (turn_id);

INSERT INTO messages (
    id, session_id, turn_id, actor_json, kind, body_json, status,
    timeline_sequence, version, created_at
)
SELECT
    id, session_id, turn_id, actor_json, kind, body_json, status,
    timeline_sequence, version, created_at
FROM messages_legacy;
DROP TABLE messages_legacy;

-- Same legacy-name trap affects `turns.handoff_*_turn_id` and `predecessor_turn_id`
-- self-references added by 0010: they were created on the *rebuilt* turns table,
-- but the rebuild happened while `turns_legacy` still referenced `sessions`, so
-- the self-references here are clean. Verify by rebuilding the dependent
-- `tool_calls` linkage to `rounds`: 0010 already rebuilt `rounds` and `tool_calls`
-- against the new `rounds` table, so those FKs are correct. No further action
-- needed for them.
