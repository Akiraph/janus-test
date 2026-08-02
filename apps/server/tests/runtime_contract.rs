use std::{collections::BTreeMap, path::PathBuf};

use janus_infrastructure::id::{
    CliSessionId, RuntimeId, ServiceId, SessionId, TerminalId, ToolCallId,
};
use janus_infrastructure::secrets::Secret;
use janus_runtime::interface::*;
use serde::Serialize;
use serde_json::json;

fn serialized<T: Serialize>(values: &[T]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            serde_json::to_value(value)
                .expect("contract value serializes")
                .as_str()
                .expect("contract enum serializes as a string")
                .to_owned()
        })
        .collect()
}

fn limits() -> ResourceLimits {
    ResourceLimits {
        timeout_ms: 30_000,
        memory_bytes: 512 * 1024 * 1024,
        cpu_millis: 1_000,
        pids: 64,
        temporary_disk_bytes: 1024 * 1024 * 1024,
        open_files: 256,
    }
}

#[test]
fn runtime_statuses_have_exhaustive_stable_serialization() {
    assert_eq!(
        serialized(&[
            RuntimeStatus::Starting,
            RuntimeStatus::Ready,
            RuntimeStatus::Stopping,
            RuntimeStatus::Stopped,
            RuntimeStatus::Failed,
            RuntimeStatus::Lost,
        ]),
        ["starting", "ready", "stopping", "stopped", "failed", "lost"]
    );
    assert_eq!(
        serialized(&[
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::Failed,
            JobStatus::Canceled,
            JobStatus::Lost,
        ]),
        [
            "queued",
            "running",
            "succeeded",
            "failed",
            "canceled",
            "lost"
        ]
    );
    assert_eq!(
        serialized(&[
            ServiceStatus::Starting,
            ServiceStatus::Running,
            ServiceStatus::Unhealthy,
            ServiceStatus::Stopping,
            ServiceStatus::Stopped,
            ServiceStatus::StoppedAfterRestart,
            ServiceStatus::Failed,
        ]),
        [
            "starting",
            "running",
            "unhealthy",
            "stopping",
            "stopped",
            "stopped_after_restart",
            "failed",
        ]
    );
    assert_eq!(
        serialized(&[
            TerminalStatus::Starting,
            TerminalStatus::Running,
            TerminalStatus::Closing,
            TerminalStatus::Exited,
            TerminalStatus::Failed,
            TerminalStatus::Lost,
        ]),
        ["starting", "running", "closing", "exited", "failed", "lost"]
    );
}

#[test]
fn runtime_capability_is_single_source_and_enforces_reason_invariant() {
    let capabilities = local_deployment_capabilities();
    assert_eq!(capabilities.len(), RuntimeCapabilityId::ALL.len());
    assert_eq!(
        capabilities
            .iter()
            .map(|value| value.id)
            .collect::<Vec<_>>(),
        RuntimeCapabilityId::ALL
    );
    // Deployment probing reflects the real host, so some dependency-backed
    // capabilities (claude_code, codex) may legitimately be `ready`. The
    // contract invariant we actually enforce is the reason_code rule owned by
    // `RuntimeCapability::new`: every non-ready capability must carry a
    // reason, and a ready capability must not. That invariant is verified
    // below against a deterministic probe and is asserted here only as a
    // structural property.
    assert!(capabilities.iter().all(|value| {
        value.scope == CapabilityScope::Deployment
            && value.checked_at.is_some()
            && (value.state == CapabilityState::Ready) == value.reason_code.is_none()
    }));

    let process = capabilities
        .iter()
        .find(|value| value.id == RuntimeCapabilityId::ProcessExecution)
        .expect("process capability is present");
    assert_eq!(process.scope, CapabilityScope::Deployment);
    assert_eq!(process.state, CapabilityState::Degraded);
    assert_eq!(process.reason_code, Some(CapabilityReason::LocalExecutor));
    let serialized =
        serde_json::to_value(process).expect("capability serializes to a stable object");
    assert_eq!(serialized["id"], "process_execution");
    assert_eq!(serialized["scope"], "deployment");
    assert_eq!(serialized["state"], "degraded");
    assert_eq!(serialized["reason_code"], "LOCAL_EXECUTOR");
    assert!(serialized.get("checked_at").is_some());
    assert!(
        RuntimeCapability::new(
            RuntimeCapabilityId::ProcessExecution,
            CapabilityScope::Deployment,
            CapabilityState::Ready,
            Some(CapabilityReason::ProbeFailed),
        )
        .is_err()
    );
    assert!(
        RuntimeCapability::new(
            RuntimeCapabilityId::ProcessExecution,
            CapabilityScope::Deployment,
            CapabilityState::Unsupported,
            None,
        )
        .is_err()
    );
}

#[test]
fn execution_specs_reject_ambiguous_or_unbounded_input() -> anyhow::Result<()> {
    for invalid in [
        "/absolute",
        "../escape",
        "nested/../escape",
        "C:/drive",
        "a\\b",
    ] {
        assert!(
            RelativeWorkingDirectory::new(invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert_eq!(RelativeWorkingDirectory::new("")?.as_str(), ".");
    assert_eq!(
        RelativeWorkingDirectory::new("src/bin")?.as_str(),
        "src/bin"
    );
    assert!(serde_json::from_value::<RelativeWorkingDirectory>(json!("../escape")).is_err());

    let mut invalid_limits = limits();
    invalid_limits.timeout_ms = 0;
    assert!(invalid_limits.validate().is_err());
    assert!(ValidatedCommand::shell("  ").is_err());

    let cli_session_id = CliSessionId::new();
    let delegated = ValidatedCommand::delegated_cli(
        DelegatedCliKind::Codex,
        "inspect the failing test",
        Some(cli_session_id),
    )?;
    assert!(matches!(
        delegated.kind(),
        CommandKind::DelegatedCli {
            cli: DelegatedCliKind::Codex,
            session_id: Some(id),
        } if *id == cli_session_id
    ));

    let mut ordinary = BTreeMap::new();
    ordinary.insert("PUBLIC_VALUE".into(), "visible".into());
    let secret = SecretEnvironmentVariable::new("TOKEN", Secret::new("never-print-this".into()))?;
    let environment = ExecutionEnvironment::new(ordinary, vec![secret])?;
    assert!(!format!("{environment:?}").contains("never-print-this"));

    let execution = ExecutionSpec::new(
        RuntimeId::new(),
        RelativeWorkingDirectory::new("src")?,
        delegated,
        environment,
        limits(),
        NetworkPolicy::ProjectRules,
    )?;
    assert!(
        ServiceSpec::new(
            ServiceId::new(),
            SessionId::new(),
            ToolCallId::new(),
            ServiceImpact::ReadOnly,
            execution,
        )
        .is_err()
    );
    assert!(TerminalSize::new(0, 24).is_err());
    assert_eq!(TerminalSize::new(120, 36)?.cols, 120);
    Ok(())
}

#[test]
fn runtime_spec_requires_a_trusted_absolute_workspace() -> anyhow::Result<()> {
    assert!(
        RuntimeSpec::new(
            RuntimeId::new(),
            janus_runtime::interface::RuntimeScope::session(SessionId::new()),
            janus_runtime::interface::ExecutorKind::Local,
            PathBuf::from("relative"),
            limits(),
            NetworkPolicy::DenyAll,
        )
        .is_err()
    );

    let directory = tempfile::tempdir()?;
    let spec = RuntimeSpec::new(
        RuntimeId::new(),
        janus_runtime::interface::RuntimeScope::session(SessionId::new()),
        janus_runtime::interface::ExecutorKind::Local,
        directory.path().to_path_buf(),
        limits(),
        NetworkPolicy::DenyAll,
    )?;
    assert!(spec.workspace_root().is_absolute());
    Ok(())
}

#[test]
fn log_cursor_is_a_decimal_string_contract() -> anyhow::Result<()> {
    let cursor = LogCursor::new(5901);
    assert_eq!(serde_json::to_value(cursor)?, json!("5901"));
    assert_eq!(
        serde_json::from_value::<LogCursor>(json!("5901"))?.value(),
        5901
    );
    assert!(serde_json::from_value::<LogCursor>(json!(5901)).is_err());
    assert!(serde_json::from_value::<LogCursor>(json!("-1")).is_err());
    Ok(())
}

#[test]
fn runtime_errors_have_exhaustive_stable_codes() {
    let errors = [
        RuntimeError::InvalidSpec("bad".into()),
        RuntimeError::ResourceBusy,
        RuntimeError::CommandForbidden,
        RuntimeError::NetworkPolicyDenied,
        RuntimeError::RuntimeUnavailable,
        RuntimeError::JobLost(janus_infrastructure::id::JobId::new()),
        RuntimeError::ServiceLost(ServiceId::new()),
        RuntimeError::TerminalTicketInvalid,
        RuntimeError::TerminalScrollbackExpired {
            first_cursor: LogCursor::new(12),
        },
        RuntimeError::TerminalNotWritable(TerminalId::new()),
    ];
    assert_eq!(
        errors.iter().map(RuntimeError::code).collect::<Vec<_>>(),
        RuntimeErrorCode::ALL
    );
    assert!(RuntimeError::ResourceBusy.retryable());
    assert!(RuntimeError::RuntimeUnavailable.retryable());
    assert!(!RuntimeError::CommandForbidden.retryable());
}
