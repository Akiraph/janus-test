# Baseline Governance

## Product baseline

User-visible workflow requirements and acceptance criteria are the product
authority. A new feature must preserve existing project, session, and data
visibility unless the requirement explicitly changes it.

## Runtime baseline

Operations own durable work and recovery. Projects own repository identity and
workspace reuse. Sessions own AI conversation history. HTTP handlers translate
external contracts and do not duplicate orchestration.

## Compatibility boundary

Existing single-PR webhook payloads remain accepted. Applied migrations are
immutable; new persistent fields require forward migrations and no data reset.
