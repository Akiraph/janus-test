# Identity

## Mission

`janus-identity` owns the single deployment Owner, passkey ceremonies, login
sessions, CSRF state, initialization, and recovery. It exposes authentication
results and immutable `AuthContext` values; resource capabilities decide what
an authenticated actor may do.

## Observable behavior

- Initialization and recovery ceremonies are one-shot and expire from the
  database, so a process restart cannot replay an already-consumed ceremony.
- Login sessions use hashed bearer tokens, idle expiry, and a stored CSRF
  token. The bearer token and recovery codes are never returned from a query.
- Development authentication creates the Owner only when the database is
  empty. Production requires passkey ceremonies and does not silently create
  an Owner.

## Invariants

- The historical identity tables remain owned here: `tenants`, `owners`,
  `initialization_tokens`, `passkeys`, `ceremonies`, `login_sessions`,
  `recovery_batches`, `recovery_codes`, `recovery_states`.
- A final passkey cannot be removed. A ceremony or recovery state is consumed
  inside a short transaction before its result is accepted.
- The public interface returns strings for identity values, keeping typed ID
  wrappers and WebAuthn persistence details out of HTTP callers.

## Boundaries

This crate does not model members, roles, RBAC, resource authorization, model
credentials, or project state. The server supplies the relying-party settings
and deployment-mode flag; this crate does not read server `Config` or choose
deployment policy.

## Design decisions

WebAuthn state is serialized into the historical `ceremonies` table so the
challenge survives process restart and can be atomically consumed. Token
hashing uses the infrastructure purpose-label helper, keeping raw bearer and
recovery values outside persistent storage.
