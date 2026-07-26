# models

Owns model providers, normalized model execution, usage ledger, retries, and
failover configuration. It does **not** own Session history or Turn state machines.

## M3 ownership

| Kind | Names |
| --- | --- |
| Tables | `model_providers`, `models`, `model_failover` (M1); `model_attempts`, `model_usage_ledger` (migration `0009_models_rounds.sql`) |
| Events | `model_config.changed`, `model.stream_delta`, `model.attempt_changed` |
| IDs | `ProviderId`, `ModelId` (config); `AttemptId` (execution) |

## Dependencies

No Module dependencies. Uses platform cipher / events only.

## Notes

- M3 adapters: OpenAI-compatible (chat/responses) + Anthropic real protocol streams.
- Provider server-side thread/session ids are transport-only and are not recovery keys.
- Token usage is written to `model_usage_ledger` per attempt; failover rows are not
  written in M3.
