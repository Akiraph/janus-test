//! SSE endpoint — real-time state push with cursor-resumable replay.
//!
//! Protocol:
//!   event: hello       — protocol version + current log end
//!   event: state       — full projection of a single changed resource
//!   event: snapshot    — full current state on connect (per authenticated user)
//!   event: ping        — heartbeat
//!
//! The client connects with `?after=<cursor>` (or `Last-Event-ID: <cursor>`,
//! which the browser EventSource sends automatically from the last `id:` it
//! saw). The server first replays every committed event after that cursor by
//! re-projecting it for the authenticated owner — same `event: state` frames
//! the live path emits, each carrying its event cursor as the SSE `id:` — then
//! sends a full snapshot to heal anything the replay could not cover (events
//! with no client-side projection). A conflicting `?after` and `Last-Event-ID`
//! is rejected, as is a cursor ahead of the committed log.

use std::time::Duration;

use axum::{
    Extension,
    extract::{Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::Stream;
use janus_infrastructure::state_broadcaster::StateKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{
    AppState,
    transport::http::{
        auth::authenticate, dto::BootstrapState, problem::Problem, request_id::RequestContext,
    },
};

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Client's last-seen event cursor. When absent, `Last-Event-ID` is used;
    /// when both are present they must agree.
    after: Option<String>,
}

#[derive(Serialize)]
struct StateFrame {
    kind: StateKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

#[derive(Serialize)]
struct SnapshotFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_timeline: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queued_turns: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_status: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_info: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bootstrap: Option<serde_json::Value>,
}

async fn build_snapshot(state: &AppState, owner_id: &str) -> SnapshotFrame {
    // Bootstrap
    let bootstrap = match state.identity().initialization_state().await {
        Ok(init_state) => {
            let bootstrap_state = match init_state {
                janus_identity::InitializationState::Uninitialized => BootstrapState::Uninitialized,
                janus_identity::InitializationState::Initialized => BootstrapState::Initialized,
            };
            Some(json!({
                "data": {
                    "state": bootstrap_state,
                    "development_auth": state.config().development_auth,
                    "webauthn_rp_name": state.config().webauthn_rp_name,
                    "version": env!("CARGO_PKG_VERSION"),
                    "limits": {
                        "max_file_bytes": janus_sessions::interface::MAX_ATTACHMENT_BYTES,
                        "max_message_bytes": janus_sessions::interface::MAX_MESSAGE_BYTES,
                        "max_attachments": janus_sessions::interface::MAX_ATTACHMENTS,
                    },
                }
            }))
        }
        Err(_) => None,
    };

    // System info
    let system_info = match state.system().events_bounds().await {
        Ok(bounds) => {
            let schema_version = state.system().schema_version().await.unwrap_or(0);
            Some(json!({
                "data": {
                    "version": env!("CARGO_PKG_VERSION"),
                    "schema_version": schema_version,
                    "mode": state.config().mode.as_str(),
                    "database": {
                        "engine": "mongodb",
                        "journal_mode": "on",
                        "ready": state.system().ready().await,
                    },
                    "events": {
                        "min_cursor": bounds.min.to_string(),
                        "max_cursor": bounds.max.to_string(),
                    },
                }
            }))
        }
        Err(_) => None,
    };

    // Projects
    let projects = match state.projects().list_projects(owner_id, 100).await {
        Ok(list) => Some(serde_json::to_value(&list).unwrap_or_default()),
        Err(_) => None,
    };

    // Providers
    let providers = match state.models().providers(owner_id).await {
        Ok(list) => Some(serde_json::to_value(&list).unwrap_or_default()),
        Err(_) => None,
    };

    SnapshotFrame {
        session: None,
        session_timeline: None,
        session_context: None,
        turn: None,
        queued_turns: None,
        project: None,
        projects,
        git_status: None,
        sessions: None,
        providers,
        system_info,
        bootstrap,
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/events",
    params(("after" = Option<String>, Query, description = "Resume cursor; falls back to Last-Event-ID")),
    responses(
        (status = 200, description = "Janus state push stream", content_type = "text/event-stream"),
        (status = 400, body = Problem, description = "Cursor mismatch or ahead of log"),
    )
)]
pub async fn events(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    headers: axum::http::HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, Problem> {
    let auth = authenticate(&state, &headers).await?;
    let owner_id = auth.owner_id.clone();

    // Resolve the resume cursor. `?after` and `Last-Event-ID` are two spellings
    // of the same position and must not disagree; neither present means the
    // client is starting fresh and replays the whole log.
    let query_cursor = query.after.as_deref();
    let header_cursor = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok());
    let cursor = match (query_cursor, header_cursor) {
        (Some(query), Some(header)) if query != header => {
            return Err(Problem::new(
                StatusCode::BAD_REQUEST,
                "CURSOR_MISMATCH",
                "Invalid cursor",
                "`after` query parameter and Last-Event-ID header disagree",
            ));
        }
        (Some(value), _) | (_, Some(value)) => value.parse::<u64>().map_err(|_| {
            Problem::new(
                StatusCode::BAD_REQUEST,
                "VALIDATION_FAILED",
                "Invalid cursor",
                "event cursor must be an integer",
            )
        })?,
        (None, None) => 0,
    };

    let bounds = state.system().events_bounds().await.map_err(|error| {
        Problem::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Internal server error",
            error.to_string(),
        )
    })?;
    if cursor > bounds.max {
        return Err(Problem::new(
            StatusCode::BAD_REQUEST,
            "EVENT_CURSOR_AHEAD",
            "Cursor ahead",
            "cursor is ahead of the committed event log",
        ));
    }
    let max_cursor = bounds.max;

    let mut receiver = state.system().subscribe();

    let heartbeat = Duration::from_secs(15);
    let stream = async_stream::stream! {
        // 1. Hello with the current log end so the client can pick a resume cursor.
        if let Ok(event) = Event::default()
            .event("hello")
            .json_data(json!({"protocol": 2, "max_cursor": max_cursor.to_string()}))
        {
            yield Ok(event);
        } else {
            return;
        }

        // 2. Replay: project every committed event after the client's cursor for
        // this owner. Same `event: state` frames as the live path, each carrying
        // its event cursor as the SSE `id:` so EventSource resumes from the last
        // one it applied. Capped so a long history cannot delay the snapshot
        // indefinitely; the snapshot heals whatever the cap drops.
        const REPLAY_CAP: usize = 2000;
        let mut replay_cursor = cursor;
        let mut replayed = 0usize;
        // `model.stream_delta` events are not projected by the engine (the
        // live path pushes accumulated text directly), so a client that
        // reconnects mid-stream would otherwise lose every token streamed
        // while it was down. Accumulate the deltas per (session, turn) during
        // the replay and emit one stream_text frame per stream, exactly like
        // the live push shape the client already understands.
        let mut replayed_streams: std::collections::HashMap<(String, String), Value> =
            std::collections::HashMap::new();
        loop {
            let events = match state.system().events_after(replay_cursor, 100).await {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(request_id = %context.request_id, %error, replay_cursor, "event replay scan failed");
                    return;
                }
            };
            if events.is_empty() {
                break;
            }
            for event in events {
                replay_cursor = event.cursor.parse().unwrap_or(replay_cursor);
                if event.event_type == "model.stream_delta" {
                    let Some(session_id) = event.payload.get("session_id").and_then(Value::as_str) else {
                        replayed += 1;
                        continue;
                    };
                    let Some(turn_id) = event.payload.get("turn_id").and_then(Value::as_str) else {
                        replayed += 1;
                        continue;
                    };
                    let Some(round_id) = event.payload.get("round_id").and_then(Value::as_str) else {
                        replayed += 1;
                        continue;
                    };
                    let channel = event.payload.get("channel").and_then(Value::as_str).unwrap_or("");
                    let delta = event.payload.get("delta").and_then(Value::as_str).unwrap_or("");
                    let entry = replayed_streams
                        .entry((session_id.to_owned(), turn_id.to_owned()))
                        .or_insert_with(|| {
                            json!({
                                "text": "",
                                "reasoning": "",
                                "seq": 0,
                                "round_id": round_id,
                            })
                        });
                    match channel {
                        "text" => {
                            if let Value::String(text) = entry.get_mut("text").unwrap_or(&mut Value::Null) {
                                text.push_str(delta);
                            }
                        }
                        "reasoning_summary" => {
                            if let Value::String(reasoning) =
                                entry.get_mut("reasoning").unwrap_or(&mut Value::Null)
                            {
                                reasoning.push_str(delta);
                            }
                        }
                        _ => {}
                    }
                    if let Some(seq) = entry.get_mut("seq") {
                        *seq = json!(replayed);
                    }
                    replayed += 1;
                    continue;
                }
                let changes = state.system().project(Some(&owner_id), &event).await;
                for change in changes {
                    let frame = StateFrame {
                        kind: change.kind,
                        id: change.id,
                        data: change.data,
                        cursor: change.cursor.clone(),
                    };
                    let event = Event::default().event("state");
                    let event = if let Some(cursor) = change.cursor {
                        event.id(cursor)
                    } else {
                        event
                    };
                    if let Ok(event) = event.json_data(&frame) {
                        yield Ok(event);
                    } else {
                        return;
                    }
                }
                replayed += 1;
                if replayed >= REPLAY_CAP {
                    tracing::info!(request_id = %context.request_id, replay_cursor, "event replay capped, snapshot heals the rest");
                    break;
                }
            }
            if replayed >= REPLAY_CAP {
                break;
            }
        }
        // Emit the reconstructed live streams as stream_text frames. The
        // cursor is absent by design: this is transient overlay state, not a
        // durable projection, and the client treats it accordingly.
        for ((session_id, turn_id), data) in replayed_streams {
            let frame = StateFrame {
                kind: StateKind::StreamText,
                id: Some(format!("{session_id}:{turn_id}")),
                data,
                cursor: None,
            };
            if let Ok(event) = Event::default().event("state").json_data(&frame) {
                yield Ok(event);
            }
        }

        // 3. Snapshot with current state for this user. Its SSE `id:` is the true
        // log end so a subsequent reconnect resumes from there rather than
        // re-replaying what the snapshot already covered.
        let snapshot_cursor = match state.system().events_bounds().await {
            Ok(bounds) => bounds.max,
            Err(_) => replay_cursor,
        };
        let snapshot = build_snapshot(&state, &owner_id).await;
        if let Ok(event) = Event::default()
            .event("snapshot")
            .id(snapshot_cursor.to_string())
            .json_data(&snapshot)
        {
            yield Ok(event);
        } else {
            return;
        }

        // 4. Loop: state changes or heartbeat.
        loop {
            tokio::select! {
                result = receiver.recv() => {
                    match result {
                        Ok(change) => {
                            let frame = StateFrame {
                                kind: change.kind,
                                id: change.id.clone(),
                                data: change.data.clone(),
                                cursor: change.cursor.clone(),
                            };
                            let event = Event::default().event("state");
                            let event = if let Some(cursor) = change.cursor.clone() {
                                event.id(cursor)
                            } else {
                                event
                            };
                            if let Ok(event) = event.json_data(&frame) {
                                yield Ok(event);
                            } else {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // Dropping the connection here was the root cause
                            // of "must refresh to see updates": the browser
                            // auto-reconnects with Last-Event-ID, replays
                            // events, and the snapshot then heals whatever the
                            // replay could not cover. A lag burst is now a
                            // resumable hiccup instead of a dead stream.
                            tracing::warn!(request_id = %context.request_id, skipped, "state broadcast consumer lagged, healing with snapshot");
                            let heal_cursor = state.system().events_bounds().await.map(|b| b.max).unwrap_or_default();
                            let snapshot = build_snapshot(&state, &owner_id).await;
                            if let Ok(event) = Event::default()
                                .event("snapshot")
                                .id(heal_cursor.to_string())
                                .json_data(&snapshot)
                            {
                                yield Ok(event);
                            } else {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
                () = tokio::time::sleep(heartbeat) => {
                    yield Ok(Event::default().event("ping").data(""));
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    ))
}
