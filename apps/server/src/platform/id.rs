use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub const fn value(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

typed_id!(EventId);
typed_id!(RequestId);
typed_id!(CorrelationId);
typed_id!(CausationId);
typed_id!(TenantId);
typed_id!(ActorId);
typed_id!(OwnerId);
typed_id!(PasskeyId);
typed_id!(ProviderId);
typed_id!(ModelId);
typed_id!(ProjectId);
typed_id!(OperationId);
typed_id!(WorkItemId);
typed_id!(GithubCredentialId);
typed_id!(RevisionId);
typed_id!(GitUpdateConflictId);
// M3 Session / Turn / Supervisor / Models identity surface.
typed_id!(SessionId);
typed_id!(TurnId);
typed_id!(MessageId);
typed_id!(RoundId);
typed_id!(ToolCallId);
typed_id!(CheckpointId);
typed_id!(AttachmentId);
typed_id!(UploadId);
typed_id!(TimelineItemId);
typed_id!(AttemptId);
typed_id!(SnapshotId);

/// Lowercase hex SHA-256 of a content-addressed blob. Not a UUID: the value is
/// derived from the bytes, so it is constructed from a string, not generated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct BlobSha(String);

impl BlobSha {
    pub fn from_hex(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlobSha {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
