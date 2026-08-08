//! Typed IDs for infrastructure-owned rows and wire formats.
//!
//! All generated IDs use UUID v7 (time-sortable, unique without a central allocator).

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
typed_id!(OperationId);
typed_id!(WorkItemId);

// Capability IDs remain technical UUID wrappers. Their owning tables and
// domain rules live in the corresponding capability crate.
typed_id!(ActorId);
typed_id!(OwnerId);
typed_id!(PasskeyId);
typed_id!(ProviderId);
typed_id!(ModelId);
typed_id!(ProjectId);
typed_id!(GithubCredentialId);
typed_id!(RevisionId);
typed_id!(GitUpdateConflictId);
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
typed_id!(RuntimeId);
typed_id!(JobId);
typed_id!(ServiceId);
typed_id!(TerminalId);
typed_id!(LogStreamId);
typed_id!(RuntimeTicketId);
typed_id!(RuntimePortId);
typed_id!(CliSessionId);
typed_id!(RuntimeSecretId);
typed_id!(EgressRuleId);
typed_id!(CliConfigId);
typed_id!(AskId);
typed_id!(PlanVersionId);
typed_id!(ContextVersionId);
typed_id!(CompactSummaryId);
typed_id!(NotificationChannelId);

/// A storage label returned by trusted blob operations; parsing it does not
/// prove that the value is a SHA-256 digest or that the object exists.
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

#[cfg(test)]
mod tests {
    use super::{BlobSha, CorrelationId, OperationId};

    #[test]
    fn typed_ids_round_trip_through_strings() {
        let id = CorrelationId::new();
        assert_eq!(id, id.to_string().parse().expect("valid correlation id"));

        let operation = OperationId::new();
        assert_eq!(
            operation,
            operation.to_string().parse().expect("valid operation id")
        );
    }

    #[test]
    fn blob_sha_keeps_the_content_address() {
        let sha = BlobSha::from_hex("abc123".to_owned());
        assert_eq!(sha.as_str(), "abc123");
        assert_eq!(sha.to_string(), "abc123");
    }
}
