# Workflows

`Application` is the server-internal interface for cross-capability workflows. It holds capability interfaces and the `ExecutionCoordinator`, then exposes use cases such as posting messages, delivering global async task results, and creating Project terminals. Its dependency accessors are crate-private and exist only for workflows, workers, and recovery.

Workflows own ordering, transaction boundaries, idempotent retry entry points, startup recovery, and resource cleanup, but never capability tables. External model, file, Git, network, and process operations run outside short database transactions; their results return through idempotent capability commands.

Every Turn wake-up enters the same `ExecutionCoordinator`. HTTP and workers may be different triggers, but they must not duplicate scheduling logic. Add a cross-capability rule here first, then decide whether the rule actually belongs in a capability crate.
