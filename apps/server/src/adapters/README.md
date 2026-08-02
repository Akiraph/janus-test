# External Adapters

This directory contains deployment-provided implementations: `git.rs` connects the system Git executable, and `runtime/local.rs` connects local processes and terminals. Model Provider and Workspace filesystem interfaces are owned by their capability crates; the server injects an implementation only where deployment needs one.

Adapters may handle protocols, processes, paths, and resource limits, but they must not decide Session, Turn, or Project workflow outcomes or write capability tables and public events directly. Do not introduce another adapter trait until a second implementation is required. Contract tests should use temporary directories, repositories, and local processes where possible.
