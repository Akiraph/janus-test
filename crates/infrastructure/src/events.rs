//! Append-only public event log for SSE and external observation.
//!
//! Not an internal command bus - modules must not use this to trigger each other's work.

use std::{fmt, str::FromStr, sync::Arc};

use crate::clock::now_utc_str;
use anyhow::Context;
use futures_util::TryStreamExt;
use mongodb::{
    ClientSession,
    bson::{Bson, Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Clone)]
pub struct EventStore {
    inner: Arc<EventStoreInner>,
}

struct EventStoreInner {
    pool: mongodb::Database,
    // Broadcast delivery is only a wake-up hint. Slow consumers may miss it and
    // must use the cursor query to catch up.
    notifier: broadcast::Sender<()>,
    // Serializes event-cursor allocation against overlapping transactions; see
    // unit_of_work.rs. Owned by `Arc` so `lock_owned` yields an
    // `OwnedMutexGuard` that can sit next to a `ClientSession`.
    append_serial: Arc<tokio::sync::Mutex<()>>,
}

impl EventStore {
    pub fn new(pool: mongodb::Database) -> Self {
        let (notifier, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(EventStoreInner {
                pool,
                notifier,
                append_serial: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.notifier.subscribe()
    }

    /// Owned guard that serializes the event-cursor `$inc`. Held for the whole
    /// lifetime of a `UnitOfWorkTransaction`, or for a standalone `append`.
    pub async fn append_lock(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.inner.append_serial.clone().lock_owned().await
    }

    pub async fn append(&self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        let _guard = self.append_lock().await;
        let mut session = self.inner.pool.client().start_session().await?;
        session.start_transaction().await?;
        let envelope = self.append_in_tx(&mut session, event).await?;
        session.commit_transaction().await?;
        // Standalone append path: same commit-then-notify rule as `UnitOfWork`.
        self.notify_committed();
        Ok(envelope)
    }

    pub(crate) async fn append_in_tx(
        &self,
        session: &mut ClientSession,
        event: NewEvent,
    ) -> anyhow::Result<EventEnvelope> {
        let event_id = EventId::new().to_string();
        let occurred_at = now_utc_str();
        // Allocate the cursor inside the same transaction as the insert. A
        // rollback returns the counter to its previous value, so a committed
        // event can never leave a hole behind itself, and the per-process
        // append lock keeps two overlapping transactions from conflicting on
        // the counter document.
        let next = self
            .inner
            .pool
            .collection::<Document>("event_seq")
            .find_one_and_update(doc! {"_id": "global"}, doc! {"$inc": {"value": 1i64}})
            .upsert(true)
            .return_document(ReturnDocument::After)
            .session(&mut *session)
            .await
            .context("allocate event cursor")?
            .ok_or_else(|| anyhow::anyhow!("event cursor did not materialize"))?;
        let cursor = next
            .get_i64("value")
            .context("event cursor missing value")?;

        let actor_json = serde_json::to_string(&event.actor)?;
        let payload_json = serde_json::to_string(&event.payload)?;
        let mut document = doc! {
            "_id": cursor,
            "event_id": &event_id,
            "event_type": event.event_type.as_str(),
            "schema_version": 1i64,
            "actor_json": &actor_json,
            "correlation_id": &event.correlation_id,
            "payload_json": &payload_json,
            "occurred_at": &occurred_at,
        };
        if let Some(resource) = &event.resource {
            document.insert("resource_json", serde_json::to_string(resource)?);
        }
        if let Some(causation_id) = &event.causation_id {
            document.insert("causation_id", causation_id);
        }
        self.inner
            .pool
            .collection::<Document>("public_events")
            .insert_one(document)
            .session(&mut *session)
            .await
            .context("append public event")?;

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
        let collection = self.inner.pool.collection::<Document>("public_events");
        let max = collection.find_one(doc! {}).sort(doc! {"_id": -1}).await?;
        let min = collection.find_one(doc! {}).sort(doc! {"_id": 1}).await?;
        let min = min
            .and_then(|document| document.get_i64("_id").ok())
            .unwrap_or(0);
        let max = max
            .and_then(|document| document.get_i64("_id").ok())
            .unwrap_or(0);
        Ok(EventBounds {
            min: u64::try_from(min)?,
            max: u64::try_from(max)?,
        })
    }

    /// Read events strictly after `cursor`, in cursor order.
    ///
    /// Returns only the contiguous prefix: if the first row is not `cursor + 1`,
    /// an event below it is still inside an uncommitted transaction, so an empty
    /// result stalls the reader until that transaction lands. The projection
    /// engine relies on never being handed a gap, so it can never skip an event.
    pub async fn after(&self, cursor: u64, limit: u32) -> anyhow::Result<Vec<EventEnvelope>> {
        let cursor = i64::try_from(cursor)?;
        let limit = i64::from(limit.min(1000));
        let mut rows = self
            .inner
            .pool
            .collection::<Document>("public_events")
            .find(doc! {"_id": {"$gt": cursor}})
            .sort(doc! {"_id": 1})
            .limit(limit)
            .await?;
        let mut documents = Vec::new();
        while let Some(document) = rows.try_next().await? {
            documents.push(document);
        }
        let mut envelopes = Vec::new();
        for (expected, document) in (cursor + 1..).zip(documents) {
            if document.get_i64("_id")? != expected {
                break;
            }
            envelopes.push(EventEnvelope::try_from(document)?);
        }
        Ok(envelopes)
    }

    /// Last cursor the projection engine has processed. 0 means it has not
    /// persisted a position yet.
    pub async fn projection_cursor(&self) -> anyhow::Result<u64> {
        let document = self
            .inner
            .pool
            .collection::<Document>("projection_cursor")
            .find_one(doc! {"_id": "1"})
            .await?;
        let cursor = document
            .and_then(|document| document.get_i64("cursor").ok())
            .unwrap_or(0);
        Ok(u64::try_from(cursor)?)
    }

    /// Persist the projection engine's processed cursor. Called after each
    /// batch so a restart can resume from exactly this position.
    pub async fn set_projection_cursor(&self, cursor: u64) -> anyhow::Result<()> {
        let cursor = i64::try_from(cursor)?;
        self.inner
            .pool
            .collection::<Document>("projection_cursor")
            .update_one(doc! {"_id": "1"}, doc! {"$set": {"cursor": cursor}})
            .upsert(true)
            .await?;
        Ok(())
    }
}

impl TryFrom<Document> for EventEnvelope {
    type Error = anyhow::Error;

    fn try_from(document: Document) -> Result<Self, Self::Error> {
        Ok(Self {
            schema_version: u16::try_from(document.get_i64("schema_version")?)?,
            event_id: document.get_str("event_id")?.to_owned(),
            cursor: u64::try_from(document.get_i64("_id")?)?.to_string(),
            event_type: document.get_str("event_type")?.to_owned(),
            occurred_at: document.get_str("occurred_at")?.to_owned(),
            actor: serde_json::from_str(document.get_str("actor_json")?)?,
            resource: document
                .get("resource_json")
                .and_then(Bson::as_str)
                .map(serde_json::from_str)
                .transpose()?,
            correlation_id: document.get_str("correlation_id")?.to_owned(),
            causation_id: document
                .get("causation_id")
                .and_then(Bson::as_str)
                .map(str::to_owned),
            payload: serde_json::from_str(document.get_str("payload_json")?)?,
        })
    }
}
