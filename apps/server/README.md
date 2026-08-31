# Server

`apps/server` is Janus's deployment composition root and public control plane. It creates infrastructure and capability interfaces, injects the local Git and Runtime adapters, owns the MongoDB schema catalog, and connects HTTP, SSE, WebSocket, and CLI boundaries to public capability APIs.

`AppState` holds deployment resources and narrow capability query access for
transports and system tests. `application::Application` is the single
composition boundary for cross-capability workflows: transaction ordering,
execution scheduling, background workers, startup recovery, and resource
cleanup. `Application` owns no business tables; each capability's
`interface.rs` owns its state transitions.

This directory may contain dependency wiring, cross-capability transactions, durable Operation workers, external-side-effect adapters, and public protocol conversion. It must not grow new capability implementations, generic repositories, global event buses, service locators, or direct writes to another capability's tables.

During startup, `AppState::initialize` runs schema initialization and execution recovery. The process entry point then removes incoming Blob leftovers and marks stale Operations before `/health/ready` becomes successful. That ordering is a deployment contract, not background housekeeping.
