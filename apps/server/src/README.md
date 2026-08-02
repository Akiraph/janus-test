# Server Source

Source is organized by dependency direction: `application` exposes cross-capability workflows, `transport` converts HTTP, SSE, and WebSocket protocols, `adapters` supplies deployment-specific Git and process implementations, and `config` parses deployment input. Capability state and public interfaces live in the capability crates under `crates/`.

`Application` is the internal boundary for cross-capability transactions, scheduling, recovery, and resource cleanup. It calls capability `interface.rs` APIs but owns no business tables. `AppState` wires composition-root resources and retains capability query getters for transports and system tests; new workflows must not be added to `AppState`.

HTTP handlers do not write capability tables directly. External side effects enter through adapters, and public types pass through the Rust OpenAPI generation chain. Do not add generic repositories, global event buses, or forwarding service locators here.
