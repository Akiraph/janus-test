use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};
use tokio::sync::broadcast;
use utoipa::ToSchema;

use super::{
    clock::{Clock, SystemClock, format_utc},
    id::EventId,
};

pub const EVENT_REGISTRY: &[(&str, u16, &str)] = &[
    ("system.started", 1, "platform"),
    ("identity.changed", 1, "identity"),
    ("model_config.changed", 1, "models"),
    ("operation.changed", 1, "platform"),
    ("project.changed", 1, "projects"),
    ("project.main_revision_changed", 1, "projects"),
    ("git.state_changed", 1, "projects"),
    ("git.update_conflict_changed", 1, "projects"),
    // M3 sessions
    ("session.changed", 1, "sessions"),
    ("session.deleted", 1, "sessions"),
    ("turn.created", 1, "sessions"),
    ("turn.status_changed", 1, "sessions"),
    ("timeline.item_created", 1, "sessions"),
    ("timeline.item_updated", 1, "sessions"),
    ("checkpoint.created", 1, "sessions"),
    // M3 models
    ("model.stream_delta", 1, "models"),
    ("model.attempt_changed", 1, "models"),
    // M3 supervisor
    ("round.changed", 1, "supervisor"),
    ("tool_call.created", 1, "supervisor"),
    ("tool_call.changed", 1, "supervisor"),
    // M3 workspace-sync
    ("session.revision_changed", 1, "workspace-sync"),
    ("workspace.diff_changed", 1, "workspace-sync"),
];

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
    pub event_type: String,
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
        let event_id = EventId::new().to_string();
        let occurred_at = format_utc(SystemClock.now());
        let result = sqlx::query(
            "INSERT INTO public_events (event_id, event_type, schema_version, actor_json, resource_json, correlation_id, causation_id, payload_json, occurred_at) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event_id)
        .bind(&event.event_type)
        .bind(serde_json::to_string(&event.actor)?)
        .bind(event.resource.as_ref().map(serde_json::to_string).transpose()?)
        .bind(&event.correlation_id)
        .bind(&event.causation_id)
        .bind(serde_json::to_string(&event.payload)?)
        .bind(&occurred_at)
        .execute(&self.inner.pool)
        .await
        .context("append public event")?;
        let cursor = u64::try_from(result.last_insert_rowid())?;
        let _ = self.inner.notifier.send(());

        Ok(EventEnvelope {
            schema_version: 1,
            event_id,
            cursor: cursor.to_string(),
            event_type: event.event_type,
            occurred_at,
            actor: event.actor,
            resource: event.resource,
            correlation_id: event.correlation_id,
            causation_id: event.causation_id,
            payload: event.payload,
        })
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
