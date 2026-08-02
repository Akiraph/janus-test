# Models

## Mission

`janus-models` owns provider configuration, embedded model definitions,
provider-level failover, model attempts, usage accounting, and the external
stream protocol adapters for Anthropic and OpenAI-compatible endpoints.

## Observable behavior

- Provider API keys are encrypted with owner/provider associated data and only
  their fingerprint and masked preview are exposed in views.
- Each stream creates a durable attempt before contacting the provider and
  finalizes it after completion or failure. A stream that ends without a
  completion event is recorded as failed.
- Provider URLs are validated before persistence and external requests disable
  redirects, so credentials cannot be forwarded to an untrusted host.

## Invariants

- This crate writes only model tables: `model_providers`, `models`,
  `model_failover`, `model_recovery_cooldowns`, `model_attempts`, and
  `model_usage_ledger`.
- Model configuration changes publish public events after the owning
  transaction commits. Session history and Turn state remain outside this
  crate.
- Provider errors are normalized before they reach callers; raw response
  bodies and credential values are not persisted or emitted as stream detail.

## Boundaries

Execution decides whether a model attempt should be retried, failed, or
accepted. This crate does not own Sessions, Rounds, tools, prompts, or HTTP
routes. The server supplies the database, event store, and master-key cipher.

## Design decisions

Streaming protocol parsing is kept beside the provider boundary because the
wire formats determine how deltas, tool calls, usage, and terminal errors are
assembled. The public interface exposes normalized values so Execution does
not depend on provider-specific JSON shapes.
