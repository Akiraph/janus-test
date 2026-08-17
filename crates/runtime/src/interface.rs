//! Public process-runtime lifecycle boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use utoipa::ToSchema;

use janus_infrastructure::{
    id::{
        AsyncTaskId, LogStreamId, ProjectId, RuntimeId, SessionId, TerminalId, ToolCallId, TurnId,
    },
    secrets::Secret,
};

pub use super::log_store::{LogRetention, LogStore};
pub use super::service::RuntimeInterface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeScope {
    Project { project_id: ProjectId },
}

impl RuntimeScope {
    pub const fn project(project_id: ProjectId) -> Self {
        Self::Project { project_id }
    }

    pub(crate) const fn kind(self) -> &'static str {
        "project"
    }

    pub(crate) fn id(self) -> String {
        let Self::Project { project_id } = self;
        project_id.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AsyncTaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Lost,
}

impl AsyncTaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Lost => "lost",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Canceled | Self::Lost
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Starting,
    Running,
    Closing,
    Exited,
    Failed,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Sync,
    AsyncTask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResourceLimits {
    pub timeout_ms: u64,
    pub memory_bytes: u64,
    pub cpu_millis: u64,
    pub pids: u32,
    pub temporary_disk_bytes: u64,
    pub open_files: u32,
}

impl ResourceLimits {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.timeout_ms == 0 {
            return Err(RuntimeError::InvalidSpec(
                "timeout_ms must be greater than zero".into(),
            ));
        }
        if self.memory_bytes == 0 || self.cpu_millis == 0 || self.pids == 0 || self.open_files == 0
        {
            return Err(RuntimeError::InvalidSpec(
                "memory_bytes, cpu_millis, pids, and open_files must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct RelativeWorkingDirectory(String);

impl RelativeWorkingDirectory {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeError> {
        let normalized = value.into();
        let normalized = normalized.trim();
        let normalized = match normalized.strip_prefix("/workspace") {
            Some("") => ".",
            Some(rest) if rest.starts_with('/') => rest.strip_prefix('/').unwrap_or("."),
            _ => normalized,
        };
        if normalized.contains('\0') {
            return Err(RuntimeError::InvalidSpec(
                "working_directory contains a null byte".into(),
            ));
        }
        Ok(Self(if normalized.is_empty() {
            ".".into()
        } else {
            normalized.into()
        }))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RelativeWorkingDirectory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCommand {
    input: String,
}

impl ValidatedCommand {
    pub fn shell(script: impl Into<String>) -> Result<Self, RuntimeError> {
        let input = script.into();
        if input.trim().is_empty() {
            return Err(RuntimeError::InvalidSpec(
                "command input cannot be empty".into(),
            ));
        }
        Ok(Self { input })
    }

    pub fn input(&self) -> &str {
        &self.input
    }
}

pub struct SecretEnvironmentVariable {
    name: String,
    value: Secret,
}

impl SecretEnvironmentVariable {
    pub fn new(name: impl Into<String>, value: Secret) -> Result<Self, RuntimeError> {
        let name = name.into();
        validate_environment_name(&name)?;
        Ok(Self { name, value })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &Secret {
        &self.value
    }
}

impl std::fmt::Debug for SecretEnvironmentVariable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretEnvironmentVariable")
            .field("name", &self.name)
            .field("value", &self.value)
            .finish()
    }
}

#[derive(Debug, Default)]
pub struct ExecutionEnvironment {
    ordinary: BTreeMap<String, String>,
    secrets: Vec<SecretEnvironmentVariable>,
}

impl ExecutionEnvironment {
    pub fn new(
        ordinary: BTreeMap<String, String>,
        secrets: Vec<SecretEnvironmentVariable>,
    ) -> Result<Self, RuntimeError> {
        let mut names = BTreeSet::new();
        for name in ordinary.keys() {
            validate_environment_name(name)?;
            names.insert(name.as_str());
        }
        for secret in &secrets {
            if !names.insert(secret.name()) {
                return Err(RuntimeError::InvalidSpec(format!(
                    "environment variable {} is defined more than once",
                    secret.name()
                )));
            }
        }
        Ok(Self { ordinary, secrets })
    }

    pub fn ordinary(&self) -> &BTreeMap<String, String> {
        &self.ordinary
    }

    pub fn secrets(&self) -> &[SecretEnvironmentVariable] {
        &self.secrets
    }
}

fn validate_environment_name(name: &str) -> Result<(), RuntimeError> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|value| value == '_' || value.is_ascii_alphabetic());
    if !valid_start || !chars.all(|value| value == '_' || value.is_ascii_alphanumeric()) {
        return Err(RuntimeError::InvalidSpec(format!(
            "{name:?} is not a portable environment variable name"
        )));
    }
    Ok(())
}

#[derive(Debug)]
pub struct RuntimeSpec {
    id: RuntimeId,
    scope: RuntimeScope,
    workspace_root: PathBuf,
    limits: ResourceLimits,
}

impl RuntimeSpec {
    pub fn new(
        id: RuntimeId,
        scope: RuntimeScope,
        workspace_root: PathBuf,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        if !workspace_root.is_absolute() {
            return Err(RuntimeError::InvalidSpec(
                "workspace_root must be an absolute trusted path".into(),
            ));
        }
        limits.validate()?;
        Ok(Self {
            id,
            scope,
            workspace_root,
            limits,
        })
    }

    pub fn id(&self) -> RuntimeId {
        self.id
    }

    pub fn scope(&self) -> RuntimeScope {
        self.scope
    }

    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }
}

#[derive(Debug)]
pub struct ExecutionSpec {
    runtime_id: RuntimeId,
    working_directory: RelativeWorkingDirectory,
    command: ValidatedCommand,
    environment: ExecutionEnvironment,
    limits: ResourceLimits,
}

impl ExecutionSpec {
    pub fn new(
        runtime_id: RuntimeId,
        working_directory: RelativeWorkingDirectory,
        command: ValidatedCommand,
        environment: ExecutionEnvironment,
        limits: ResourceLimits,
    ) -> Result<Self, RuntimeError> {
        limits.validate()?;
        Ok(Self {
            runtime_id,
            working_directory,
            command,
            environment,
            limits,
        })
    }

    pub fn runtime_id(&self) -> RuntimeId {
        self.runtime_id
    }

    pub fn working_directory(&self) -> &RelativeWorkingDirectory {
        &self.working_directory
    }

    pub fn command(&self) -> &ValidatedCommand {
        &self.command
    }

    pub fn environment(&self) -> &ExecutionEnvironment {
        &self.environment
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }
}

#[derive(Debug)]
pub struct AsyncTaskSpec {
    pub id: AsyncTaskId,
    pub session_id: SessionId,
    pub controlling_turn_id: TurnId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub execution: ExecutionSpec,
}

impl AsyncTaskSpec {
    pub fn new(
        id: AsyncTaskId,
        session_id: SessionId,
        controlling_turn_id: TurnId,
        initiated_by_tool_call_id: ToolCallId,
        execution: ExecutionSpec,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            id,
            session_id,
            controlling_turn_id,
            initiated_by_tool_call_id,
            execution,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSize {
    pub fn new(cols: u16, rows: u16) -> Result<Self, RuntimeError> {
        if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
            return Err(RuntimeError::InvalidSpec(
                "terminal dimensions must be between 1 and 1000".into(),
            ));
        }
        Ok(Self { cols, rows })
    }
}

#[derive(Debug)]
pub struct TerminalSpec {
    pub id: TerminalId,
    pub runtime_id: RuntimeId,
    pub project_id: ProjectId,
    pub working_directory: RelativeWorkingDirectory,
    pub environment: ExecutionEnvironment,
    pub size: TerminalSize,
}

/// Abstract signal a Terminal consumer may raise to the running shell.
///
/// The executor maps these to whatever the local execution backend supports.
/// It is intentionally not a raw POSIX signal value so the public contract
/// stays cross-platform: `ctrl_c` is an interrupt request and `terminate` is an
/// ungraceful kill request. Neither is required to be perfectly reliable on a
/// non-tty pipe backend; both are best-effort and the durable Terminal state
/// remains the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSignal {
    CtrlC,
    Terminate,
}

/// Issued access material for a Terminal WebSocket upgrade.
///
/// The original token is returned once to the requesting actor and never
/// persisted. Only the [`TerminalTicket::token_hash`] and metadata needed to
/// validate a later upgrade are stored. Consumption is atomic and single-use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalTicket {
    pub terminal_id: TerminalId,
    /// The raw bearer token to hand to the WebSocket client. Never put this
    /// into SQLite or an event payload.
    #[serde(rename = "token")]
    pub token: String,
    pub expires_at: String,
}

/// Record persisted for an outstanding Terminal access ticket.
#[derive(Debug, Clone)]
pub struct TerminalTicketRecord {
    pub terminal_id: TerminalId,
    pub token_hash: String,
    pub actor_id: String,
    pub origin: String,
    pub expires_at: String,
}

/// Request to issue a new Terminal access ticket.
#[derive(Debug, Clone)]
pub struct TerminalTicketRequest {
    pub terminal_id: TerminalId,
    pub actor_id: String,
    pub origin: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, ToSchema)]
#[schema(value_type = String, example = "0")]
pub struct LogCursor(u64);

impl LogCursor {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for LogCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for LogCursor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for LogCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value
            .parse()
            .map(Self)
            .map_err(|_| de::Error::custom("log cursor must be an unsigned decimal string"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExitSummary {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ResourceUsage {
    pub cpu_millis: u64,
    pub peak_memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExecutionResult {
    pub mode: ExecutionMode,
    pub exit: ExitSummary,
    pub stdout: String,
    pub stderr: String,
    pub log_stream_id: LogStreamId,
    pub output_cursor: LogCursor,
    pub duration_ms: u64,
    pub usage: ResourceUsage,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorRuntimeHandle {
    pub executor_identity: String,
    pub executor_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorProcessHandle {
    pub process_identity: String,
    pub executor_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCompletion {
    pub exit: ExitSummary,
    pub duration_ms: u64,
    pub usage: ResourceUsage,
}

pub trait RuntimeExecutor: Send + Sync {
    fn ensure_runtime<'a>(
        &'a self,
        spec: &'a RuntimeSpec,
    ) -> BoxFuture<'a, Result<ExecutorRuntimeHandle, RuntimeError>>;

    fn stop_runtime<'a>(
        &'a self,
        runtime_id: RuntimeId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    fn execute_sync<'a>(
        &'a self,
        spec: ExecutionSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutionResult, RuntimeError>>;

    fn start_async_task<'a>(
        &'a self,
        spec: AsyncTaskSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>>;

    fn wait_async_task<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    fn write_async_task_stdin<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
        input: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    fn cancel_async_task<'a>(
        &'a self,
        id: AsyncTaskId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    /// Start a long-lived interactive shell for a Terminal and begin streaming
    /// its stdout/stderr into the Terminal scrollback log stream.
    fn start_terminal<'a>(
        &'a self,
        spec: TerminalSpec,
        scrollback_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>>;

    /// Write raw input bytes to the Terminal's shell stdin. Emptiness is the
    /// caller's responsibility; an empty write is a no-op.
    fn write_terminal_input<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        input: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    /// Request a Terminal resize. Backend may only remember the new size; the
    /// durable Terminal projection records it regardless.
    fn resize_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        size: TerminalSize,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    /// Raise an abstract signal (interrupt / terminate) against the shell.
    fn signal_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
        signal: TerminalSignal,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    /// Stop the Terminal shell, finalizing its scrollback log stream. Returns
    /// the shell's process completion summary. A Terminal that already exited
    /// returns its recorded completion without touching a live process.
    fn close_terminal<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    /// Await a Terminal shell's natural exit without signaling it. Returns the
    /// recorded completion when the shell has already exited, or waits for it.
    /// The owner drops the live handle after this resolves.
    fn await_terminal_exit<'a>(
        &'a self,
        id: TerminalId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeProjection {
    pub id: RuntimeId,
    pub scope: RuntimeScope,
    pub status: RuntimeStatus,
    pub limits: ResourceLimits,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub stopped_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AsyncTaskProjection {
    pub id: AsyncTaskId,
    pub runtime_id: RuntimeId,
    pub session_id: SessionId,
    pub controlling_turn_id: TurnId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub status: AsyncTaskStatus,
    pub command_summary: String,
    pub log_stream_id: LogStreamId,
    pub exit: Option<ExitSummary>,
    pub usage: ResourceUsage,
    pub version: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalProjection {
    pub id: TerminalId,
    pub runtime_id: RuntimeId,
    pub project_id: ProjectId,
    pub status: TerminalStatus,
    pub size: TerminalSize,
    pub scrollback_stream_id: LogStreamId,
    pub first_cursor: LogCursor,
    pub next_cursor: LogCursor,
    pub writable: bool,
    pub exit: Option<ExitSummary>,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogStreamProjection {
    pub id: LogStreamId,
    pub first_cursor: LogCursor,
    pub next_cursor: LogCursor,
    pub retained_bytes: u64,
    pub total_bytes: u64,
    pub truncated: bool,
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogChannel {
    Stdout,
    Stderr,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOwnerKind {
    Sync,
    AsyncTask,
    Terminal,
}

impl LogOwnerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::AsyncTask => "async_task",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogChunk {
    pub start_cursor: LogCursor,
    pub end_cursor: LogCursor,
    pub channel: LogChannel,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogRange {
    pub stream: LogStreamProjection,
    pub after: LogCursor,
    pub chunks: Vec<LogChunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeErrorCode {
    ValidationFailed,
    ResourceBusy,
    RuntimeUnavailable,
    AsyncTaskLost,
    TerminalTicketInvalid,
    TerminalScrollbackExpired,
    TerminalNotWritable,
}

impl RuntimeErrorCode {
    pub const ALL: [Self; 7] = [
        Self::ValidationFailed,
        Self::ResourceBusy,
        Self::RuntimeUnavailable,
        Self::AsyncTaskLost,
        Self::TerminalTicketInvalid,
        Self::TerminalScrollbackExpired,
        Self::TerminalNotWritable,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidationFailed => "VALIDATION_FAILED",
            Self::ResourceBusy => "RESOURCE_BUSY",
            Self::RuntimeUnavailable => "RUNTIME_UNAVAILABLE",
            Self::AsyncTaskLost => "ASYNC_TASK_LOST",
            Self::TerminalTicketInvalid => "TERMINAL_TICKET_INVALID",
            Self::TerminalScrollbackExpired => "TERMINAL_SCROLLBACK_EXPIRED",
            Self::TerminalNotWritable => "TERMINAL_NOT_WRITABLE",
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("the runtime specification is invalid: {0}")]
    InvalidSpec(String),
    #[error("the runtime resource is busy")]
    ResourceBusy,
    #[error("the runtime is unavailable")]
    RuntimeUnavailable,
    #[error("the runtime is unavailable: {0}")]
    RuntimeUnavailableDetail(String),
    #[error("async_task {0} can no longer be controlled")]
    AsyncTaskLost(AsyncTaskId),
    #[error("the terminal access ticket is invalid")]
    TerminalTicketInvalid,
    #[error("terminal scrollback before cursor {first_cursor} is no longer retained")]
    TerminalScrollbackExpired { first_cursor: LogCursor },
    #[error("terminal {0} is not writable")]
    TerminalNotWritable(TerminalId),
}

impl RuntimeError {
    pub const fn code(&self) -> RuntimeErrorCode {
        match self {
            Self::InvalidSpec(_) => RuntimeErrorCode::ValidationFailed,
            Self::ResourceBusy => RuntimeErrorCode::ResourceBusy,
            Self::RuntimeUnavailable | Self::RuntimeUnavailableDetail(_) => {
                RuntimeErrorCode::RuntimeUnavailable
            }
            Self::AsyncTaskLost(_) => RuntimeErrorCode::AsyncTaskLost,
            Self::TerminalTicketInvalid => RuntimeErrorCode::TerminalTicketInvalid,
            Self::TerminalScrollbackExpired { .. } => RuntimeErrorCode::TerminalScrollbackExpired,
            Self::TerminalNotWritable(_) => RuntimeErrorCode::TerminalNotWritable,
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::ResourceBusy | Self::RuntimeUnavailable | Self::RuntimeUnavailableDetail(_)
        )
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::RuntimeUnavailableDetail(detail.into())
    }
}

#[cfg(test)]
mod tests {
    use super::RelativeWorkingDirectory;

    #[test]
    fn accepts_the_logical_workspace_absolute_prefix() {
        assert_eq!(
            RelativeWorkingDirectory::new("/workspace")
                .expect("workspace root should be valid")
                .as_str(),
            "."
        );
        assert_eq!(
            RelativeWorkingDirectory::new("/workspace/src")
                .expect("workspace child should be valid")
                .as_str(),
            "src"
        );
        assert_eq!(
            RelativeWorkingDirectory::new("/workspace/../outside")
                .expect("arbitrary working directories are allowed")
                .as_str(),
            "../outside"
        );
        assert_eq!(
            RelativeWorkingDirectory::new("/etc")
                .expect("absolute working directories are allowed")
                .as_str(),
            "/etc"
        );
    }
}
