-- janus-module: supervisor
-- recovery: additive; preserves all existing Tool Calls.

ALTER TABLE tool_calls ADD COLUMN provider_call_id TEXT;

CREATE UNIQUE INDEX tool_calls_provider_call_idx
ON tool_calls (round_id, provider_call_id)
WHERE provider_call_id IS NOT NULL;
