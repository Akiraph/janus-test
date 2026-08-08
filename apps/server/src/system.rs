//! Narrow read facade for the system-status + projection surface.
//!
//! Health/SSE transports depend on this instead of reaching into
//! `Database`/`EventStore`/`Application` directly. The composition root keeps
//! owning the raw services; this exposes only what the public system surface
//! needs: readiness, schema version, event cursor reads, the live state
//! subscription, and event projection.

use std::sync::Arc;

use janus_infrastructure::{
    database::Database,
    events::{EventBounds, EventEnvelope, EventStore, NewEvent},
    managed_storage::BlobStore,
    state_broadcaster::{StateBroadcaster, StateChange},
};
use tokio::sync::broadcast;

use crate::application::{state_worker::project_event, Application};

/// Read-only surface over the system plumbing. Transports never hold
/// `Database`/`EventStore`/`Application`; they hold this.
pub struct SystemRead {
    database: Database,
    events: EventStore,
    blobs: BlobStore,
    broadcaster: StateBroadcaster,
    application: Application,
}

impl SystemRead {
    pub(crate) fn new(
        database: Database,
        events: EventStore,
        blobs: BlobStore,
        broadcaster: StateBroadcaster,
        application: Application,
    ) -> Self {
        Self {
            database,
            events,
            blobs,
            broadcaster,
            application,
        }
    }

    /// Database readiness probe (used by `/health/ready` and `/system/info`).
    pub async fn ready(&self) -> bool {
        self.database.ready().await
    }

    /// Highest applied schema version.
    pub async fn schema_version(&self) -> anyhow::Result<i64> {
        self.database.schema_version().await
    }

    /// Committed event-log cursor range.
    pub async fn events_bounds(&self) -> anyhow::Result<EventBounds> {
        self.events.bounds().await
    }

    /// Committed events after `cursor`, capped at `limit`.
    pub async fn events_after(
        &self,
        cursor: u64,
        limit: u32,
    ) -> anyhow::Result<Vec<EventEnvelope>> {
        self.events.after(cursor, limit).await
    }

    /// Append a system-level event (used by `main` for the boot event).
    pub async fn append(&self, event: NewEvent) -> anyhow::Result<EventEnvelope> {
        self.events.append(event).await
    }

    /// Startup blob recovery: remove crash leftovers, sweep unreferenced
    /// objects, and clean stale incoming uploads. Called from `main` while
    /// `/health/ready` is held at 503.
    pub async fn recover_blobs(&self) -> anyhow::Result<()> {
        self.blobs.recover_cleanup().await?;
        self.blobs.sweep_unreferenced().await?;
        self.blobs.clean_incoming().await
    }

    /// Live subscription to projected state changes (SSE real-time path).
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StateChange>> {
        self.broadcaster.subscribe()
    }

    /// Project one committed event into the state changes it drives, for the
    /// given owner (the SSE replay re-projects per authenticated owner).
    pub async fn project(
        &self,
        owner: Option<&str>,
        event: &EventEnvelope,
    ) -> Vec<StateChange> {
        project_event(&self.application, owner, event).await
    }
}
