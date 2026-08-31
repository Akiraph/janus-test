# Infrastructure

## Mission

`janus-infrastructure` gives capability modules and the server composition root durable technical primitives for a local, single-owner deployment: typed identifiers, UTC values, MongoDB access, transactional writes, public event replay, resumable operations, content-addressed bytes, encrypted secret values, and portable process helpers. It preserves storage and recovery semantics without knowing what a project, session, turn, or work kind means.

## Observable behavior

- A data root is exclusive. A second process fails during startup instead of sharing database files and recovery responsibility.
- Public events can be replayed from a cursor. A subscriber notification is only a wake-up hint and is sent after commit, so consumers must poll to repair missed notifications.
- Operation creation can replay an identical request, reject an idempotency key reused with a different digest, and expose leases and steps for recovery. It never performs the external side effect.
- Blob writes make the object available before registering its logical reference. A crash may leave an incoming temporary file or an object without a database reference; cleanup may remove only incoming files, while committed-object collection belongs elsewhere.
- Secret ciphertext is authenticated with caller-supplied associated data. A value encrypted for one owner, provider, or resource cannot be decrypted under another label.
- Development may create a local master-key file; production must supply `JANUS_MASTER_KEY`. The caller chooses that deployment mode, so this crate does not infer it from the environment.
- Process output decoding is bounded and explicit about invalid UTF-8. Bash discovery and PATH construction are host helpers used by Runtime and Execution; they do not execute a command or define a workflow.

## Invariants

- This crate depends only downward on generic technical libraries. It must not depend on server, deployment policy, or capability modules.
- The server composition root owns the schema catalog. Infrastructure runs idempotent collection creation at open but does not decide collection ownership or schema evolution.
- Event notifications follow commit. A rollback must never wake a consumer as if an event had become visible.
- An idempotency key with the same request digest reuses the stored operation; a different digest is an error, never an overwrite.
- A lease nonce is the ownership proof for completing or failing leased work. Expired leases may be reclaimed, but a stale holder may not mutate the new lease.
- Content identity is the SHA-256 digest of bytes. Existing objects are length-checked before reuse, and committed object files are not deleted by crash cleanup.
- Janus timestamps remain millisecond RFC 3339 values because storage ordering and public wire data cross this boundary.
- Typed IDs are UUID v7 wrappers shared across capability interfaces. The corresponding capability still owns the table and any domain validation.

## Boundaries and non-goals

This crate does not define work kinds, business event catalogs, capability tables, workflow orchestration, deployment policy, process execution policy, or external-side-effect handlers. Public events are an outward observation channel, not a command bus. Blob reference registration is here; mark-and-sweep collection and domain manifests are not.

Do not add a generic repository, service locator, or convenience coordinator here. If a rule needs business vocabulary or writes another module's state, it belongs above this crate.

## Design decisions

- Keep this as an independent crate so the compiler enforces the dependency ceiling; a `platform/` folder inside server would not.
- Keep collection creation inside `Database::open` so the connection layer stays reusable while the composition root retains ownership of the complete schema catalog.
- Reserve the single-writer lock at the start of short write transactions. Deferred transactions make contention appear later, after application code has already done work that cannot be committed safely.
- Treat event broadcast as a post-commit wake-up, not durable delivery. The database cursor is the durable source of truth and polling repairs lost notifications.
- Store blobs with a same-filesystem temporary file and atomic rename before committing references. This leaves a recoverable crash boundary without allowing a reference to point at a missing object.
- Keep operation journaling separate from side effects. The journal records intent, leases, and completed steps; the owning workflow decides how to execute or recover the side effect.
- Keep secret encryption here as a technical primitive, while the server composition root decides whether development key creation is permitted and capability code supplies stable associated-data labels.
