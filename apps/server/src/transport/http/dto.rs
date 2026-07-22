use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LiveResponse {
    pub status: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReadyResponse {
    pub status: &'static str,
    pub database: &'static str,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BootstrapResponse {
    pub data: BootstrapData,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BootstrapData {
    pub state: BootstrapState,
    pub development_auth: bool,
    pub webauthn_rp_name: String,
    pub version: &'static str,
    pub limits: PublicLimits,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapState {
    Uninitialized,
    Initialized,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct PublicLimits {
    pub max_file_bytes: u64,
    pub max_message_bytes: u64,
    pub max_attachments: u16,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SystemInfoResponse {
    pub data: SystemInfo,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SystemInfo {
    pub version: &'static str,
    pub schema_version: i64,
    pub mode: String,
    pub database: DatabaseInfo,
    pub events: EventInfo,
    pub capabilities: Vec<RuntimeCapability>,
    pub update_available: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DatabaseInfo {
    pub engine: &'static str,
    pub journal_mode: &'static str,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EventInfo {
    pub min_cursor: String,
    pub max_cursor: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RuntimeCapability {
    pub id: &'static str,
    pub scope: &'static str,
    pub state: CapabilityState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<CapabilityReason>,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Ready,
    Degraded,
    Unconfigured,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityReason {
    LocalExecutor,
    ConfigMissing,
    DependencyMissing,
    PlatformUnsupported,
    PolicyDisabled,
    ProbeFailed,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub after: Option<String>,
}
