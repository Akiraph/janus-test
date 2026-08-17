//! Append-only public event log for SSE and external observation.
//!
//! Not an internal command bus - modules must not use this to trigger each other's work.

use std::{fmt, str::FromStr, sync::Arc};

use crate::clock::now_utc_str;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};
use tokio::sync::broadcast;
use utoipa::ToSchema;

use crate::id::EventId;

/// Canonical public event types. Every event appended anywhere in the system
/// must use one of these variants, so consumers (notably the projection engine)
/// can `match` exhaustively and the compiler flags a new event type that nobody
/// projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    TurnCreated,
    TurnStatusChanged,
    SessionChanged,
    SessionDeleted,
    ContextChanged,
    TimelineItemCreated,
    TimelineItemUpdated,
    ToolCallCreated,
    ToolCallChanged,
    RoundChanged,
    ProjectChanged,
    ProjectMainRevisionChanged,
    TerminalChanged,
    RuntimeChanged,
    AsyncTaskChanged,
    OperationChanged,
    NotificationChannelChanged,
    ModelConfigChanged,
    ModelStreamDelta,
    ModelAttemptRetrying,
    GitStateChanged,
    CheckpointCreated,
    SystemStarted,
}

impl EventType {
    pub const fn as_str(self) -> &'static str {
        use EventType::*;
        match self {
            TurnCreated => "turn.created",
            TurnStatusChanged => "turn.status_changed",
            SessionChanged => "session.changed",
            SessionDeleted => "session.deleted",
            ContextChanged => "context.changed",
            TimelineItemCreated => "timeline.item_created",
            TimelineItemUpdated => "timeline.item_updated",
            ToolCallCreated => "tool_call.created",
            ToolCallChanged => "tool_call.changed",
            RoundChanged => "round.changed",
            ProjectChanged => "project.changed",
            ProjectMainRevisionChanged => "project.main_revision_changed",
            TerminalChanged => "terminal.changed",
            RuntimeChanged => "runtime.changed",
            AsyncTaskChanged => "async_task.changed",
            OperationChanged => "operation.changed",
            NotificationChannelChanged => "notification_channel.changed",
            ModelConfigChanged => "model_config.changed",
            ModelStreamDelta => "model.stream_delta",
            ModelAttemptRetrying => "model.attempt_retrying",
            GitStateChanged => "git.state_changed",
            CheckpointCreated => "checkpoint.created",
            SystemStarted => "system.started",
        }
    }
}

impl FromStr for EventType {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        use EventType::*;
        match value {
            "turn.created" => Ok(TurnCreated),
            "turn.status_changed" => Ok(TurnStatusChanged),
            "session.changed" => Ok(SessionChanged),
            "session.deleted" => Ok(SessionDeleted),
            "context.changed" => Ok(ContextChanged),
            "timeline.item_created" => Ok(TimelineItemCreated),
            "timeline.item_updated" => Ok(TimelineItemUpdated),
            "tool_call.created" => Ok(ToolCallCreated),
            "tool_call.changed" => Ok(ToolCallChanged),
            "round.changed" => Ok(RoundChanged),
            "project.changed" => Ok(ProjectChanged),
            "project.main_revision_changed" => Ok(ProjectMainRevisionChanged),
            "terminal.changed" => Ok(TerminalChanged),
            "runtime.changed" => Ok(RuntimeChanged),
            "async_task.changed" => Ok(AsyncTaskChanged),
            "operation.changed" => Ok(OperationChanged),
            "notification_channel.changed" => Ok(NotificationChannelChanged),
            "model_config.changed" => Ok(ModelConfigChanged),
            "model.stream_delta" => Ok(ModelStreamDelta),
            "model.attempt_retrying" => Ok(ModelAttemptRetrying),
            "git.state_changed" => Ok(GitStateChanged),
            "checkpoint.created" => Ok(CheckpointCreated),
            "system.started" => Ok(SystemStarted),
            _ => Err("unknown event type"),
        }
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub event_id: String,
    pub cursor: String,
    pub event_type: String,
    pub occurred_at: String,
    pub actor: Value,
    pub resource: Option<Value>,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub event_type: EventType,
    pub actor: Value,
    pub resource: Option<Value>,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventBounds {
    pub min: u64,
    pub max: u64,
}

#[derive(Debug, Clone, FromRow)]
struct EventRow {
    cursor: i64,
    event_id: String,
    event_type: String,
    schema_version: i64,
    actor_json: String,
    resource_json: Option<String>,
    correlation_id: String,
    causation_id: Option<String>,
    payload_json: String,
    occurred_at: String,
}

#[derive(Clone)]
pub struct EventStore {
    inner: Arc<EventStoreInner>,
}

struct EventStoreInner {
    pool: SqlitePool,
    // Broadcast delivery is only a wake-up hint. Slow consumers may miss it and
    // must use the cursor query to catch up.
    notifier: broadcast::Sender<()>,
}

impl EventStore {
    pub fn new(pool: SqlitePool) -> Self {
        let (notifier, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(EventStoreInner { pool, notifier }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.notifier.subscribe()
    }

    pub async fn append(&self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        let mut transaction = self.inner.pool.begin().await?;
        let envelope = self.append_in_tx(&mut transaction, event).await?;
        transaction.commit().await?;
        // Standalone append path: same commit-then-notify rule as `UnitOfWork`.
        self.notify_committed();
        Ok(envelope)
    }

    pub(crate) async fn append_in_tx(
        &self,
        transaction: &mut sqlx::SqliteConnection,
        event: NewEvent,
    ) -> anyhow::Result<EventEnvelope> {
        let event_id = EventId::new().to_string();
        let occurred_at = now_utc_str();
        let result = sqlx::query(
            "INSERT INTO public_events (event_id, event_type, schema_version, actor_json, resource_json, correlation_id, causation_id, payload_json, occurred_at) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event_id)
        .bind(event.event_type.as_str())
        .bind(serde_json::to_string(&event.actor)?)
        .bind(event.resource.as_ref().map(serde_json::to_string).transpose()?)
        .bind(&event.correlation_id)
        .bind(&event.causation_id)
        .bind(serde_json::to_string(&event.payload)?)
        .bind(&occurred_at)
        .execute(&mut *transaction)
        .await
        .context("append public event")?;
        let cursor = u64::try_from(result.last_insert_rowid())?;

        Ok(EventEnvelope {
            schema_version: 1,
            event_id,
            cursor: cursor.to_string(),
            event_type: event.event_type.to_string(),
            occurred_at,
            actor: event.actor,
            resource: event.resource,
            correlation_id: event.correlation_id,
            causation_id: event.causation_id,
            payload: event.payload,
        })
    }

    pub(crate) fn notify_committed(&self) {
        let _ = self.inner.notifier.send(());
    }

    pub async fn bounds(&self) -> anyhow::Result<EventBounds> {
        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT MIN(cursor), MAX(cursor) FROM public_events",
        )
        .fetch_one(&self.inner.pool)
        .await?;
        Ok(EventBounds {
            min: u64::try_from(row.0.unwrap_or(0))?,
            max: u64::try_from(row.1.unwrap_or(0))?,
        })
    }

    pub async fn after(&self, cursor: u64, limit: u32) -> anyhow::Result<Vec<EventEnvelope>> {
        let cursor = i64::try_from(cursor)?;
        let limit = i64::from(limit.min(1000));
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT cursor, event_id, event_type, schema_version, actor_json, resource_json, correlation_id, causation_id, payload_json, occurred_at FROM public_events WHERE cursor > ? ORDER BY cursor ASC LIMIT ?",
        )
        .bind(cursor)
        .bind(limit)
        .fetch_all(&self.inner.pool)
        .await?;

        rows.into_iter().map(EventEnvelope::try_from).collect()
    }

    /// Last cursor the projection engine has processed. 0 means it has not
    /// persisted a position yet.
    pub async fn projection_cursor(&self) -> anyhow::Result<u64> {
        let row = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT cursor FROM projection_cursor WHERE id = 1",
        )
        .fetch_optional(&self.inner.pool)
        .await?;
        Ok(u64::try_from(row.flatten().unwrap_or(0))?)
    }

    /// Persist the projection engine's processed cursor. Called after each
    /// batch so a restart can resume from exactly this position.
    pub async fn set_projection_cursor(&self, cursor: u64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO projection_cursor (id, cursor) VALUES (1, ?) \
             ON CONFLICT(id) DO UPDATE SET cursor = excluded.cursor",
        )
        .bind(i64::try_from(cursor)?)
        .execute(&self.inner.pool)
        .await?;
        Ok(())
    }
}

impl TryFrom<EventRow> for EventEnvelope {
    type Error = anyhow::Error;

    fn try_from(row: EventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            schema_version: u16::try_from(row.schema_version)?,
            event_id: row.event_id,
            cursor: u64::try_from(row.cursor)?.to_string(),
            event_type: row.event_type,
            occurred_at: row.occurred_at,
            actor: serde_json::from_str(&row.actor_json)?,
            resource: row
                .resource_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            correlation_id: row.correlation_id,
            causation_id: row.causation_id,
            payload: serde_json::from_str(&row.payload_json)?,
        })
    }
}
