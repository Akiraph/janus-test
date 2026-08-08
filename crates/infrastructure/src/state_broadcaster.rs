//! State broadcaster for real-time state push to SSE consumers.
//!
//! Unlike the legacy `EventStore` which emits "what changed" facts, this
//! channel broadcasts the *complete current projection* of a resource so
//! consumers can `setQueryData` without invalidation or re-fetch.
//!
//! Each `StateChange` carries the full projection of a single resource,
//! keyed by its `StateKind` + resource id. The SSE handler serializes
//! these frames and sends them to all connected clients.
//!
//! A slow consumer that cannot keep up is disconnected (the broadcast
//! channel capacity is bounded), forcing the client to reconnect and
//! receive a fresh `snapshot`.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

/// The kind of state being pushed. Maps directly to the `kind` field
/// in the SSE `state` frame and to the query key prefix on the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    /// Session summary (query key: ["session", id])
    Session,
    /// Session timeline page (query key: ["session-timeline", session_id])
    SessionTimeline,
    /// Session diff (query key: ["session-diff", session_id])
    SessionDiff,
    /// Session context usage (query key: ["session-context", session_id])
    SessionContext,
    /// Turn summary (query key: ["turn", session_id, turn_id])
    Turn,
    /// Queued turns list (query key: ["queued-turns", session_id])
    QueuedTurns,
    /// Project view (query key: ["project", id])
    Project,
    /// Projects list (query key: ["projects"])
    Projects,
    /// Git status (query key: ["git-status", project_id])
    GitStatus,
    /// Git log (query key: ["git-log", project_id, limit])
    GitLog,
    /// File tree (query key: ["file-tree", project_id, path])
    FileTree,
    /// Terminal projection (query key: ["terminal", id])
    Terminal,
    /// Terminals list (query key: ["terminals", project_id])
    Terminals,
    /// Model providers list (query key: ["model-providers"])
    Providers,
    /// System info (query key: ["system-info"])
    SystemInfo,
    /// Bootstrap (query key: ["bootstrap"])
    Bootstrap,
    /// Github credentials (query key: ["github-credentials"])
    GithubCredentials,
    /// Streaming assistant text/reasoning — not a persisted query, handled
    /// separately by the stream-text consumer hook.
    StreamText,
    /// Session list for a project (query key: ["sessions", project_id])
    Sessions,
    /// Runtime jobs list for a session (query key: ["jobs", session_id])
    Jobs,
    /// Notification channel list (query key: ["notification-channels"])
    NotificationChannels,
    /// Operation (query key: ["operations", id])
    Operation,
}

/// A single state change for one resource. The SSE handler serializes this
/// as `event: state` with `{"kind": ..., "id": ..., "data": ...}`.
#[derive(Debug, Clone)]
pub struct StateChange {
    pub kind: StateKind,
    /// Resource identifier (session id, project id, turn id, etc.).
    /// Absent for list-type changes (projects, providers, etc.).
    pub id: Option<String>,
    /// The complete projection of the resource. This is the same value
    /// that the corresponding GET endpoint would return.
    pub data: Value,
    /// The `public_events` cursor the projection was derived from, when the
    /// push was driven by an event. SSE consumers track the max cursor seen
    /// so a reconnect can resume from exactly where they left off.
    pub cursor: Option<String>,
}

/// Bounded broadcast channel for state push. Capacity is set high enough
/// for bursty Turn execution but low enough that a lagging consumer will
/// disconnect rather than accumulate unbounded memory.
const BROADCAST_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct StateBroadcaster {
    inner: Arc<StateBroadcasterInner>,
}

struct StateBroadcasterInner {
    tx: broadcast::Sender<Arc<StateChange>>,
}

impl StateBroadcaster {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(StateBroadcasterInner { tx }),
        }
    }

    /// Subscribe to state changes. Returns a receiver that may lag behind;
    /// if it does, `recv()` returns `RecvError::Lagged` and the consumer
    /// should disconnect and reconnect for a fresh snapshot.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StateChange>> {
        self.inner.tx.subscribe()
    }

    /// Push a state change to all connected consumers. If the channel is
    /// full, the oldest consumer is dropped (gets `Lagged` on next recv).
    pub fn push(&self, change: StateChange) {
        let _ = self.inner.tx.send(Arc::new(change));
    }

    /// Convenience: push a session state change.
    pub fn push_session(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::Session,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push a session timeline change.
    pub fn push_session_timeline(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::SessionTimeline,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push a turn state change.
    pub fn push_turn(&self, session_id: &str, turn_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::Turn,
            id: Some(format!("{session_id}_{turn_id}")),
            data,
            cursor: None,
        });
    }

    /// Convenience: push a stream text change (accumulated full text).
    pub fn push_stream_text(&self, session_id: &str, turn_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::StreamText,
            id: Some(format!("{session_id}:{turn_id}")),
            data,
            cursor: None,
        });
    }

    /// Convenience: push projects list.
    pub fn push_projects(&self, data: Value) {
        self.push(StateChange {
            kind: StateKind::Projects,
            id: None,
            data,
            cursor: None,
        });
    }

    /// Convenience: push a single project.
    pub fn push_project(&self, project_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::Project,
            id: Some(project_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push providers list.
    pub fn push_providers(&self, data: Value) {
        self.push(StateChange {
            kind: StateKind::Providers,
            id: None,
            data,
            cursor: None,
        });
    }

    /// Convenience: push system info.
    pub fn push_system_info(&self, data: Value) {
        self.push(StateChange {
            kind: StateKind::SystemInfo,
            id: None,
            data,
            cursor: None,
        });
    }

    /// Convenience: push bootstrap.
    pub fn push_bootstrap(&self, data: Value) {
        self.push(StateChange {
            kind: StateKind::Bootstrap,
            id: None,
            data,
            cursor: None,
        });
    }

    /// Convenience: push git status.
    pub fn push_git_status(&self, project_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::GitStatus,
            id: Some(project_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push sessions list for a project.
    pub fn push_sessions(&self, project_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::Sessions,
            id: Some(project_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push queued turns.
    pub fn push_queued_turns(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::QueuedTurns,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push runtime jobs for a session.
    pub fn push_jobs(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::Jobs,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push session timeline.
    pub fn push_timeline(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::SessionTimeline,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push session diff.
    pub fn push_session_diff(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::SessionDiff,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push session context usage.
    pub fn push_session_context(&self, session_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::SessionContext,
            id: Some(session_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push git log for a project.
    pub fn push_git_log(&self, project_id: &str, data: Value) {
        self.push(StateChange {
            kind: StateKind::GitLog,
            id: Some(project_id.to_owned()),
            data,
            cursor: None,
        });
    }

    /// Convenience: push notification channel list.
    pub fn push_notification_channels(&self, data: Value) {
        self.push(StateChange {
            kind: StateKind::NotificationChannels,
            id: None,
            data,
            cursor: None,
        });
    }
}

impl Default for StateBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}
