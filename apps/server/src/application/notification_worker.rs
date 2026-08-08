//! Best-effort delivery of selected committed public events.
//!
//! EventStore remains an observation log, not an internal command bus. This
//! worker polls it with an explicit cursor and translates only notification
//! policy events into calls to the notifications capability.

use janus_infrastructure::{
    events::EventEnvelope,
    id::{SessionId, TurnId},
};
use janus_notifications::interface::{NotificationEvent, NotificationEventKind};
use serde_json::{Value, json};
use tokio::time::Duration;
use tracing::{info, warn};

use super::Application;

pub fn spawn(state: Application) {
    tokio::spawn(async move {
        let mut cursor = match state.events().bounds().await {
            Ok(bounds) => bounds.max,
            Err(error) => {
                warn!(%error, "notification worker could not read initial event cursor");
                0
            }
        };
        let mut wake = state.events().subscribe();
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        info!(cursor, "janus notification worker started");
        loop {
            tokio::select! {
                _ = wake.recv() => {}
                _ = ticker.tick() => {}
            }
            loop {
                let events = match state.events().after(cursor, 100).await {
                    Ok(events) => events,
                    Err(error) => {
                        warn!(%error, cursor, "notification event scan failed");
                        break;
                    }
                };
                if events.is_empty() {
                    break;
                }
                for event in events {
                    cursor = event.cursor.parse().unwrap_or(cursor);
                    if let Some((owner_id, notification)) = translate(&state, &event).await {
                        if let Err(error) = state
                            .notifications()
                            .dispatch(&owner_id, &notification)
                            .await
                        {
                            warn!(%error, event_type = %event.event_type, cursor, "notification delivery failed");
                        }
                    }
                }
            }
        }
    });
}

async fn translate(
    state: &Application,
    event: &EventEnvelope,
) -> Option<(String, NotificationEvent)> {
    let (kind, title, message) = match event.event_type.as_str() {
        "turn.status_changed" => match event.payload.get("to").and_then(Value::as_str) {
            Some("completed") => (
                NotificationEventKind::TurnCompleted,
                "Turn completed",
                "The model turn completed.",
            ),
            Some("failed") | Some("interrupted") => (
                NotificationEventKind::TurnFailed,
                "Turn failed",
                "The model turn needs attention.",
            ),
            Some("waiting_for_model") => (
                NotificationEventKind::ModelWaiting,
                "Model needs attention",
                "The model turn is waiting for a model retry or configuration change.",
            ),
            _ => return None,
        },
        "ask.changed" if event.payload.get("status").and_then(Value::as_str) == Some("open") => (
            NotificationEventKind::AskOpened,
            "Janus is waiting for your answer",
            "The model asked a question in a session.",
        ),
        "job.changed" => match event.payload.get("status").and_then(Value::as_str) {
            Some("succeeded") => (
                NotificationEventKind::JobCompleted,
                "Async job completed",
                "A background bash or CLI job completed successfully.",
            ),
            Some("failed") | Some("canceled") | Some("lost") => (
                NotificationEventKind::JobCompleted,
                "Async job finished",
                "A background bash or CLI job finished without success.",
            ),
            _ => return None,
        },
        _ => return None,
    };
    let session_id = event_session_id(state, event).await?;
    let session = state.sessions().get_session(session_id).await.ok()?;
    let project_id = session.project_id.parse().ok()?;
    let owner_id = state.projects().owner_id(project_id).await.ok()?;
    Some((
        owner_id,
        NotificationEvent {
            kind,
            title: title.into(),
            message: message.into(),
            data: json!({
                "event_type": event.event_type,
                "event_id": event.event_id,
                "cursor": event.cursor,
                "session_id": session_id,
                "payload": event.payload,
            }),
        },
    ))
}

async fn event_session_id(state: &Application, event: &EventEnvelope) -> Option<SessionId> {
    let raw = event
        .payload
        .get("session_id")
        .and_then(Value::as_str)
        .or_else(|| event.payload.get("sessionId").and_then(Value::as_str));
    if let Some(raw) = raw {
        if let Ok(session_id) = raw.parse() {
            return Some(session_id);
        }
    }
    let turn_id = event
        .payload
        .get("turn_id")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .resource
                .as_ref()
                .and_then(|resource| resource.get("id"))
                .and_then(Value::as_str)
        })?
        .parse::<TurnId>()
        .ok()?;
    state.sessions().session_id_for_turn(turn_id).await.ok()
}
