# janus-test

`janus-test` is Janus's black-box verification entry point. It connects to a running service and uses public HTTP, SSE, and WebSocket interfaces to check health, resource changes, errors, and recovery. It does not access the database directly or call Rust internals.

The default flow uses a deterministic test Provider for CI and real system workflows. Real Provider verification belongs in an explicit smoke flow covering authentication, streaming, usage, retries, failover, latency, and cost; external credentials must never enter ordinary tests implicitly.
