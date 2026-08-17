//! Turn execution state machine: round streaming, tool execution,
//! completion and failure settlement.
use super::*;
use janus_infrastructure::id::ProjectId;
use std::path::Path;

fn as_i64_tokens(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn non_system_input_tokens(value: u64, system_prompt_tokens: i64) -> i64 {
    as_i64_tokens(value)
        .saturating_sub(system_prompt_tokens)
        .max(0)
}

fn estimate_chat_tokens(chat: &[ChatMessage]) -> i64 {
    i64::try_from(
        serde_json::to_string(chat)
            .map(|value| value.len().saturating_add(3) / 4)
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX)
}

fn estimate_prompt_tokens(prompt: &str) -> i64 {
    i64::try_from(prompt.len().saturating_add(3) / 4).unwrap_or(i64::MAX)
}

struct ToolExecutionContext<'a> {
    session_id: SessionId,
    project_id: ProjectId,
    turn_id: TurnId,
    actor: &'a Value,
    workspace_root: &'a Path,
    git_token: Option<&'a str>,
}

impl ExecutionInterface {
    /// Execute a running Turn until finish tool, model stop without tools, or failure.
    pub async fn execute_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<TurnExecutionOutcome, ExecutionError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status != TurnStatus::Running || !turn.active {
            return Ok(TurnExecutionOutcome); // idempotent
        }
        let session_id = turn.session_id;
        let owner_id = self.projects.owner_id(turn.project_id).await?;
        let workspace_root = self
            .projects
            .main_workspace_root(&owner_id, turn.project_id)
            .await?;
        let git_token = self
            .projects
            .git_token_for_project(&owner_id, turn.project_id)
            .await
            .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;

        let Some(model_snapshot) = turn.model_snapshot.as_ref() else {
            self.fail_turn(session_id, turn_id, "model is not configured")
                .await?;
            return Ok(TurnExecutionOutcome);
        };
        let system_prompt = self
            .system_prompt_for_turn(turn.project_id, turn.session_id, turn.goal_mode)
            .await?;
        let system_prompt_tokens = estimate_prompt_tokens(&system_prompt);
        let supports_images = model_snapshot.supports_images
            || model_snapshot
                .failover
                .iter()
                .any(|candidate| candidate.supports_images);

        let (mut chat, mut input_cursor) = self
            .load_chat_history(session_id, turn_id, supports_images)
            .await?;
        chat.insert(
            0,
            ChatMessage {
                role: ChatRole::System,
                parts: vec![ContentPart::Text {
                    text: system_prompt.clone(),
                }],
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            },
        );

        let has_attachments = !self
            .sessions
            .list_attachments(session_id)
            .await
            .map_err(|error| {
                ExecutionError::Internal(anyhow::anyhow!("list attachments: {error}"))
            })?
            .is_empty();
        let tools: Vec<ToolSpec> = available_tools(has_attachments)
            .into_iter()
            .map(|t| ToolSpec {
                name: t.name.into(),
                description: t.description.into(),
                parameters: t.parameters,
            })
            .collect();

        let actor = json!({"kind": "execution"});
        let mut finished = false;
        let mut finish_summary: Option<CompletionSummary> = None;

        let last_round_sequence: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sequence), 0) FROM rounds WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .fetch_one(&self.pool)
                .await?;
        let mut round_seq = last_round_sequence.saturating_add(1);
        loop {
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(TurnExecutionOutcome);
            }

            // Provider input usage describes the complete request context and
            // therefore includes Janus's system prefix. Count every model
            // attempt that belongs to this Turn, including retries/failovers,
            // while removing that prefix from each request. The ledger is the
            // authoritative source; rounds only retain the winning attempt.
            let usage_rows: Vec<(i64, i64)> = sqlx::query_as(
                "SELECT input_tokens, output_tokens FROM model_usage_ledger \
                 WHERE turn_id = ? ORDER BY occurred_at, id",
            )
            .bind(turn_id.to_string())
            .fetch_all(&self.pool)
            .await?;
            let turn_exchange = aggregate_turn_token_exchange(&usage_rows, system_prompt_tokens);
            let turn_input_base = turn_exchange.upload_tokens;
            let turn_output_base = turn_exchange.download_tokens;

            let (turn_inputs, next_cursor) = self
                .load_turn_inputs_after(session_id, turn_id, input_cursor, supports_images)
                .await?;
            chat.extend(turn_inputs);
            input_cursor = next_cursor;

            let round_id = RoundId::new();
            let now = now_utc_str();
            let version = format!("v_{}", RoundId::new());
            let mut work = self.unit_of_work.begin().await?;
            if !self
                .sessions
                .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
                .await?
            {
                work.rollback().await?;
                return Ok(TurnExecutionOutcome);
            }
            let context_version_id = record_context_version_in_tx(
                work.connection(),
                session_id,
                Some(&turn_id.to_string()),
                estimate_chat_tokens(&chat),
                i64::from(model_snapshot.context_limit.max(1)),
                "not_needed",
                json!({
                    "kind": "turn_round",
                    "message_count": chat.len(),
                }),
            )
            .await
            .map_err(ExecutionError::Internal)?;
            let inserted = sqlx::query(
                "INSERT INTO rounds \
                 (id, turn_id, sequence, context_version, status, candidate_snapshot_json, \
                  final_attempt_id, output_summary_json, input_tokens, output_tokens, \
                  stop_reason, version, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 'running', ?, NULL, NULL, 0, 0, NULL, ?, ?, ?)",
            )
            .bind(round_id.to_string())
            .bind(turn_id.to_string())
            .bind(round_seq)
            .bind(&context_version_id)
            .bind(serde_json::to_string(&model_snapshot.failover)?)
            .bind(&version)
            .bind(&now)
            .bind(&now)
            .execute(work.connection())
            .await?;
            if inserted.rows_affected() != 1 {
                work.rollback().await?;
                return Ok(TurnExecutionOutcome);
            }
            work.append_event(NewEvent {
                event_type: EventType::RoundChanged,
                actor: actor.clone(),
                resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "round_id": round_id.to_string(),
                    "turn_id": turn_id.to_string(),
                    "sequence": round_seq,
                    "status": "running",
                }),
            })
            .await?;
            work.commit().await?;

            let mut candidate_requests = Vec::with_capacity(1 + model_snapshot.failover.len());
            candidate_requests.push(ModelRequest {
                owner_id: owner_id.clone(),
                provider_id: model_snapshot.provider_id.clone(),
                upstream_model_id: model_snapshot.upstream_model_id.clone(),
                parameters: model_snapshot.parameters.clone(),
                messages: chat.clone(),
                tools: tools.clone(),
                round_id: Some(round_id.to_string()),
                project_id: Some(turn.project_id.to_string()),
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
            });
            candidate_requests.extend(model_snapshot.failover.iter().map(|candidate| {
                ModelRequest {
                    owner_id: owner_id.clone(),
                    provider_id: candidate.provider_id.clone(),
                    upstream_model_id: candidate.upstream_model_id.clone(),
                    parameters: candidate.parameters.clone(),
                    messages: chat.clone(),
                    tools: tools.clone(),
                    round_id: Some(round_id.to_string()),
                    project_id: Some(turn.project_id.to_string()),
                    session_id: Some(session_id.to_string()),
                    turn_id: Some(turn_id.to_string()),
                }
            }));

            self.state_broadcaster.push_stream_text(
                &session_id.to_string(),
                &turn_id.to_string(),
                json!({
                    "text": "",
                    "reasoning": "",
                    "seq": 0,
                    "round_id": round_id,
                    "turn_input_tokens": turn_input_base,
                    "turn_output_tokens": turn_output_base,
                    "turn_exchange_tokens": turn_input_base.saturating_add(turn_output_base),
                    "direction": "upload",
                }),
            );

            let stream_events = self.events.clone();
            let stream_broadcaster = self.state_broadcaster.clone();
            let stream_actor = actor.clone();
            let stream_project_id = turn.project_id.to_string();
            let stream_session_id = session_id.to_string();
            let stream_turn_id = turn_id.to_string();
            let stream_round_id = round_id.to_string();
            let accumulated_text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let accumulated_reasoning = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            // Server-side thinking interval: from the first reasoning delta to
            // the first answer delta. Pushed with the stream so the client can
            // label the Thought row with its duration the moment thinking
            // completes, instead of only when the durable round settles.
            let reasoning_started_at =
                std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
            let reasoning_ended_at =
                std::sync::Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
            let mut publish_stream_event = move |event: ModelStreamEvent| {
                let events = stream_events.clone();
                let broadcaster = stream_broadcaster.clone();
                let actor = stream_actor.clone();
                let project_id = stream_project_id.clone();
                let session_id = stream_session_id.clone();
                let turn_id = stream_turn_id.clone();
                let round_id = stream_round_id.clone();
                let acc_text = accumulated_text.clone();
                let acc_reasoning = accumulated_reasoning.clone();
                let timing_started = reasoning_started_at.clone();
                let timing_ended = reasoning_ended_at.clone();
                async move {
                    match event {
                        ModelStreamEvent::Delta {
                            attempt_id,
                            sequence,
                            channel,
                            text,
                            provisional,
                            usage,
                        } => {
                            let channel = match channel {
                                StreamChannel::Text => "text",
                                StreamChannel::ReasoningSummary => "reasoning_summary",
                                StreamChannel::ToolCallPreview => "tool_call_preview",
                            };
                            // Accumulate text for state push
                            if channel == "text"
                                && let Ok(mut acc) = acc_text.lock()
                            {
                                acc.push_str(&text);
                            } else if channel == "reasoning_summary"
                                && let Ok(mut acc) = acc_reasoning.lock()
                            {
                                acc.push_str(&text);
                            }
                            // Track the thinking interval across deltas.
                            if channel == "reasoning_summary" && !text.is_empty() {
                                if let Ok(mut started) = timing_started.lock()
                                    && started.is_none()
                                {
                                    *started = Some(std::time::Instant::now());
                                }
                            } else if channel == "text" && !text.is_empty() {
                                let thinking_started = timing_started
                                    .lock()
                                    .ok()
                                    .is_some_and(|started| started.is_some());
                                if thinking_started
                                    && let Ok(mut ended) = timing_ended.lock()
                                    && ended.is_none()
                                {
                                    *ended = Some(std::time::Instant::now());
                                }
                            }
                            let reasoning_duration_ms = match (
                                timing_started.lock().ok().map(|guard| *guard),
                                timing_ended.lock().ok().map(|guard| *guard),
                            ) {
                                (Some(Some(started)), Some(Some(ended))) => u64::try_from(
                                    ended.saturating_duration_since(started).as_millis(),
                                )
                                .ok(),
                                _ => None,
                            };
                            let current_text =
                                acc_text.lock().ok().map(|a| a.clone()).unwrap_or_default();
                            let current_reasoning = acc_reasoning
                                .lock()
                                .ok()
                                .map(|a| a.clone())
                                .unwrap_or_default();
                            let (turn_input_tokens, turn_output_tokens) = usage
                                .as_ref()
                                .map(|usage| {
                                    (
                                        turn_input_base.saturating_add(non_system_input_tokens(
                                            usage.input_tokens,
                                            system_prompt_tokens,
                                        )),
                                        turn_output_base
                                            .saturating_add(as_i64_tokens(usage.output_tokens)),
                                    )
                                })
                                .unwrap_or((turn_input_base, turn_output_base));
                            let usage_value = usage.as_ref().map(|u| {
                                json!({
                                    "input_tokens": u.input_tokens,
                                    "output_tokens": u.output_tokens,
                                    "cache_tokens": u.cache_tokens,
                                })
                            });
                            // Push accumulated text to live clients via broadcaster.
                            // round_id lets the client match a live stream to the
                            // durable assistant timeline item and drop the
                            // provisional overlay once the round is persisted.
                            // reasoning_duration_ms is present from the first
                            // answer delta onward so the Thought row can show
                            // its duration immediately.
                            broadcaster.push_stream_text(
                                &session_id,
                                &turn_id,
                                json!({
                                    "text": current_text,
                                    "reasoning": current_reasoning,
                                    "seq": sequence,
                                    "round_id": round_id,
                                    "usage": usage.as_ref().map(|u| json!({
                                        "input_tokens": non_system_input_tokens(
                                            u.input_tokens,
                                            system_prompt_tokens,
                                        ),
                                        "output_tokens": u.output_tokens,
                                        "cache_tokens": u.cache_tokens,
                                    })),
                                    "turn_input_tokens": turn_input_tokens,
                                    "turn_output_tokens": turn_output_tokens,
                                    "turn_exchange_tokens": turn_input_tokens
                                        .saturating_add(turn_output_tokens),
                                    "direction": "download",
                                    "reasoning_duration_ms": reasoning_duration_ms,
                                }),
                            );
                            // Also persist to EventStore for replay
                            let _ = events
                                .append(NewEvent {
                                    event_type: EventType::ModelStreamDelta,
                                    actor,
                                    resource: Some(json!({"kind": "round", "id": round_id})),
                                    correlation_id: CorrelationId::new().to_string(),
                                    causation_id: None,
                                    payload: json!({
                                        "project_id": project_id,
                                        "session_id": session_id,
                                        "turn_id": turn_id,
                                        "round_id": round_id,
                                        "attempt_id": attempt_id,
                                        "sequence": sequence,
                                        "channel": channel,
                                        "delta": text,
                                        "provisional": provisional,
                                        "usage": usage_value,
                                    }),
                                })
                                .await;
                        }
                        ModelStreamEvent::Retrying {
                            attempt_id,
                            attempt,
                            detail,
                            retry_after_ms,
                        } => {
                            // Reset accumulated text on retry — new attempt starts fresh
                            if let Ok(mut acc) = acc_text.lock() {
                                acc.clear();
                            }
                            if let Ok(mut acc) = acc_reasoning.lock() {
                                acc.clear();
                            }
                            broadcaster.push_stream_text(
                                &session_id,
                                &turn_id,
                                json!({
                                    "text": "",
                                    "reasoning": "",
                                    "seq": 0,
                                    "retrying": true,
                                    "attempt": attempt,
                                    "detail": detail,
                                    "turn_input_tokens": turn_input_base,
                                    "turn_output_tokens": turn_output_base,
                                    "turn_exchange_tokens": turn_input_base
                                        .saturating_add(turn_output_base),
                                    "direction": "upload",
                                }),
                            );
                            let _ = events
                                .append(NewEvent {
                                    event_type: EventType::ModelAttemptRetrying,
                                    actor,
                                    resource: Some(json!({"kind": "turn", "id": turn_id})),
                                    correlation_id: CorrelationId::new().to_string(),
                                    causation_id: None,
                                    payload: json!({
                                        "project_id": project_id,
                                        "session_id": session_id,
                                        "turn_id": turn_id,
                                        "round_id": round_id,
                                        "attempt_id": attempt_id,
                                        "attempt": attempt,
                                        "detail": detail,
                                        "retry_after_ms": retry_after_ms,
                                    }),
                                })
                                .await;
                        }
                        ModelStreamEvent::ToolCallDelta { .. }
                        | ModelStreamEvent::Completed { .. }
                        | ModelStreamEvent::Failed { .. } => {}
                    }
                }
            };
            let events = self
                .try_round_stream(candidate_requests, &mut publish_stream_event)
                .await?;
            if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                return Ok(TurnExecutionOutcome);
            }

            let completed = events.iter().find_map(|e| match e {
                ModelStreamEvent::Completed {
                    attempt_id,
                    usage,
                    stop_reason,
                    tool_calls,
                    text,
                    reasoning,
                    reasoning_content,
                    reasoning_duration_ms,
                } => Some((
                    attempt_id.clone(),
                    usage.clone(),
                    stop_reason.clone(),
                    tool_calls.clone(),
                    text.clone(),
                    reasoning.clone(),
                    reasoning_content.clone(),
                    *reasoning_duration_ms,
                )),
                _ => None,
            });

            let Some((
                attempt_id,
                usage,
                stop_reason,
                tool_calls,
                text,
                reasoning,
                reasoning_content,
                reasoning_duration_ms,
            )) = completed
            else {
                // Failed stream — no tool execution. Retryable provider faults
                // Retryable provider faults stay inside the round retry loop;
                // deterministic faults fail the Turn immediately.
                let detail = events
                    .iter()
                    .find_map(|e| match e {
                        ModelStreamEvent::Failed { code, detail, .. } => {
                            Some(format!("{code}: {detail}"))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "stream failed".into());
                self.fail_round(session_id, turn_id, &round_id, &detail)
                    .await?;
                self.fail_turn(session_id, turn_id, &detail).await?;
                return Ok(TurnExecutionOutcome);
            };

            let Some(accepted_calls) = self
                .accept_round_response(AcceptedRoundResponse {
                    session_id,
                    turn_id,
                    round_id: &round_id,
                    attempt_id: &attempt_id,
                    input_tokens: non_system_input_tokens(usage.input_tokens, system_prompt_tokens),
                    output_tokens: as_i64_tokens(usage.output_tokens),
                    stop_reason: stop_reason.as_deref(),
                    text: &text,
                    reasoning: &reasoning,
                    reasoning_content: reasoning_content.as_deref(),
                    reasoning_duration_ms,
                    tool_calls: &tool_calls,
                    actor: &actor,
                })
                .await?
            else {
                return Ok(TurnExecutionOutcome);
            };

            if !text.is_empty() || !tool_calls.is_empty() || reasoning_content.is_some() {
                chat.push(ChatMessage {
                    role: ChatRole::Assistant,
                    parts: vec![ContentPart::Text { text: text.clone() }],
                    tool_call_id: None,
                    tool_calls: tool_calls.clone(),
                    // Keep only the provider's raw reasoning on the in-memory
                    // history. The display summary is not safe to echo back.
                    reasoning_content,
                });
            }

            if tool_calls.is_empty() {
                // Model stopped without tools — complete turn with text as summary.
                finish_summary = Some(CompletionSummary::from_text(&text));
                finished = true;
                break;
            }

            // Execute tools in declaration order. Async Bash is a completed
            // tool call from the Turn's perspective; its terminal result is
            // delivered later as a separate system Turn.
            let mut round_tool_messages: Vec<ChatMessage> = Vec::new();
            let mut stop_executing = false;
            let mut skip_reason = "a prior tool finished this Round";
            for accepted_call in &accepted_calls {
                if stop_executing {
                    let message = self
                        .settle_unrun_tool_call(
                            session_id,
                            turn_id,
                            accepted_call,
                            skip_reason,
                            &actor,
                        )
                        .await?;
                    round_tool_messages.push(message);
                    continue;
                }
                if !self.sessions.turn_is_runnable(session_id, turn_id).await? {
                    return Ok(TurnExecutionOutcome);
                }
                let Some(executed) = self
                    .run_one_tool(
                        ToolExecutionContext {
                            session_id,
                            project_id: turn.project_id,
                            turn_id,
                            actor: &actor,
                            workspace_root: &workspace_root,
                            git_token: git_token.as_deref(),
                        },
                        accepted_call,
                    )
                    .await?
                else {
                    return Ok(TurnExecutionOutcome);
                };
                let ExecutedToolCall { outcome, message } = executed;
                if let Some(fs) = outcome.finish_summary {
                    finish_summary = Some(fs);
                    finished = true;
                    stop_executing = true;
                    skip_reason = "a prior tool finished the Turn";
                }
                round_tool_messages.push(message);
            }
            chat.extend(round_tool_messages);
            if finished {
                break;
            }

            round_seq = round_seq.saturating_add(1);
        }

        if finished {
            self.complete_turn(
                session_id,
                turn_id,
                finish_summary.unwrap_or_else(|| CompletionSummary::from_text("")),
            )
            .await?;
        }
        Ok(TurnExecutionOutcome)
    }

    async fn system_prompt_for_turn(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        goal_mode: bool,
    ) -> Result<String, ExecutionError> {
        let memory = self.projects.memory_context(project_id).await?;
        let active_sessions = self.sessions.active_sessions(50).await?;
        let async_tasks = self.runtime.async_tasks(100).await?;
        let mut prompt = SYSTEM_PROMPT.to_owned();
        if goal_mode {
            prompt.push_str(
                "\n\nGoal mode:\nContinue pursuing the user's objective across as many Rounds as needed. ",
            );
            prompt.push_str(
                "Do not stop merely because one subtask finished; inspect evidence, keep editing the Main workspace, and stop only when the objective is complete or a concrete blocker remains.",
            );
        }
        if !memory.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&memory);
        }
        if !active_sessions.is_empty() {
            prompt.push_str("\n\nActive sessions in this project:\n");
            for session in active_sessions {
                prompt.push_str("- ");
                prompt.push_str(&session.id);
                if session.id == session_id.to_string() {
                    prompt.push_str(" (current)");
                }
                prompt.push_str(" [project ");
                prompt.push_str(&session.project_id);
                prompt.push(']');
                if let Some(title) = session.title {
                    prompt.push_str(": ");
                    prompt.push_str(&title);
                }
                if let Some(turn_id) = session.active_turn_id {
                    prompt.push_str(" [turn ");
                    prompt.push_str(&turn_id);
                    prompt.push(']');
                }
                prompt.push('\n');
            }
        }
        if !async_tasks.is_empty() {
            prompt.push_str("\nGlobal async Bash tasks:\n");
            for task in async_tasks {
                prompt.push_str("- ");
                prompt.push_str(&task.id.to_string());
                prompt.push_str(" [");
                prompt.push_str(task.status.as_str());
                prompt.push_str("] ");
                prompt.push_str(&task.command_summary);
                prompt.push('\n');
            }
        }
        Ok(prompt)
    }
}

impl ExecutionInterface {
    pub(crate) async fn message_parts(
        &self,
        session_id: SessionId,
        body: &Value,
        include_images: bool,
    ) -> Result<Vec<ContentPart>, ExecutionError> {
        let mut parts = Vec::new();
        for part in body
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        parts.push(ContentPart::Text {
                            text: text.to_owned(),
                        });
                    }
                }
                Some("attachment_reference") => {
                    let attachment_id = part
                        .get("attachment_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow::anyhow!("attachment reference is missing its id"))?
                        .parse()
                        .map_err(|error| anyhow::anyhow!("invalid attachment id: {error}"))?;
                    let attachment = self
                        .sessions
                        .get_attachment(session_id, attachment_id)
                        .await?;
                    let metadata = json!({
                        "id": attachment.id.to_string(),
                        "name": attachment.name,
                        "mime": attachment.mime,
                        "byte_size": attachment.byte_size,
                    });
                    parts.push(ContentPart::Text {
                        text: format!(
                            "[Session attachment {metadata}. Use attachment.read or attachment.save with this id.]"
                        ),
                    });
                    if include_images && attachment.blob_sha.is_some() {
                        let bytes = read_attachment_bytes(&self.blobs, &attachment)
                            .await?
                            .ok_or_else(|| {
                                ExecutionError::Internal(anyhow::anyhow!(
                                    "attachment {} content is unavailable",
                                    attachment.id
                                ))
                            })?;
                        if let Some(mime) = supported_image_mime(&bytes) {
                            parts.push(ContentPart::Image {
                                mime: mime.to_owned(),
                                bytes,
                                width: None,
                                height: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(parts)
    }

    /// Run one Round's model stream across ordered candidates. Transient
    /// failures retry the current candidate indefinitely with low-frequency
    /// backoff after the reconnect notice threshold. Configuration failures
    /// move to the next candidate because another configured route may work.
    ///
    /// Provisional attempts that fail never contribute tool calls; their only
    /// durable footprint is the `model_attempts` rows the stream layer already
    /// writes. Usage from any succeeded attempt is reported by the stream layer
    /// in its `Completed` event and aggregated normally.
    async fn try_round_stream<F, Fut>(
        &self,
        requests: Vec<ModelRequest>,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ExecutionError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut last_failed_events = Vec::new();
        for (candidate_index, req) in requests.into_iter().enumerate() {
            let candidate_order = i64::try_from(candidate_index).map_err(|_| {
                ExecutionError::Internal(anyhow::anyhow!("candidate index overflow"))
            })?;
            // The retry index is unbounded. The first five retries are
            // presented as reconnecting; later retries continue at a slower
            // cadence until the provider succeeds or the Turn is canceled.
            let mut attempt = 0;
            loop {
                let events = self
                    .models
                    .stream_completion_with_candidate(req.clone(), candidate_order, on_event)
                    .await?;
                let failed = events.iter().rev().find_map(|e| match e {
                    ModelStreamEvent::Failed {
                        attempt_id,
                        code,
                        detail,
                    } => Some((attempt_id.clone(), code.clone(), detail.clone())),
                    _ => None,
                });
                let Some((attempt_id, code, detail)) = failed else {
                    return Ok(events);
                };
                let retry_attempt = attempt + 1;
                let decision = classify(&code, &detail, retry_attempt);
                match decision.class {
                    FaultClass::Config => {
                        last_failed_events = events;
                        break;
                    }
                    FaultClass::Transient => {
                        let retrying = ModelStreamEvent::Retrying {
                            attempt_id: attempt_id.clone(),
                            attempt: retry_attempt,
                            detail: detail.clone(),
                            retry_after_ms: decision.retry_after.as_millis() as u64,
                        };
                        on_event(retrying).await;
                        tokio::time::sleep(decision.retry_after).await;
                        attempt = retry_attempt;
                    }
                }
            }
        }
        Ok(last_failed_events)
    }

    async fn accept_round_response(
        &self,
        input: AcceptedRoundResponse<'_>,
    ) -> Result<Option<Vec<AcceptedToolCall>>, ExecutionError> {
        let AcceptedRoundResponse {
            session_id,
            turn_id,
            round_id,
            attempt_id,
            input_tokens,
            output_tokens,
            stop_reason,
            text,
            reasoning,
            reasoning_content,
            reasoning_duration_ms,
            tool_calls,
            actor,
        } = input;
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            return Ok(None);
        }
        let mut round_body = json!({"text": text, "reasoning": reasoning});
        if let Some(raw) = reasoning_content {
            round_body["reasoning_content"] = json!(raw);
        }
        let accepted = sqlx::query(
            "UPDATE rounds SET status = 'succeeded', final_attempt_id = ?, input_tokens = ?, \
             output_tokens = ?, stop_reason = ?, output_summary_json = ?, updated_at = ? \
             WHERE id = ? AND status = 'running' AND turn_id = ?",
        )
        .bind(attempt_id)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(stop_reason)
        .bind(round_body.to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if accepted.rows_affected() != 1 {
            return Ok(None);
        }

        let declared_calls = serde_json::to_value(tool_calls)?;
        let (_, timeline_item_id, _) = self
            .sessions
            .append_assistant_message_in_tx(
                work.connection(),
                AppendAssistantMessage {
                    session_id,
                    turn_id,
                    round_id: *round_id,
                    text,
                    reasoning,
                    reasoning_content,
                    duration_ms: reasoning_duration_ms
                        .map(|duration| duration.min(i64::MAX as u64) as i64),
                    tool_calls: &declared_calls,
                    actor,
                    now: &now,
                },
            )
            .await?;
        let mut persisted_calls = Vec::with_capacity(tool_calls.len());
        for (ordinal, request) in tool_calls.iter().enumerate() {
            let id = ToolCallId::new();
            let input = serde_json::from_str::<Value>(&request.arguments_json)
                .unwrap_or_else(|_| json!({}));
            sqlx::query(
                "INSERT INTO tool_calls \
                 (id, round_id, ord, tool_name, schema_version, input_json, result_summary_json, \
                  status, actor_json, error_code, provider_call_id, started_at, ended_at, version) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, 'requested', ?, NULL, ?, NULL, NULL, ?)",
            )
            .bind(id.to_string())
            .bind(round_id.to_string())
            .bind(ordinal as i64)
            .bind(&request.name)
            .bind(SCHEMA_VERSION)
            .bind(input.to_string())
            .bind(actor.to_string())
            .bind(&request.id)
            .bind(format!("v_{}", ToolCallId::new()))
            .execute(work.connection())
            .await?;
            persisted_calls.push(AcceptedToolCall {
                id,
                ordinal: ordinal as i64,
                request: request.clone(),
            });
        }
        let correlation_id = CorrelationId::new().to_string();
        work.append_event(NewEvent {
            event_type: EventType::RoundChanged,
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
            correlation_id: correlation_id.clone(),
            causation_id: None,
            payload: json!({
                "round_id": round_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "status": "succeeded",
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }),
        })
        .await?;
        if let Some(timeline_item_id) = timeline_item_id {
            work.append_event(NewEvent {
                event_type: EventType::TimelineItemCreated,
                actor: actor.clone(),
                resource: Some(json!({"kind": "session", "id": session_id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "timeline_item_id": timeline_item_id,
                    "kind": "assistant_message",
                }),
            })
            .await?;
        }
        for call in &persisted_calls {
            work.append_event(NewEvent {
                event_type: EventType::ToolCallCreated,
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": call.id.to_string()})),
                correlation_id: correlation_id.clone(),
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "tool_call_id": call.id.to_string(),
                    "provider_call_id": call.request.id,
                    "tool_name": call.request.name,
                    "status": "requested",
                    "ordinal": call.ordinal,
                }),
            })
            .await?;
        }
        work.commit().await?;
        Ok(Some(persisted_calls))
    }

    async fn fail_round(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        round_id: &RoundId,
        detail: &str,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            return Ok(());
        }
        let changed = sqlx::query(
            "UPDATE rounds SET status = 'failed', stop_reason = ?, output_summary_json = ?, \
              updated_at = ? WHERE id = ? AND status = 'running' AND turn_id = ?",
        )
        .bind("error")
        .bind(json!({"error": detail}).to_string())
        .bind(&now)
        .bind(round_id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if changed.rows_affected() != 1 {
            return Ok(());
        }
        work.append_event(NewEvent {
            event_type: EventType::RoundChanged,
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "round", "id": round_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "round_id": round_id.to_string(),
                "session_id": session_id.to_string(),
                "turn_id": turn_id.to_string(),
                "status": "failed",
                "detail": detail,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    async fn settle_unrun_tool_call(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        accepted: &AcceptedToolCall,
        reason: &str,
        actor: &Value,
    ) -> Result<ChatMessage, ExecutionError> {
        let now = now_utc_str();
        let input = serde_json::from_str::<Value>(&accepted.request.arguments_json)
            .unwrap_or_else(|_| json!({}));
        let text = format!(
            "tool `{}` was not executed because {reason}",
            accepted.request.name
        );
        let durable_parts = json!([{"type": "text", "text": text}]);
        let message = ChatMessage {
            role: ChatRole::Tool,
            parts: vec![ContentPart::Text { text: text.clone() }],
            tool_call_id: Some(accepted.request.id.clone()),
            tool_calls: Vec::new(),
            reasoning_content: None,
        };
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Failed,
            parts: vec![ToolResultPart::Text { text: text.clone() }],
            summary: json!({
                "ok": false,
                "skipped": true,
                "reason": reason,
                "detail": text,
            }),
            error_code: Some("TOOL_SKIPPED_AFTER_BLOCK".into()),
            finish_summary: None,
        };
        attach_tool_display(&accepted.request.name, &input, &mut outcome);
        let summary = outcome.summary;
        let mut work = self.unit_of_work.begin().await?;
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = 'canceled', result_summary_json = ?, error_code = ?, \
              ended_at = ?, version = ? \
             WHERE id = ? AND status = 'requested' \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(summary.to_string())
        .bind("TOOL_SKIPPED_AFTER_BLOCK")
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if changed.rows_affected() == 1 {
            let (_, timeline_item_id, _) = self
                .sessions
                .append_tool_result_in_tx(
                    work.connection(),
                    AppendToolResultInput {
                        session_id,
                        turn_id,
                        tool_call_id: &accepted.id.to_string(),
                        provider_call_id: &accepted.request.id,
                        tool_name: &accepted.request.name,
                        status: "canceled",
                        summary: &summary,
                        model_parts: &durable_parts,
                        actor,
                        now: &now,
                    },
                )
                .await?;
            work.append_event(NewEvent {
                event_type: EventType::ToolCallChanged,
                actor: actor.clone(),
                resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
                correlation_id: CorrelationId::new().to_string(),
                causation_id: None,
                payload: json!({
                    "session_id": session_id.to_string(),
                    "tool_call_id": accepted.id.to_string(),
                    "provider_call_id": accepted.request.id,
                    "tool_name": accepted.request.name,
                    "status": "canceled",
                    "summary": summary,
                    "timeline_item_id": timeline_item_id,
                    "skipped": true,
                }),
            })
            .await?;
            work.commit().await?;
        } else {
            work.rollback().await?;
        }
        Ok(message)
    }

    async fn read_paths_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<HashSet<String>, ExecutionError> {
        let paths: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT json_extract(tc.input_json, '$.path') FROM tool_calls tc \
             JOIN rounds r ON r.id = tc.round_id \
             WHERE r.turn_id = ? AND tc.tool_name = 'read' AND tc.status = 'succeeded'",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(paths.into_iter().flatten().collect())
    }

    async fn run_one_tool(
        &self,
        context: ToolExecutionContext<'_>,
        accepted: &AcceptedToolCall,
    ) -> Result<Option<ExecutedToolCall>, ExecutionError> {
        let ToolExecutionContext {
            session_id,
            project_id,
            turn_id,
            actor,
            workspace_root,
            git_token,
        } = context;
        let now = now_utc_str();
        let input: Value =
            serde_json::from_str(&accepted.request.arguments_json).unwrap_or_else(|_| json!({}));
        let mut work = self.unit_of_work.begin().await?;
        if !self
            .sessions
            .turn_is_runnable_in_tx(work.connection(), session_id, turn_id)
            .await?
        {
            work.rollback().await?;
            return Ok(None);
        }
        let started = sqlx::query(
            "UPDATE tool_calls SET status = 'running', started_at = ?, version = ? \
             WHERE id = ? AND status = 'requested' \
               AND round_id IN (SELECT id FROM rounds WHERE turn_id = ?)",
        )
        .bind(&now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .bind(turn_id.to_string())
        .execute(work.connection())
        .await?;
        if started.rows_affected() != 1 {
            work.rollback().await?;
            return Ok(None);
        }
        work.append_event(NewEvent {
            event_type: EventType::ToolCallChanged,
            actor: actor.clone(),
            resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "tool_call_id": accepted.id.to_string(),
                "provider_call_id": accepted.request.id,
                "tool_name": accepted.request.name,
                "status": "running",
            }),
        })
        .await?;
        work.commit().await?;

        let read_paths = self.read_paths_for_turn(turn_id).await?;
        let ctx = ToolContext {
            project_id,
            session_id,
            turn_id,
            tool_call_id: accepted.id,
            workspace: &self.workspace,
            workspace_root,
            workspace_handle: janus_workspace::interface::WorkspaceHandle::main(project_id),
            sessions: &self.sessions,
            projects: &self.projects,
            blobs: &self.blobs,
            runtime: &self.runtime,
            git_token,
            read_paths: &read_paths,
            actor: actor.clone(),
        };
        let mut outcome = match execute_tool(&ctx, &accepted.request.name, &input).await {
            Ok(outcome) => outcome,
            Err(error @ ExecutionError::Storage(_))
            | Err(error @ ExecutionError::Serde(_))
            | Err(error @ ExecutionError::Internal(_)) => return Err(error),
            Err(error) => {
                let mut outcome = crate::types::ToolOutcome {
                    disposition: ToolExecutionDisposition::Failed,
                    parts: vec![ToolResultPart::Text {
                        text: error.to_string(),
                    }],
                    summary: json!({
                        "ok": false,
                        "error": error.to_string(),
                        "detail": error.to_string(),
                    }),
                    error_code: Some("TOOL_EXECUTION_FAILED".into()),
                    finish_summary: None,
                };
                attach_tool_display(&accepted.request.name, &input, &mut outcome);
                outcome
            }
        };
        let ended = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        self.persist_tool_effects_in_tx(
            work.connection(),
            turn_id,
            accepted.id,
            &mut outcome.summary,
            &ended,
        )
        .await?;
        if outcome.summary.get("plan_version_id").is_some()
            && let Some(ToolResultPart::Json { value }) = outcome.parts.first_mut()
        {
            *value = outcome.summary.clone();
        }
        let (message, durable_parts) = tool_result_message(&outcome, &accepted.request.id);
        let status = outcome.disposition.as_str();
        let ended_at = Some(ended.as_str());
        let finalized = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = ?, \
              ended_at = ?, version = ? WHERE id = ? AND status = 'running'",
        )
        .bind(status)
        .bind(outcome.summary.to_string())
        .bind(&outcome.error_code)
        .bind(ended_at)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(accepted.id.to_string())
        .execute(work.connection())
        .await?;
        if finalized.rows_affected() != 1 {
            work.rollback().await?;
            return Ok(None);
        }
        let (_, timeline_item_id, _) = self
            .sessions
            .append_tool_result_in_tx(
                work.connection(),
                AppendToolResultInput {
                    session_id,
                    turn_id,
                    tool_call_id: &accepted.id.to_string(),
                    provider_call_id: &accepted.request.id,
                    tool_name: &accepted.request.name,
                    status,
                    summary: &outcome.summary,
                    model_parts: &durable_parts,
                    actor,
                    now: &ended,
                },
            )
            .await?;
        work.append_event(NewEvent {
            event_type: EventType::ToolCallChanged,
            actor: actor.clone(),
            resource: Some(json!({"kind": "tool_call", "id": accepted.id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "session_id": session_id.to_string(),
                "tool_call_id": accepted.id.to_string(),
                "provider_call_id": accepted.request.id,
                "tool_name": accepted.request.name,
                "status": status,
                "summary": outcome.summary,
                "timeline_item_id": timeline_item_id,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(Some(ExecutedToolCall { outcome, message }))
    }

    async fn complete_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        summary: CompletionSummary,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let summary_value = serde_json::to_value(&summary)?;
        let mut work = self.unit_of_work.begin().await?;
        let unfinished_calls: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM tool_calls AS call \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE round.turn_id = ? AND call.status IN ('requested', 'running')",
        )
        .bind(turn_id.to_string())
        .fetch_one(work.connection())
        .await?;
        if unfinished_calls > 0 {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "Turn completion attempted with unfinished Tool Calls"
            )));
        }
        let (input_tokens, output_tokens): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0) \
             FROM rounds WHERE turn_id = ? AND status = 'succeeded'",
        )
        .bind(turn_id.to_string())
        .fetch_one(work.connection())
        .await?;
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                work.connection(),
                session_id,
                turn_id,
                ActiveTurnOutcome::Completed {
                    summary: &summary_value,
                    input_tokens,
                    output_tokens,
                },
                &now,
            )
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(());
        };
        work.append_event(NewEvent {
            event_type: EventType::TurnStatusChanged,
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "summary": summary_value,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    async fn fail_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), ExecutionError> {
        let now = now_utc_str();
        let mut work = self.unit_of_work.begin().await?;
        let summary = json!({"error": reason});
        let transition = self
            .sessions
            .settle_active_turn_in_tx(
                work.connection(),
                session_id,
                turn_id,
                ActiveTurnOutcome::Failed {
                    reason,
                    summary: &summary,
                },
                &now,
            )
            .await?;
        let Some(transition) = transition else {
            work.rollback().await?;
            return Ok(());
        };
        work.append_event(NewEvent {
            event_type: EventType::TurnStatusChanged,
            actor: json!({"kind": "execution"}),
            resource: Some(json!({"kind": "turn", "id": turn_id.to_string()})),
            correlation_id: CorrelationId::new().to_string(),
            causation_id: None,
            payload: json!({
                "turn_id": turn_id.to_string(),
                "from": transition.from_status.as_str(),
                "to": transition.to_status.as_str(),
                "reason": reason,
                "session_version": transition.session_version,
            }),
        })
        .await?;
        work.commit().await?;
        Ok(())
    }

    pub async fn settle_execution_failure(
        &self,
        turn_id: TurnId,
        reason: &str,
    ) -> Result<(), ExecutionError> {
        let turn = self.load_turn(turn_id).await?;
        if turn.status != TurnStatus::Running || !turn.active {
            return Ok(());
        }
        let session_id = turn.session_id;
        self.fail_turn(session_id, turn_id, reason).await
    }
}

#[cfg(test)]
mod tests {
    use super::{aggregate_turn_token_exchange, estimated_system_prompt_tokens};

    #[test]
    fn turn_exchange_sums_attempts_and_excludes_system_prefix() {
        let exchange = aggregate_turn_token_exchange(&[(100, 20), (50, 5)], 10);
        assert_eq!(exchange.upload_tokens, 130);
        assert_eq!(exchange.download_tokens, 25);
    }

    #[test]
    fn turn_exchange_does_not_underflow_small_input() {
        let exchange = aggregate_turn_token_exchange(&[(5, 2)], 10);
        assert_eq!(exchange.upload_tokens, 0);
        assert_eq!(exchange.download_tokens, 2);
    }

    #[test]
    fn system_prompt_estimate_is_stable_and_positive() {
        assert!(estimated_system_prompt_tokens() > 0);
    }
}
