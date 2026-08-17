//! Universal projection engine.
//!
//! The projection engine is the single source of real-time state on the SSE
//! channel. Every public event is projected (exhaustively matched) to the
//! complete current projections it affects, and those projections are
//! broadcast so clients can apply them via `setQueryData` without polling or
//! invalidation.
//!
//! The engine only reads committed state: EventStore notifies subscribers
//! after the unit of work commits, so each broadcast carries the
//! authoritative post-transition projection. It intentionally does NOT
//! project `model.stream_delta` / `model.attempt_retrying`; those are
//! consumed by the direct stream-text path. Terminal events also stay off
//! this channel, while durable operation, runtime, and async-task events are
//! projected to their corresponding query caches.
//!
//! The processed cursor is persisted after every batch, so a restart resumes
//! from exactly where the engine stopped and reprojects the events that were
//! committed while it was down (projections are idempotent reads, so
//! reprojection converges).

use janus_infrastructure::{
    events::{EventEnvelope, EventType},
    id::{ProjectId, SessionId, TerminalId, TurnId},
    state_broadcaster::{StateChange, StateKind},
};
use serde_json::Value;
use tokio::time::Duration;
use tracing::{info, warn};

use super::Application;

pub fn spawn(state: Application) {
    tokio::spawn(async move {
        let mut cursor = match state.events().projection_cursor().await {
            Ok(cursor) => cursor,
            Err(error) => {
                warn!(%error, "projection engine could not read persisted cursor");
                match state.events().bounds().await {
                    Ok(bounds) => bounds.max,
                    Err(error) => {
                        warn!(%error, "projection engine could not read event bounds");
                        0
                    }
                }
            }
        };
        let mut wake = state.events().subscribe();
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        info!(cursor, "janus projection engine started");
        loop {
            tokio::select! {
                _ = wake.recv() => {}
                _ = ticker.tick() => {}
            }
            loop {
                let events = match state.events().after(cursor, 100).await {
                    Ok(events) => events,
                    Err(error) => {
                        warn!(%error, cursor, "projection engine scan failed");
                        break;
                    }
                };
                if events.is_empty() {
                    break;
                }
                for event in events {
                    cursor = event.cursor.parse().unwrap_or(cursor);
                    for change in project_event(&state, None, &event).await {
                        state.state_broadcaster().push(change);
                    }
                }
                if let Err(error) = state.events().set_projection_cursor(cursor).await {
                    warn!(%error, cursor, "projection engine could not persist cursor");
                }
            }
        }
    });
}

/// Project a single committed event to state frames. The match is exhaustive
/// over `EventType`, so a newly added event type fails to compile until
/// someone decides how it projects.
///
/// `owner` overrides the actor-derived owner for owner-scoped projections
/// (project / projects / providers / channels). The engine passes `None` and
/// derives it from the event actor; the SSE replay path passes the
/// authenticated owner.
pub async fn project_event(
    state: &Application,
    owner: Option<&str>,
    event: &EventEnvelope,
) -> Vec<StateChange> {
    let cursor = event.cursor.clone();
    let event_type = match event.event_type.parse::<EventType>() {
        Ok(event_type) => event_type,
        Err(_) => {
            warn!(event_type = %event.event_type, "projection engine: unknown event type");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    use EventType::*;
    match event_type {
        // Turn lifecycle: the session, timeline, queued list, turn summary and
        // (for status transitions) context usage all change together.
        TurnCreated | TurnStatusChanged => {
            let Some((session_id, turn_id)) = event_session_turn(state, event).await else {
                return out;
            };
            push_session(&mut out, state, session_id, Some(&cursor)).await;
            push_timeline(&mut out, state, session_id, Some(&cursor)).await;
            push_queued(&mut out, state, session_id, Some(&cursor)).await;
            push_turn(&mut out, state, session_id, turn_id, Some(&cursor)).await;
            if matches!(event_type, TurnStatusChanged) {
                push_context(&mut out, state, session_id, Some(&cursor)).await;
            }
        }

        SessionChanged => {
            let Some(session_id) = event_session_id(event) else {
                return out;
            };
            push_session(&mut out, state, session_id, Some(&cursor)).await;
            push_sessions_for_session(&mut out, state, session_id, Some(&cursor)).await;
        }

        SessionDeleted => {
            let Some(project_id) = event.payload.get("project_id").and_then(Value::as_str) else {
                return out;
            };
            push_sessions(&mut out, state, project_id, Some(&cursor)).await;
        }

        ContextChanged => {
            let Some(session_id) = event_session_id(event) else {
                return out;
            };
            push_context(&mut out, state, session_id, Some(&cursor)).await;
        }

        TimelineItemCreated | TimelineItemUpdated | ToolCallCreated => {
            let Some(session_id) = event_session_id(event) else {
                return out;
            };
            push_timeline(&mut out, state, session_id, Some(&cursor)).await;
            push_context(&mut out, state, session_id, Some(&cursor)).await;
        }

        ToolCallChanged => {
            let Some(session_id) = resolve_session_id(state, event).await else {
                return out;
            };
            push_timeline(&mut out, state, session_id, Some(&cursor)).await;
            if let Some(turn_id) = event_turn_id(event) {
                push_turn(&mut out, state, session_id, turn_id, Some(&cursor)).await;
            }
        }

        RoundChanged => {
            let Some(session_id) = resolve_session_id(state, event).await else {
                return out;
            };
            if let Some(turn_id) = event_turn_id(event) {
                push_turn(&mut out, state, session_id, turn_id, Some(&cursor)).await;
            }
            push_context(&mut out, state, session_id, Some(&cursor)).await;
        }

        CheckpointCreated => {
            let Some(session_id) = event_session_id(event) else {
                return out;
            };
            push_session(&mut out, state, session_id, Some(&cursor)).await;
            push_timeline(&mut out, state, session_id, Some(&cursor)).await;
        }

        ProjectChanged | ProjectMainRevisionChanged => {
            let Some((actor_owner, project_id)) = event_owner_project(event) else {
                return out;
            };
            let owner_id = owner.map(str::to_owned).unwrap_or(actor_owner);
            push_project_and_list(&mut out, state, &owner_id, &project_id, Some(&cursor)).await;
        }

        GitStateChanged => {
            let Some((actor_owner, project_id)) = event_owner_project(event) else {
                return out;
            };
            let owner_id = owner.map(str::to_owned).unwrap_or(actor_owner);
            push_git(&mut out, state, &owner_id, &project_id, Some(&cursor)).await;
        }

        AsyncTaskChanged => {
            let Some(session_id) = event_session_id(event) else {
                return out;
            };
            push_async_tasks(&mut out, state, Some(&cursor)).await;
            push_timeline(&mut out, state, session_id, Some(&cursor)).await;
            if let Some(async_task_id) = event_resource_id(event, "async_task")
                && let Ok(async_task_id) = async_task_id.parse()
                && let Ok(async_task) = state.runtime().async_task(async_task_id).await
            {
                push_turn(
                    &mut out,
                    state,
                    session_id,
                    async_task.controlling_turn_id,
                    Some(&cursor),
                )
                .await;
            }
        }

        TerminalChanged => {
            let Some(terminal_id) = event_resource_id(event, "terminal") else {
                return out;
            };
            push_terminal(&mut out, state, &terminal_id, Some(&cursor)).await;
            if let Some(project_id) = event_project_id(event) {
                push_terminals(&mut out, state, &project_id, Some(&cursor)).await;
            }
        }

        RuntimeChanged => {
            if let Some(session_id) = event_session_id(event) {
                push_session(&mut out, state, session_id, Some(&cursor)).await;
            }
        }

        OperationChanged => {
            if let Some(operation_id) = event_resource_id(event, "operation").or_else(|| {
                event
                    .payload
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }) {
                push_operation(&mut out, state, &operation_id, Some(&cursor)).await;
            }
        }

        NotificationChannelChanged => {
            let Some(actor_owner) = event_owner_id(event) else {
                return out;
            };
            let owner_id = owner.map(str::to_owned).unwrap_or(actor_owner);
            push_channels(&mut out, state, &owner_id, Some(&cursor)).await;
        }

        ModelConfigChanged => {
            let Some(actor_owner) = event_owner_id(event) else {
                return out;
            };
            let owner_id = owner.map(str::to_owned).unwrap_or(actor_owner);
            push_providers(&mut out, state, &owner_id, Some(&cursor)).await;
        }

        // Consumed by the direct stream-text path; projecting stream text here
        // would duplicate live output. Retry state is durable, however, so the
        // Turn projection must move with its retry event.
        ModelStreamDelta => {}

        ModelAttemptRetrying => {
            let Some((session_id, turn_id)) = event_session_turn(state, event).await else {
                return out;
            };
            push_turn(&mut out, state, session_id, turn_id, Some(&cursor)).await;
        }

        SystemStarted => {}
    }
    out
}

// ----- projection helpers: read the current projection, emit a frame -----

async fn push_session(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    cursor: Option<&str>,
) {
    if let Ok(session) = state.sessions().get_session(session_id).await {
        out.push(StateChange {
            kind: StateKind::Session,
            id: Some(session_id.to_string()),
            data: serde_json::to_value(&session).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_timeline(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    cursor: Option<&str>,
) {
    if let Ok(timeline) = state.sessions().timeline(session_id, None, None, 100).await {
        out.push(StateChange {
            kind: StateKind::SessionTimeline,
            id: Some(session_id.to_string()),
            data: serde_json::to_value(&timeline).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_queued(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    cursor: Option<&str>,
) {
    if let Ok(queued) = state.sessions().queued_turns(session_id).await {
        out.push(StateChange {
            kind: StateKind::QueuedTurns,
            id: Some(session_id.to_string()),
            data: serde_json::to_value(&queued).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

/// Same projection the GET /sessions/{sid}/turns/{tid} handler returns:
/// `get_turn` enriched with the latest model attempt.
async fn push_turn(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    turn_id: TurnId,
    cursor: Option<&str>,
) {
    let Ok(turn) = state.turn_summary(session_id, turn_id).await else {
        return;
    };
    out.push(StateChange {
        kind: StateKind::Turn,
        id: Some(format!("{session_id}_{turn_id}")),
        data: serde_json::to_value(&turn).unwrap_or_default(),
        cursor: cursor.map(str::to_owned),
    });
}

async fn push_context(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    cursor: Option<&str>,
) {
    let Ok(Some(usage)) = state.session_context_usage(session_id).await else {
        return;
    };
    out.push(StateChange {
        kind: StateKind::SessionContext,
        id: Some(session_id.to_string()),
        data: serde_json::to_value(&usage).unwrap_or_default(),
        cursor: cursor.map(str::to_owned),
    });
}

async fn push_sessions_for_session(
    out: &mut Vec<StateChange>,
    state: &Application,
    session_id: SessionId,
    cursor: Option<&str>,
) {
    let Ok(session) = state.sessions().get_session(session_id).await else {
        return;
    };
    push_sessions(out, state, &session.project_id, cursor).await;
}

async fn push_sessions(
    out: &mut Vec<StateChange>,
    state: &Application,
    project_id: &str,
    cursor: Option<&str>,
) {
    let Ok(project_id) = project_id.parse::<ProjectId>() else {
        return;
    };
    if let Ok(sessions) = state.sessions().list_sessions(project_id, 100).await {
        out.push(StateChange {
            kind: StateKind::Sessions,
            id: Some(project_id.to_string()),
            data: serde_json::to_value(&sessions).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_project_and_list(
    out: &mut Vec<StateChange>,
    state: &Application,
    owner_id: &str,
    project_id: &str,
    cursor: Option<&str>,
) {
    if let Ok(view) = state.projects().get_project(owner_id, project_id).await {
        out.push(StateChange {
            kind: StateKind::Project,
            id: Some(project_id.to_owned()),
            data: serde_json::to_value(&view).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
    if let Ok(list) = state.projects().list_projects(owner_id, 100).await {
        out.push(StateChange {
            kind: StateKind::Projects,
            id: None,
            data: serde_json::to_value(&list).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_git(
    out: &mut Vec<StateChange>,
    state: &Application,
    owner_id: &str,
    project_id: &str,
    cursor: Option<&str>,
) {
    if let Ok(status) = state
        .source_control()
        .git_status(owner_id, project_id)
        .await
    {
        out.push(StateChange {
            kind: StateKind::GitStatus,
            id: Some(project_id.to_owned()),
            data: serde_json::to_value(&status).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
    if let Ok(entries) = state
        .source_control()
        .git_log(owner_id, project_id, 50)
        .await
    {
        out.push(StateChange {
            kind: StateKind::GitLog,
            id: Some(project_id.to_owned()),
            data: serde_json::to_value(&entries).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_async_tasks(out: &mut Vec<StateChange>, state: &Application, cursor: Option<&str>) {
    if let Ok(tasks) = state.runtime().async_tasks(200).await {
        out.push(StateChange {
            kind: StateKind::AsyncTasks,
            id: None,
            data: serde_json::to_value(&tasks).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_operation(
    out: &mut Vec<StateChange>,
    state: &Application,
    operation_id: &str,
    cursor: Option<&str>,
) {
    if let Ok(Some(operation)) = state.operations().get(operation_id).await {
        out.push(StateChange {
            kind: StateKind::Operation,
            id: Some(operation_id.to_owned()),
            data: serde_json::to_value(operation).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_terminal(
    out: &mut Vec<StateChange>,
    state: &Application,
    terminal_id: &str,
    cursor: Option<&str>,
) {
    let Ok(terminal_id) = terminal_id.parse::<TerminalId>() else {
        return;
    };
    if let Ok(terminal) = state.runtime().terminal(terminal_id).await {
        out.push(StateChange {
            kind: StateKind::Terminal,
            id: Some(terminal_id.to_string()),
            data: serde_json::to_value(terminal).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_terminals(
    out: &mut Vec<StateChange>,
    state: &Application,
    project_id: &str,
    cursor: Option<&str>,
) {
    let Ok(project_id) = project_id.parse::<ProjectId>() else {
        return;
    };
    if let Ok(terminals) = state.runtime().list_terminals(project_id).await {
        out.push(StateChange {
            kind: StateKind::Terminals,
            id: Some(project_id.to_string()),
            data: serde_json::to_value(terminals).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_providers(
    out: &mut Vec<StateChange>,
    state: &Application,
    owner_id: &str,
    cursor: Option<&str>,
) {
    if let Ok(list) = state.models().providers(owner_id).await {
        out.push(StateChange {
            kind: StateKind::Providers,
            id: None,
            data: serde_json::to_value(&list).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

async fn push_channels(
    out: &mut Vec<StateChange>,
    state: &Application,
    owner_id: &str,
    cursor: Option<&str>,
) {
    if let Ok(list) = state.notifications().channels(owner_id).await {
        out.push(StateChange {
            kind: StateKind::NotificationChannels,
            id: None,
            data: serde_json::to_value(&list).unwrap_or_default(),
            cursor: cursor.map(str::to_owned),
        });
    }
}

// ----- event shape extraction -----

fn event_session_id(event: &EventEnvelope) -> Option<SessionId> {
    if let Some(raw) = event.payload.get("session_id").and_then(Value::as_str) {
        return raw.parse().ok();
    }
    if let Some(resource) = event.resource.as_ref()
        && resource.get("kind").and_then(Value::as_str) == Some("session")
        && let Some(id) = resource.get("id").and_then(Value::as_str)
    {
        return id.parse().ok();
    }
    None
}

fn event_turn_id(event: &EventEnvelope) -> Option<TurnId> {
    if let Some(raw) = event.payload.get("turn_id").and_then(Value::as_str) {
        return raw.parse().ok();
    }
    if let Some(resource) = event.resource.as_ref()
        && resource.get("kind").and_then(Value::as_str) == Some("turn")
        && let Some(id) = resource.get("id").and_then(Value::as_str)
    {
        return id.parse().ok();
    }
    None
}

fn event_resource_id(event: &EventEnvelope, kind: &str) -> Option<String> {
    if let Some(resource) = event.resource.as_ref()
        && resource.get("kind").and_then(Value::as_str) == Some(kind)
    {
        return resource
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    event
        .payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Session id for an event, resolving through the turn when the event only
/// carries a turn id (e.g. tool-call/round changes).
async fn resolve_session_id(state: &Application, event: &EventEnvelope) -> Option<SessionId> {
    if let Some(session_id) = event_session_id(event) {
        return Some(session_id);
    }
    let turn_id = event_turn_id(event)?;
    state.sessions().session_id_for_turn(turn_id).await.ok()
}

/// session_id + turn_id for turn-scoped events, resolving through the turn.
async fn event_session_turn(
    state: &Application,
    event: &EventEnvelope,
) -> Option<(SessionId, TurnId)> {
    let turn_id = event_turn_id(event)?;
    let session_id = if let Some(raw) = event.payload.get("session_id").and_then(Value::as_str) {
        raw.parse::<SessionId>().ok()?
    } else {
        state.sessions().session_id_for_turn(turn_id).await.ok()?
    };
    Some((session_id, turn_id))
}

fn event_owner_id(event: &EventEnvelope) -> Option<String> {
    event
        .actor
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn event_project_id(event: &EventEnvelope) -> Option<String> {
    if let Some(raw) = event.payload.get("project_id").and_then(Value::as_str) {
        return Some(raw.to_owned());
    }
    if let Some(resource) = event.resource.as_ref()
        && resource.get("kind").and_then(Value::as_str) == Some("project")
        && let Some(id) = resource.get("id").and_then(Value::as_str)
    {
        return Some(id.to_owned());
    }
    None
}

fn event_owner_project(event: &EventEnvelope) -> Option<(String, String)> {
    Some((event_owner_id(event)?, event_project_id(event)?))
}
