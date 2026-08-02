# Public Transport

This directory is the public HTTP, SSE, and WebSocket boundary. It handles authentication context, request validation, DTOs, OpenAPI, cursors, and error mapping before calling an `Application` workflow or a capability's public interface.

Handlers do not operate the database, Unit of Work, EventStore, or internal projections, and they do not implement business retries, resource cleanup, or Turn scheduling. Rust generates the OpenAPI document and frontend types from it; internal structures must not leak into the API without an explicit design. Direct capability queries are valid protocol conversion, while cross-capability writes must go through `Application`.
