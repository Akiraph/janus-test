//! Public process-runtime lifecycle boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::Utc;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use utoipa::ToSchema;

use crate::platform::{
    clock::format_utc,
    id::{
        CliSessionId, JobId, LogStreamId, ProjectId, RuntimeId, RuntimePortId, ServiceId,
        SessionId, TerminalId, ToolCallId, TurnId,
    },
    secret::Secret,
};

pub use super::service::RuntimeInterface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    Local,
    Container,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum RuntimeCapabilityId {
    #[serde(rename = "process_execution")]
    ProcessExecution,
    #[serde(rename = "container_isolation")]
    ContainerIsolation,
    #[serde(rename = "bash_egress")]
    BashEgress,
    #[serde(rename = "browser")]
    Browser,
    #[serde(rename = "live_preview")]
    LivePreview,
    #[serde(rename = "delegated_cli.claude_code")]
    DelegatedCliClaudeCode,
    #[serde(rename = "delegated_cli.codex")]
    DelegatedCliCodex,
}

impl RuntimeCapabilityId {
    pub const ALL: [Self; 7] = [
        Self::ProcessExecution,
        Self::ContainerIsolation,
        Self::BashEgress,
        Self::Browser,
        Self::LivePreview,
        Self::DelegatedCliClaudeCode,
        Self::DelegatedCliCodex,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityScope {
    Deployment,
    Project,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Ready,
    Degraded,
    Unconfigured,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityReason {
    LocalExecutor,
    ConfigMissing,
    DependencyMissing,
    PlatformUnsupported,
    PolicyDisabled,
    ProbeFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeCapability {
    pub id: RuntimeCapabilityId,
    pub scope: CapabilityScope,
    pub state: CapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<CapabilityReason>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effective_limits: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<String>,
}

impl RuntimeCapability {
    pub fn new(
        id: RuntimeCapabilityId,
        scope: CapabilityScope,
        state: CapabilityState,
        reason_code: Option<CapabilityReason>,
    ) -> Result<Self, RuntimeError> {
        if matches!(state, CapabilityState::Ready) == reason_code.is_some() {
            return Err(RuntimeError::InvalidSpec(
                "ready capabilities omit reason_code and all other states require it".into(),
            ));
        }
        Ok(Self {
            id,
            scope,
            state,
            reason_code,
            effective_limits: BTreeMap::new(),
            checked_at: None,
        })
    }

    pub fn with_effective_limits(mut self, effective_limits: BTreeMap<String, u64>) -> Self {
        self.effective_limits = effective_limits;
        self
    }

    pub fn with_checked_at(mut self, checked_at: impl Into<String>) -> Self {
        self.checked_at = Some(checked_at.into());
        self
    }
}

/// Detect the current host and evaluate its development deployment capabilities.
pub fn local_deployment_capabilities() -> Vec<RuntimeCapability> {
    RuntimeCapabilityEvaluator::deployment(&DeploymentCapabilityProbe::detect(), false)
}

#[derive(Debug, Clone)]
pub struct DeploymentCapabilityProbe {
    pub platform_supports_container: bool,
    pub podman_available: bool,
    pub podman_probe_failed: bool,
    pub browser_available: bool,
    pub claude_code_available: bool,
    pub codex_available: bool,
    pub checked_at: String,
}

impl DeploymentCapabilityProbe {
    pub fn detect() -> Self {
        Self {
            platform_supports_container: cfg!(target_os = "linux"),
            podman_available: command_available("podman"),
            podman_probe_failed: false,
            browser_available: false,
            claude_code_available: command_available("claude"),
            codex_available: command_available("codex"),
            checked_at: format_utc(Utc::now()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectiveCapabilityConfig {
    pub executor: ExecutorKind,
    pub production: bool,
    pub allow_insecure_local_executor: bool,
    pub bash_egress_configured: bool,
    pub live_preview_configured: bool,
    pub scope: CapabilityScope,
}

pub struct RuntimeCapabilityEvaluator;

impl RuntimeCapabilityEvaluator {
    pub fn deployment(
        probe: &DeploymentCapabilityProbe,
        production: bool,
    ) -> Vec<RuntimeCapability> {
        let executor = if probe.platform_supports_container && probe.podman_available {
            ExecutorKind::Container
        } else {
            ExecutorKind::Local
        };
        Self::effective(
            probe,
            EffectiveCapabilityConfig {
                executor,
                production,
                allow_insecure_local_executor: !production,
                bash_egress_configured: false,
                live_preview_configured: false,
                scope: CapabilityScope::Deployment,
            },
        )
    }

    pub fn effective(
        probe: &DeploymentCapabilityProbe,
        config: EffectiveCapabilityConfig,
    ) -> Vec<RuntimeCapability> {
        RuntimeCapabilityId::ALL
            .into_iter()
            .map(|id| {
                let (state, reason) = evaluate_capability(id, probe, config);
                RuntimeCapability::new(id, config.scope, state, reason)
                    .expect("the exhaustive runtime capability matrix is valid")
                    .with_checked_at(probe.checked_at.clone())
            })
            .collect()
    }
}

fn evaluate_capability(
    id: RuntimeCapabilityId,
    probe: &DeploymentCapabilityProbe,
    config: EffectiveCapabilityConfig,
) -> (CapabilityState, Option<CapabilityReason>) {
    let local_allowed = !config.production || config.allow_insecure_local_executor;
    match id {
        RuntimeCapabilityId::ProcessExecution => match config.executor {
            ExecutorKind::Local if local_allowed => (
                CapabilityState::Degraded,
                Some(CapabilityReason::LocalExecutor),
            ),
            ExecutorKind::Local => (
                CapabilityState::Unconfigured,
                Some(CapabilityReason::PolicyDisabled),
            ),
            ExecutorKind::Container if !probe.platform_supports_container => (
                CapabilityState::Unsupported,
                Some(CapabilityReason::PlatformUnsupported),
            ),
            ExecutorKind::Container if probe.podman_probe_failed => (
                CapabilityState::Unconfigured,
                Some(CapabilityReason::ProbeFailed),
            ),
            ExecutorKind::Container if !probe.podman_available => (
                CapabilityState::Unconfigured,
                Some(CapabilityReason::DependencyMissing),
            ),
            ExecutorKind::Container => (CapabilityState::Ready, None),
        },
        RuntimeCapabilityId::ContainerIsolation => {
            if !probe.platform_supports_container {
                (
                    CapabilityState::Unsupported,
                    Some(CapabilityReason::PlatformUnsupported),
                )
            } else if config.executor == ExecutorKind::Local {
                (
                    CapabilityState::Unsupported,
                    Some(CapabilityReason::LocalExecutor),
                )
            } else if probe.podman_probe_failed {
                (
                    CapabilityState::Unconfigured,
                    Some(CapabilityReason::ProbeFailed),
                )
            } else if !probe.podman_available {
                (
                    CapabilityState::Unconfigured,
                    Some(CapabilityReason::DependencyMissing),
                )
            } else {
                (CapabilityState::Ready, None)
            }
        }
        RuntimeCapabilityId::BashEgress => {
            if !config.bash_egress_configured {
                (
                    CapabilityState::Unconfigured,
                    Some(CapabilityReason::ConfigMissing),
                )
            } else if config.executor == ExecutorKind::Local {
                (
                    CapabilityState::Degraded,
                    Some(CapabilityReason::LocalExecutor),
                )
            } else {
                (CapabilityState::Ready, None)
            }
        }
        RuntimeCapabilityId::Browser => dependency_capability(probe.browser_available),
        RuntimeCapabilityId::LivePreview => {
            if config.live_preview_configured {
                (CapabilityState::Ready, None)
            } else {
                (
                    CapabilityState::Unconfigured,
                    Some(CapabilityReason::ConfigMissing),
                )
            }
        }
        RuntimeCapabilityId::DelegatedCliClaudeCode => {
            dependency_capability(probe.claude_code_available)
        }
        RuntimeCapabilityId::DelegatedCliCodex => dependency_capability(probe.codex_available),
    }
}

const fn dependency_capability(available: bool) -> (CapabilityState, Option<CapabilityReason>) {
    if available {
        (CapabilityState::Ready, None)
    } else {
        (
            CapabilityState::Unconfigured,
            Some(CapabilityReason::DependencyMissing),
        )
    }
}

fn command_available(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var_os("PATHEXT")
            .map(|value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .filter(|value| !value.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect()
            })
            .unwrap_or_else(|| vec![".exe".into(), ".cmd".into(), ".bat".into()])
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).any(|directory| {
        extensions.iter().any(|extension| {
            let candidate = if extension.is_empty() {
                directory.join(command)
            } else {
                directory.join(format!("{command}{extension}"))
            };
            Path::new(&candidate).is_file()
        })
    })
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Canceled,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Starting,
    Running,
    Unhealthy,
    Stopping,
    Stopped,
    StoppedAfterRestart,
    Failed,
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
pub enum ServiceImpact {
    ReadOnly,
    IgnoredOutput,
    SourceWriting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Sync,
    Job,
    Service,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    DenyAll,
    ProjectRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedCliKind {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct RelativeWorkingDirectory(String);

impl RelativeWorkingDirectory {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeError> {
        let value = value.into();
        let normalized = value.trim();
        if normalized.is_empty() || normalized == "." {
            return Ok(Self(".".into()));
        }
        if normalized.starts_with('/')
            || normalized.starts_with('\\')
            || normalized.contains('\\')
            || normalized.contains('\0')
        {
            return Err(RuntimeError::InvalidSpec(
                "working_directory must use a workspace-relative slash path".into(),
            ));
        }
        let mut segments = normalized.split('/');
        let first = segments
            .next()
            .expect("a non-empty relative path has a first segment");
        if first.ends_with(':')
            || first.is_empty()
            || first == "."
            || first == ".."
            || segments.any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(RuntimeError::InvalidSpec(
                "working_directory contains an invalid path segment".into(),
            ));
        }
        Ok(Self(normalized.into()))
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
pub enum CommandKind {
    Shell,
    DelegatedCli {
        cli: DelegatedCliKind,
        session_id: Option<CliSessionId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCommand {
    kind: CommandKind,
    input: String,
}

impl ValidatedCommand {
    pub fn shell(script: impl Into<String>) -> Result<Self, RuntimeError> {
        Self::new(CommandKind::Shell, script.into())
    }

    pub fn delegated_cli(
        cli: DelegatedCliKind,
        instruction: impl Into<String>,
        session_id: Option<CliSessionId>,
    ) -> Result<Self, RuntimeError> {
        Self::new(
            CommandKind::DelegatedCli { cli, session_id },
            instruction.into(),
        )
    }

    fn new(kind: CommandKind, input: String) -> Result<Self, RuntimeError> {
        if input.trim().is_empty() {
            return Err(RuntimeError::InvalidSpec(
                "command input cannot be empty".into(),
            ));
        }
        if input.len() > 1024 * 1024 {
            return Err(RuntimeError::InvalidSpec(
                "command input exceeds the one MiB contract limit".into(),
            ));
        }
        Ok(Self { kind, input })
    }

    pub fn kind(&self) -> &CommandKind {
        &self.kind
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
    session_id: SessionId,
    executor: ExecutorKind,
    workspace_root: PathBuf,
    limits: ResourceLimits,
    network_policy: NetworkPolicy,
}

impl RuntimeSpec {
    pub fn new(
        id: RuntimeId,
        session_id: SessionId,
        executor: ExecutorKind,
        workspace_root: PathBuf,
        limits: ResourceLimits,
        network_policy: NetworkPolicy,
    ) -> Result<Self, RuntimeError> {
        if !workspace_root.is_absolute() {
            return Err(RuntimeError::InvalidSpec(
                "workspace_root must be an absolute trusted path".into(),
            ));
        }
        limits.validate()?;
        Ok(Self {
            id,
            session_id,
            executor,
            workspace_root,
            limits,
            network_policy,
        })
    }

    pub fn id(&self) -> RuntimeId {
        self.id
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn executor(&self) -> ExecutorKind {
        self.executor
    }

    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    pub fn network_policy(&self) -> NetworkPolicy {
        self.network_policy
    }
}

#[derive(Debug)]
pub struct ExecutionSpec {
    runtime_id: RuntimeId,
    working_directory: RelativeWorkingDirectory,
    command: ValidatedCommand,
    environment: ExecutionEnvironment,
    limits: ResourceLimits,
    network_policy: NetworkPolicy,
}

impl ExecutionSpec {
    pub fn new(
        runtime_id: RuntimeId,
        working_directory: RelativeWorkingDirectory,
        command: ValidatedCommand,
        environment: ExecutionEnvironment,
        limits: ResourceLimits,
        network_policy: NetworkPolicy,
    ) -> Result<Self, RuntimeError> {
        limits.validate()?;
        Ok(Self {
            runtime_id,
            working_directory,
            command,
            environment,
            limits,
            network_policy,
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

    pub fn network_policy(&self) -> NetworkPolicy {
        self.network_policy
    }
}

#[derive(Debug)]
pub struct JobSpec {
    pub id: JobId,
    pub session_id: SessionId,
    pub controlling_turn_id: TurnId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub execution: ExecutionSpec,
}

impl JobSpec {
    pub fn new(
        id: JobId,
        session_id: SessionId,
        controlling_turn_id: TurnId,
        initiated_by_tool_call_id: ToolCallId,
        execution: ExecutionSpec,
    ) -> Result<Self, RuntimeError> {
        if matches!(execution.command().kind(), CommandKind::DelegatedCli { .. })
            || matches!(execution.command().kind(), CommandKind::Shell)
        {
            return Ok(Self {
                id,
                session_id,
                controlling_turn_id,
                initiated_by_tool_call_id,
                execution,
            });
        }
        Err(RuntimeError::InvalidSpec(
            "unsupported finite Job command kind".into(),
        ))
    }
}

#[derive(Debug)]
pub struct ServiceSpec {
    pub id: ServiceId,
    pub session_id: SessionId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub impact: ServiceImpact,
    pub execution: ExecutionSpec,
}

impl ServiceSpec {
    pub fn new(
        id: ServiceId,
        session_id: SessionId,
        initiated_by_tool_call_id: ToolCallId,
        impact: ServiceImpact,
        execution: ExecutionSpec,
    ) -> Result<Self, RuntimeError> {
        if !matches!(execution.command().kind(), CommandKind::Shell) {
            return Err(RuntimeError::InvalidSpec(
                "a Service must use a shell command".into(),
            ));
        }
        Ok(Self {
            id,
            session_id,
            initiated_by_tool_call_id,
            impact,
            execution,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum TerminalOwner {
    Project(ProjectId),
    Session(SessionId),
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
    pub owner: TerminalOwner,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceHealth {
    Unknown,
    Healthy,
    Unhealthy,
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

    fn start_job<'a>(
        &'a self,
        spec: JobSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>>;

    fn wait_job<'a>(
        &'a self,
        id: JobId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    fn write_job_stdin<'a>(
        &'a self,
        id: JobId,
        executor_nonce: &'a str,
        input: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;

    fn cancel_job<'a>(
        &'a self,
        id: JobId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    fn start_service<'a>(
        &'a self,
        spec: ServiceSpec,
        log_stream_id: LogStreamId,
    ) -> BoxFuture<'a, Result<ExecutorProcessHandle, RuntimeError>>;

    fn wait_service<'a>(
        &'a self,
        id: ServiceId,
        executor_nonce: &'a str,
    ) -> BoxFuture<'a, Result<ProcessCompletion, RuntimeError>>;

    fn stop_service<'a>(
        &'a self,
        id: ServiceId,
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
    pub session_id: SessionId,
    pub executor: ExecutorKind,
    pub status: RuntimeStatus,
    pub capabilities: Vec<RuntimeCapability>,
    pub limits: ResourceLimits,
    pub version: String,
    pub created_at: String,
    pub updated_at: String,
    pub stopped_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JobProjection {
    pub id: JobId,
    pub runtime_id: RuntimeId,
    pub session_id: SessionId,
    pub controlling_turn_id: TurnId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub cli_session_id: Option<CliSessionId>,
    pub status: JobStatus,
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
pub struct ServiceProjection {
    pub id: ServiceId,
    pub runtime_id: RuntimeId,
    pub session_id: SessionId,
    pub initiated_by_tool_call_id: ToolCallId,
    pub status: ServiceStatus,
    pub impact: ServiceImpact,
    pub command_summary: String,
    pub health: ServiceHealth,
    pub log_stream_id: LogStreamId,
    pub ports: Vec<RuntimePortProjection>,
    pub exit: Option<ExitSummary>,
    pub version: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Http,
    Https,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuntimePortProjection {
    pub id: RuntimePortId,
    pub service_id: ServiceId,
    pub name: String,
    pub protocol: PortProtocol,
    pub internal_port: u16,
    pub health_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TerminalProjection {
    pub id: TerminalId,
    pub runtime_id: RuntimeId,
    pub owner: TerminalOwner,
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
    Job,
    Service,
    Terminal,
}

impl LogOwnerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Job => "job",
            Self::Service => "service",
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
    CommandForbidden,
    NetworkPolicyDenied,
    RuntimeUnavailable,
    JobLost,
    ServiceLost,
    TerminalTicketInvalid,
    TerminalScrollbackExpired,
    TerminalNotWritable,
}

impl RuntimeErrorCode {
    pub const ALL: [Self; 10] = [
        Self::ValidationFailed,
        Self::ResourceBusy,
        Self::CommandForbidden,
        Self::NetworkPolicyDenied,
        Self::RuntimeUnavailable,
        Self::JobLost,
        Self::ServiceLost,
        Self::TerminalTicketInvalid,
        Self::TerminalScrollbackExpired,
        Self::TerminalNotWritable,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ValidationFailed => "VALIDATION_FAILED",
            Self::ResourceBusy => "RESOURCE_BUSY",
            Self::CommandForbidden => "COMMAND_FORBIDDEN",
            Self::NetworkPolicyDenied => "NETWORK_POLICY_DENIED",
            Self::RuntimeUnavailable => "RUNTIME_UNAVAILABLE",
            Self::JobLost => "JOB_LOST",
            Self::ServiceLost => "SERVICE_LOST",
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
    #[error("the command is forbidden by runtime policy")]
    CommandForbidden,
    #[error("the destination is denied by network policy")]
    NetworkPolicyDenied,
    #[error("the runtime is unavailable")]
    RuntimeUnavailable,
    #[error("job {0} can no longer be controlled")]
    JobLost(JobId),
    #[error("service {0} can no longer be controlled")]
    ServiceLost(ServiceId),
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
            Self::CommandForbidden => RuntimeErrorCode::CommandForbidden,
            Self::NetworkPolicyDenied => RuntimeErrorCode::NetworkPolicyDenied,
            Self::RuntimeUnavailable => RuntimeErrorCode::RuntimeUnavailable,
            Self::JobLost(_) => RuntimeErrorCode::JobLost,
            Self::ServiceLost(_) => RuntimeErrorCode::ServiceLost,
            Self::TerminalTicketInvalid => RuntimeErrorCode::TerminalTicketInvalid,
            Self::TerminalScrollbackExpired { .. } => RuntimeErrorCode::TerminalScrollbackExpired,
            Self::TerminalNotWritable(_) => RuntimeErrorCode::TerminalNotWritable,
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(self, Self::ResourceBusy | Self::RuntimeUnavailable)
    }
}
