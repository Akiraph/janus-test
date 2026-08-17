//! Cross-capability context compaction workflow.
//!
//! The execution capability owns context versions and compact summaries while
//! Sessions owns the visible timeline item. The application layer keeps their
//! state and public events in the same transaction and runs the command through
//! the durable operation worker.

use chrono::Duration;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Application;
use super::operation_kinds::KIND_CONTEXT_COMPACT;
use janus_execution::interface::{
    DEFAULT_CONTEXT_LIMIT, ScheduleCompactInput, context_usage_near_limit,
};
use janus_infrastructure::clock::{format_utc, now_utc, now_utc_str};
use janus_infrastructure::events::{EventType, NewEvent};
use janus_infrastructure::id::{CompactSummaryId, CorrelationId, SessionId};
use janus_infrastructure::operations::{
    CreateOperation, CreateWork, IdempotencyOutcome, IdempotencyRequest, OperationStatus,
    OperationView, StepState, WorkClaim,
};
use janus_sessions::interface::{ContextCompactedTimelineInput, SessionsError};

pub(crate) struct CompactContextRequest {
    pub(crate) owner_id: String,
    pub(crate) session_id: SessionId,
    pub(crate) actor: Value,
    pub(crate) correlation_id: CorrelationId,
    pub(crate) idempotency: IdempotencyRequest,
    pub(crate) context_limit: Option<i64>,
}

impl Application {
    pub(crate) async fn auto_compact_idle_sessions(&self) -> Result<(), SessionsError> {
        for session_id in self.sessions().ready_session_ids().await? {
            let Some(usage) = self
                .execution()
                .latest_context_usage(session_id)
                .await
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?
            else {
                continue;
            };
            if matches!(usage.compact_status.as_str(), "scheduled" | "running")
                || !context_usage_near_limit(usage.estimated_input_tokens, usage.context_limit)
            {
                continue;
            }

            let request = CompactContextRequest {
                owner_id: "system".to_owned(),
                session_id,
                actor: json!({"kind": "system", "reason": "context_threshold"}),
                correlation_id: CorrelationId::new(),
                idempotency: IdempotencyRequest {
                    key: format!("auto-context-compact:{session_id}:{}", usage.created_at),
                    owner_id: "system".to_owned(),
                    method: "AUTO".to_owned(),
                    normalized_route: format!("/internal/sessions/{session_id}/context/compact"),
                    digest: "auto-context-compact".to_owned(),
                    expires_at: format_utc(now_utc() + Duration::hours(24)),
                },
                context_limit: Some(usage.context_limit),
            };
            match self.request_context_compact(request).await {
                Ok(_) => {}
                Err(
                    SessionsError::ActiveTurnExists
                    | SessionsError::SessionDeleting
                    | SessionsError::Validation(_),
                ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(crate) async fn request_context_compact(
        &self,
        input: CompactContextRequest,
    ) -> Result<OperationView, SessionsError> {
        let (source_first, source_last, item_count) =
            self.sessions().timeline_bounds(input.session_id).await?;
        let source_last = source_last.unwrap_or_else(|| "0".to_owned());
        let timeline = self
            .sessions()
            .timeline(input.session_id, None, None, 10_000)
            .await?;
        let timeline_digest = compact_timeline_digest(&timeline.items);
        let summary = json!({
            "kind": "manual_compact",
            "title": "Context Compacted",
            "source_first_timeline_id": source_first,
            "source_last_timeline_id": source_last,
            "item_count": item_count,
            "timeline_digest": timeline_digest,
            "message": "Prior timeline context is represented by this durable compact summary.",
        });
        let compact_summary_id = CompactSummaryId::new().to_string();
        let session_id = input.session_id.to_string();
        let payload = json!({
            "session_id": session_id,
            "compact_summary_id": compact_summary_id,
            "source_first_timeline_id": source_first.clone(),
            "source_last_timeline_id": source_last.clone(),
            "summary": summary.clone(),
            "actor": input.actor.clone(),
        });

        let mut work = self.unit_of_work().begin().await?;
        let created = self
            .operations()
            .create_in_tx(
                &mut work,
                CreateOperation {
                    kind: KIND_CONTEXT_COMPACT,
                    actor: payload
                        .get("actor")
                        .cloned()
                        .unwrap_or_else(|| json!({"kind": "owner", "id": input.owner_id})),
                    target_kind: "session",
                    target_id: Some(&session_id),
                    conditions: json!({
                        "session_id": session_id,
                        "compact_summary_id": compact_summary_id,
                    }),
                    correlation_id: input.correlation_id,
                    idempotency: Some(input.idempotency),
                },
                Some(CreateWork {
                    handler_kind: KIND_CONTEXT_COMPACT,
                    payload: payload.clone(),
                }),
            )
            .await
            .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;

        if matches!(created.outcome, IdempotencyOutcome::New) {
            let current_session = self.sessions().get_session(input.session_id).await?;
            if current_session.state == "deleting" {
                return Err(SessionsError::SessionDeleting);
            }
            if current_session.active_turn_id.is_some() || current_session.state == "active" {
                return Err(SessionsError::ActiveTurnExists);
            }
            if self
                .execution()
                .context_compact_in_progress_in_tx(work.connection(), input.session_id)
                .await
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?
            {
                return Err(SessionsError::Validation(
                    "context compact is already in progress".into(),
                ));
            }
            let context_limit = if let Some(context_limit) = input
                .context_limit
                .filter(|context_limit| *context_limit > 0)
            {
                context_limit
            } else {
                self.execution()
                    .latest_context_usage(input.session_id)
                    .await
                    .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?
                    .map(|usage| usage.context_limit)
                    .filter(|context_limit| *context_limit > 0)
                    .unwrap_or(DEFAULT_CONTEXT_LIMIT)
            };
            self.execution()
                .schedule_context_compact_in_tx(
                    work.connection(),
                    ScheduleCompactInput {
                        session_id: input.session_id,
                        compact_summary_id: compact_summary_id.clone(),
                        source_first: payload
                            .get("source_first_timeline_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        source_last: payload
                            .get("source_last_timeline_id")
                            .and_then(Value::as_str)
                            .unwrap_or("0")
                            .to_owned(),
                        summary: payload.get("summary").cloned().unwrap_or_default(),
                        estimated_input_tokens: 0,
                        context_limit,
                    },
                )
                .await
                .map_err(|error| SessionsError::Internal(anyhow::anyhow!(error)))?;
            work.append_event(NewEvent {
                event_type: EventType::ContextChanged,
                actor: payload
                    .get("actor")
                    .cloned()
                    .unwrap_or_else(|| json!({"kind": "owner", "id": input.owner_id})),
                resource: Some(json!({"kind": "session", "id": session_id})),
                correlation_id: input.correlation_id.to_string(),
                causation_id: Some(created.operation.id.clone()),
                payload: json!({
                    "session_id": session_id,
                    "compact_summary_id": compact_summary_id,
                    "compact_status": "scheduled",
                }),
            })
            .await
            .map_err(SessionsError::Internal)?;
        }
        work.commit().await?;
        Ok(created.operation)
    }
}

fn compact_timeline_digest(items: &[janus_sessions::interface::TimelineItemView]) -> String {
    const MAX_DIGEST_BYTES: usize = 120_000;
    let mut digest = String::new();
    for item in items {
        let projection = serde_json::to_string(&item.projection)
            .unwrap_or_else(|_| "{\"error\":\"projection unavailable\"}".to_owned());
        let mut line = String::new();
        line.push_str("[order=");
        line.push_str(&item.display_order.to_string());
        line.push_str("] ");
        line.push_str(&item.kind);
        if let Some(turn_id) = &item.turn_id {
            line.push_str(" turn=");
            line.push_str(turn_id);
        }
        line.push_str(": ");
        line.push_str(&projection);
        line.push('\n');
        let remaining = MAX_DIGEST_BYTES.saturating_sub(digest.len());
        if remaining == 0 {
            break;
        }
        if line.len() > remaining {
            let mut end = remaining;
            while end > 0 && !line.is_char_boundary(end) {
                end -= 1;
            }
            digest.push_str(&line[..end]);
            break;
        }
        digest.push_str(&line);
    }
    digest
}

#[derive(Debug, Deserialize)]
struct CompactContextWork {
    operation_id: String,
    session_id: String,
    compact_summary_id: String,
    source_first_timeline_id: Option<String>,
    source_last_timeline_id: String,
    summary: Value,
    actor: Value,
}

pub(crate) async fn run_context_compact_operation(
    state: &Application,
    payload: &Value,
    work_id: &str,
    work_nonce: &str,
) -> Result<(), anyhow::Error> {
    let input: CompactContextWork = serde_json::from_value(payload.clone())?;
    let session_id: SessionId = input.session_id.parse()?;
    let claim = WorkClaim {
        id: work_id,
        nonce: work_nonce,
    };
    let step = state
        .operations()
        .begin_step_claimed(
            claim,
            &input.operation_id,
            "compact_context",
            json!({
                "session_id": input.session_id,
                "compact_summary_id": input.compact_summary_id,
            }),
        )
        .await?;
    state.operations().assert_claimed(claim).await?;

    if matches!(step, StepState::Running | StepState::NeedsReconciliation) {
        let mut work = state.unit_of_work().begin().await?;
        let changed = state
            .execution()
            .begin_context_compact_in_tx(work.connection(), session_id, &input.compact_summary_id)
            .await?;
        if changed {
            work.append_event(NewEvent {
                event_type: EventType::ContextChanged,
                actor: input.actor.clone(),
                resource: Some(json!({"kind": "session", "id": input.session_id})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: Some(input.operation_id.clone()),
                payload: json!({
                    "session_id": input.session_id,
                    "compact_summary_id": input.compact_summary_id,
                    "compact_status": "running",
                }),
            })
            .await?;
        }
        work.commit().await?;
    }

    let estimated_input_tokens = estimate_summary_tokens(&input.summary);
    let now = now_utc_str();
    let mut work = state.unit_of_work().begin().await?;
    let completed = state
        .execution()
        .complete_context_compact_in_tx(
            work.connection(),
            session_id,
            &input.compact_summary_id,
            estimated_input_tokens,
        )
        .await?;
    let (timeline_item_id, created) = state
        .sessions()
        .append_context_compacted_in_tx(
            work.connection(),
            ContextCompactedTimelineInput {
                session_id,
                compact_summary_id: &input.compact_summary_id,
                source_first_timeline_id: input.source_first_timeline_id.as_deref(),
                source_last_timeline_id: &input.source_last_timeline_id,
                summary: &input.summary,
                now: &now,
            },
        )
        .await?;
    let correlation_id = CorrelationId::new().to_string();
    if created {
        work.append_event(NewEvent {
            event_type: EventType::TimelineItemCreated,
            actor: input.actor.clone(),
            resource: Some(json!({"kind": "session", "id": input.session_id})),
            correlation_id: correlation_id.clone(),
            causation_id: Some(input.operation_id.clone()),
            payload: json!({
                "session_id": input.session_id,
                "timeline_item_id": timeline_item_id,
                "kind": "context_compacted",
                "compact_summary_id": input.compact_summary_id,
            }),
        })
        .await?;
    }
    if completed || created {
        work.append_event(NewEvent {
            event_type: EventType::ContextChanged,
            actor: input.actor,
            resource: Some(json!({"kind": "session", "id": input.session_id})),
            correlation_id,
            causation_id: Some(input.operation_id.clone()),
            payload: json!({
                "session_id": input.session_id,
                "compact_summary_id": input.compact_summary_id,
                "compact_status": "succeeded",
                "timeline_item_id": timeline_item_id,
            }),
        })
        .await?;
    }
    work.commit().await?;

    if matches!(step, StepState::Running | StepState::NeedsReconciliation) {
        state
            .operations()
            .complete_step_claimed(claim, &input.operation_id, "compact_context", None)
            .await?;
    }
    let finished = state
        .operations()
        .finish_claimed(
            &input.operation_id,
            work_id,
            work_nonce,
            janus_infrastructure::operations::OperationCompletion {
                status: OperationStatus::Succeeded,
                result: Some(json!({
                    "session_id": input.session_id,
                    "compact_summary_id": input.compact_summary_id,
                    "timeline_item_id": timeline_item_id,
                })),
                problem: None,
                correlation_id: CorrelationId::new(),
            },
        )
        .await?;
    if !finished {
        if state
            .operations()
            .get(&input.operation_id)
            .await?
            .is_some_and(|operation| operation.status == "succeeded")
        {
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "context compact operation lease became stale"
        ));
    }
    Ok(())
}

fn estimate_summary_tokens(summary: &Value) -> i64 {
    i64::try_from(summary.to_string().len().saturating_add(3) / 4).unwrap_or(i64::MAX)
}
