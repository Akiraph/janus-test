use std::{collections::BTreeMap, path::PathBuf};

use janus_infrastructure::id::{ProjectId, RuntimeId, TerminalId};
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
            AsyncTaskStatus::Queued,
            AsyncTaskStatus::Running,
            AsyncTaskStatus::Succeeded,
            AsyncTaskStatus::Failed,
            AsyncTaskStatus::Canceled,
            AsyncTaskStatus::Lost,
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
fn execution_specs_allow_unrestricted_working_directories() -> anyhow::Result<()> {
    for accepted in [
        "/absolute",
        "../escape",
        "nested/../escape",
        "C:/drive",
        "a\\b",
    ] {
        assert!(
            RelativeWorkingDirectory::new(accepted).is_ok(),
            "rejected {accepted}"
        );
    }
    assert_eq!(RelativeWorkingDirectory::new("")?.as_str(), ".");
    assert_eq!(
        RelativeWorkingDirectory::new("src/bin")?.as_str(),
        "src/bin"
    );
    assert_eq!(
        serde_json::from_value::<RelativeWorkingDirectory>(json!("../escape"))?.as_str(),
        "../escape"
    );

    let mut invalid_limits = limits();
    invalid_limits.timeout_ms = 0;
    assert!(invalid_limits.validate().is_err());
    assert!(ValidatedCommand::shell("  ").is_err());

    let command = ValidatedCommand::shell("inspect the failing test")?;

    let mut ordinary = BTreeMap::new();
    ordinary.insert("PUBLIC_VALUE".into(), "visible".into());
    let secret = SecretEnvironmentVariable::new("TOKEN", Secret::new("never-print-this".into()))?;
    let environment = ExecutionEnvironment::new(ordinary, vec![secret])?;
    assert!(!format!("{environment:?}").contains("never-print-this"));

    let _execution = ExecutionSpec::new(
        RuntimeId::new(),
        RelativeWorkingDirectory::new("src")?,
        command,
        environment,
        limits(),
    )?;
    assert!(TerminalSize::new(0, 24).is_err());
    assert_eq!(TerminalSize::new(120, 36)?.cols, 120);
    Ok(())
}

#[test]
fn runtime_spec_requires_a_trusted_absolute_workspace() -> anyhow::Result<()> {
    assert!(
        RuntimeSpec::new(
            RuntimeId::new(),
            janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
            PathBuf::from("relative"),
            limits(),
        )
        .is_err()
    );

    let directory = tempfile::tempdir()?;
    let spec = RuntimeSpec::new(
        RuntimeId::new(),
        janus_runtime::interface::RuntimeScope::project(ProjectId::new()),
        directory.path().to_path_buf(),
        limits(),
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
        RuntimeError::RuntimeUnavailable,
        RuntimeError::AsyncTaskLost(janus_infrastructure::id::AsyncTaskId::new()),
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
}
