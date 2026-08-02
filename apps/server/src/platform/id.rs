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

typed_id!(TenantId);
typed_id!(ActorId);
typed_id!(OwnerId);
typed_id!(PasskeyId);
typed_id!(ProviderId);
typed_id!(ModelId);
typed_id!(ProjectId);
typed_id!(GithubCredentialId);
typed_id!(RevisionId);
typed_id!(GitUpdateConflictId);
// Session, Turn, Execution, Models, and Runtime identity surface.
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
