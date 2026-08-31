//! Turn and async-task persistence: loading queries and transactional writes.
use super::*;

use futures_util::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};

impl ExecutionInterface {
    pub async fn context_compact_in_progress(
        &self,
        session_id: SessionId,
    ) -> Result<bool, ExecutionError> {
        let count = self
            .pool
            .collection::<Document>("context_versions")
            .count_documents(doc! {
                "session_id": session_id.to_string(),
                "compact_status": {"$in": ["scheduled", "running"]},
            })
            .await?;
        Ok(count != 0)
    }

    pub async fn context_compact_in_progress_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
    ) -> Result<bool, ExecutionError> {
        let count = self
            .pool
            .collection::<Document>("context_versions")
            .count_documents(doc! {
                "session_id": session_id.to_string(),
                "compact_status": {"$in": ["scheduled", "running"]},
            })
            .session(&mut *tx)
            .await?;
        Ok(count != 0)
    }

    pub async fn schedule_context_compact_in_tx(
        &self,
        tx: &mut ClientSession,
        input: ScheduleCompactInput,
    ) -> Result<String, ExecutionError> {
        super::super::context::schedule_compact_in_tx(&self.pool, tx, input)
            .await
            .map_err(ExecutionError::Internal)
    }

    pub async fn begin_context_compact_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        compact_summary_id: &str,
    ) -> Result<bool, ExecutionError> {
        super::super::context::begin_compact_in_tx(&self.pool, tx, session_id, compact_summary_id)
            .await
            .map_err(ExecutionError::Internal)
    }

    pub async fn finalize_compact_summary_in_tx(
        &self,
        tx: &mut ClientSession,
        compact_summary_id: &str,
        summary: Value,
        model_attempt_id: Option<&str>,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), ExecutionError> {
        super::super::context::finalize_compact_summary_in_tx(
            &self.pool,
            tx,
            compact_summary_id,
            summary,
            model_attempt_id,
            input_tokens,
            output_tokens,
        )
        .await
        .map_err(ExecutionError::Internal)
    }

    pub async fn complete_context_compact_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        compact_summary_id: &str,
        estimated_input_tokens: i64,
    ) -> Result<bool, ExecutionError> {
        super::super::context::complete_compact_in_tx(
            &self.pool,
            tx,
            session_id,
            compact_summary_id,
            estimated_input_tokens,
        )
        .await
        .map_err(ExecutionError::Internal)
    }

    pub async fn turn_token_exchange(
        &self,
        turn_id: TurnId,
    ) -> Result<TurnTokenExchange, ExecutionError> {
        let mut cursor = self
            .pool
            .collection::<Document>("model_usage_ledger")
            .find(doc! {"turn_id": turn_id.to_string()})
            .sort(doc! {"occurred_at": 1, "_id": 1})
            .await?;
        let mut rows = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            rows.push((
                document.get_i64("input_tokens")?,
                document.get_i64("output_tokens")?,
            ));
        }
        Ok(aggregate_turn_token_exchange(
            &rows,
            estimated_system_prompt_tokens(),
        ))
    }

    pub async fn latest_context_usage(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ContextUsageView>, ExecutionError> {
        let document = self
            .pool
            .collection::<Document>("context_versions")
            .find_one(doc! {"session_id": session_id.to_string()})
            .sort(doc! {"sequence": -1})
            .await?;
        let view = match document {
            Some(doc) => Some(ContextUsageView {
                estimated_input_tokens: doc.get_i64("estimated_input_tokens")?,
                context_limit: doc.get_i64("context_limit")?,
                compact_status: doc.get_str("compact_status")?.to_owned(),
                created_at: doc.get_str("created_at")?.to_owned(),
            }),
            None => None,
        };
        Ok(view)
    }

    pub async fn latest_model_attempt_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Option<TurnModelAttempt>, ExecutionError> {
        let mut cursor = self
            .pool
            .collection::<Document>("rounds")
            .find(doc! {"turn_id": turn_id.to_string()})
            .sort(doc! {"sequence": 1})
            .await?;
        let mut round_ids = Vec::new();
        while let Some(document) = cursor.try_next().await? {
            let id = document
                .get_str("_id")?
                .parse::<RoundId>()
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
            round_ids.push(id);
        }
        let Some(attempt) = self.models.latest_attempt_for_rounds(&round_ids).await? else {
            return Ok(None);
        };
        let status = match attempt.status.as_str() {
            "running" => ModelAttemptStatus::Running,
            "succeeded" => ModelAttemptStatus::Succeeded,
            "failed" => ModelAttemptStatus::Failed,
            "canceled" => ModelAttemptStatus::Canceled,
            "interrupted" => ModelAttemptStatus::Interrupted,
            other => {
                return Err(ExecutionError::Internal(anyhow::anyhow!(
                    "unknown model attempt status {other}"
                )));
            }
        };
        Ok(Some(TurnModelAttempt {
            attempt: durable_retry_index(attempt.attempt, status),
            status,
            detail: attempt.detail,
        }))
    }

    pub(crate) async fn load_turn(&self, turn_id: TurnId) -> Result<ExecutionTurn, ExecutionError> {
        match self.sessions.execution_turn(turn_id).await {
            Ok(turn) => Ok(turn),
            Err(janus_sessions::interface::SessionsError::NotFound) => {
                Err(ExecutionError::TurnNotFound)
            }
            Err(error) => Err(ExecutionError::Sessions(error)),
        }
    }

    pub(crate) async fn load_chat_history(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        supports_images: bool,
    ) -> Result<(Vec<ChatMessage>, i64), ExecutionError> {
        let compact = super::super::context::latest_compact_summary(&self.pool, session_id)
            .await
            .map_err(ExecutionError::Internal)?;
        let rows = if let Some((_, source_last)) = compact.as_ref() {
            self.sessions
                .context_messages_after_timeline(session_id, turn_id, source_last)
                .await?
        } else {
            self.sessions.context_messages(session_id, turn_id).await?
        };
        let mut out = Vec::new();
        if let Some((summary, _)) = compact {
            out.push(ChatMessage {
                role: ChatRole::System,
                parts: vec![ContentPart::Text {
                    text: format!("Durable context summary:\n{summary}"),
                }],
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            });
        }
        let mut input_cursor = 0;
        let current_turn_id = turn_id.to_string();
        for row in rows {
            let role = match row.kind.as_str() {
                "user" => ChatRole::User,
                "assistant" => ChatRole::Assistant,
                "system" => ChatRole::System,
                "tool_result_ref" => ChatRole::Tool,
                _ => continue,
            };
            if row.turn_id.as_deref() == Some(current_turn_id.as_str()) && row.kind == "user" {
                input_cursor = input_cursor.max(row.timeline_sequence);
            }
            let tool_call_id = row
                .body
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let tool_calls = row
                .body
                .get("tool_calls")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let include_images = supports_images
                && row.turn_id.as_deref() == Some(current_turn_id.as_str())
                && row.kind == "user";
            // Thinking-mode providers must receive the assistant's prior
            // reasoning back verbatim. The display-formatted "reasoning"
            // field is intentionally not a protocol fallback: it may contain
            // line breaks inserted by the UI formatter and is not echo-safe.
            let reasoning_content = if matches!(role, ChatRole::Assistant) {
                row.body
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            out.push(ChatMessage {
                role,
                parts: self
                    .message_parts(session_id, &row.body, include_images)
                    .await?,
                tool_call_id,
                tool_calls,
                reasoning_content,
            });
        }
        Ok((out, input_cursor))
    }

    pub(crate) async fn load_turn_inputs_after(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        input_cursor: i64,
        supports_images: bool,
    ) -> Result<(Vec<ChatMessage>, i64), ExecutionError> {
        let rows = self
            .sessions
            .turn_inputs_after(turn_id, input_cursor)
            .await?;
        let mut out = Vec::new();
        let mut next_cursor = input_cursor;
        for row in rows {
            next_cursor = next_cursor.max(row.timeline_sequence);
            let mut parts = self
                .message_parts(session_id, &row.body, supports_images)
                .await?;
            if parts.is_empty() {
                continue;
            }
            let input_kind = row
                .body
                .get("turn_input")
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str);
            let prefix = match input_kind {
                Some("steer") => Some("[steer] "),
                _ => None,
            };
            if let Some(prefix) = prefix {
                if let Some(ContentPart::Text { text }) = parts
                    .iter_mut()
                    .find(|part| matches!(part, ContentPart::Text { .. }))
                {
                    text.insert_str(0, prefix);
                } else {
                    parts.insert(
                        0,
                        ContentPart::Text {
                            text: prefix.trim_end().to_owned(),
                        },
                    );
                }
            }
            out.push(ChatMessage {
                role: ChatRole::User,
                parts,
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            });
        }
        Ok((out, next_cursor))
    }

    pub(crate) async fn persist_tool_effects_in_tx(
        &self,
        tx: &mut ClientSession,
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        summary: &mut Value,
        now: &str,
    ) -> Result<(), ExecutionError> {
        if let Some(async_task_id) = summary.get("task_id").and_then(Value::as_str) {
            self.pool
                .collection::<Document>("tool_calls")
                .update_one(
                    doc! {"_id": tool_call_id.to_string(), "status": "running"},
                    doc! {"$set": {"async_task_id": async_task_id}},
                )
                .session(&mut *tx)
                .await?;
        }

        let Some(plan) = summary.get("plan").cloned() else {
            return Ok(());
        };
        let plan_id = plan
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("plan id is missing"))?;
        let todos = plan.get("todos").cloned().unwrap_or_else(|| json!([]));
        let evidence = plan.get("evidence").cloned().unwrap_or_else(|| json!([]));
        let latest = self
            .pool
            .collection::<Document>("plan_versions")
            .find_one(doc! {"turn_id": turn_id.to_string()})
            .sort(doc! {"sequence": -1})
            .session(&mut *tx)
            .await?;
        let sequence = latest
            .and_then(|doc| doc.get("sequence").and_then(Bson::as_i64))
            .unwrap_or(0)
            .saturating_add(1);
        let plan_json = todos.to_string();
        let evidence_json = evidence.to_string();
        let inserted = self
            .pool
            .collection::<Document>("plan_versions")
            .insert_one(doc! {
                "_id": plan_id,
                "turn_id": turn_id.to_string(),
                "sequence": sequence,
                "plan_json": &plan_json,
                "evidence_json": &evidence_json,
                "created_at": now,
            })
            .session(&mut *tx)
            .await;
        match inserted {
            Ok(_) => {
                if let Some(object) = summary.as_object_mut() {
                    object.insert("plan_version_id".into(), Value::String(plan_id.into()));
                    object.insert("sequence".into(), Value::from(sequence));
                }
            }
            Err(error) if is_duplicate_key(&error) => {
                // The model reused a plan.id that was already recorded; point
                // the summary at the row that actually exists, with its real
                // sequence, instead of a version that was never inserted.
                let existing = self
                    .pool
                    .collection::<Document>("plan_versions")
                    .find_one(doc! {"_id": plan_id})
                    .session(&mut *tx)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("plan version vanished after duplicate create")
                    })?;
                if let Some(object) = summary.as_object_mut() {
                    object.insert(
                        "plan_version_id".into(),
                        Value::String(existing.get_str("_id")?.to_owned()),
                    );
                    object.insert(
                        "sequence".into(),
                        Value::from(
                            existing
                                .get("sequence")
                                .and_then(Bson::as_i64)
                                .unwrap_or(sequence),
                        ),
                    );
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub async fn delete_session_execution_in_tx(
        &self,
        tx: &mut ClientSession,
        session_id: SessionId,
        turn_ids: &[TurnId],
    ) -> Result<(), ExecutionError> {
        for turn_id in turn_ids {
            self.pool
                .collection::<Document>("plan_versions")
                .delete_many(doc! {"turn_id": turn_id.to_string()})
                .session(&mut *tx)
                .await?;
            self.pool
                .collection::<Document>("rounds")
                .delete_many(doc! {"turn_id": turn_id.to_string()})
                .session(&mut *tx)
                .await?;
        }
        self.pool
            .collection::<Document>("context_versions")
            .delete_many(doc! {"session_id": session_id.to_string()})
            .session(&mut *tx)
            .await?;
        self.pool
            .collection::<Document>("compact_summaries")
            .delete_many(doc! {"session_id": session_id.to_string()})
            .session(&mut *tx)
            .await?;
        Ok(())
    }
}

fn is_duplicate_key(error: &mongodb::error::Error) -> bool {
    matches!(
        error.kind.as_ref(),
        mongodb::error::ErrorKind::Write(mongodb::error::WriteFailure::WriteError(write))
            if write.code == 11000
    )
}

/// Translate a stored 0-based ledger retry index into the 1-based reconnect
/// counter the conversation status row renders.
///
/// A `failed` attempt means the next retry is already scheduled, so it advances
/// the counter; a `running` attempt reports the retry it is itself executing,
/// which is `0` for the Round's first attempt. Settled attempts have no retry in
/// flight and report `0` so the status row does not keep a stale reconnect
/// notice on screen.
fn durable_retry_index(ledger_attempt: i64, status: ModelAttemptStatus) -> i64 {
    match status {
        ModelAttemptStatus::Failed => ledger_attempt.saturating_add(1),
        ModelAttemptStatus::Running => ledger_attempt.max(0),
        ModelAttemptStatus::Succeeded
        | ModelAttemptStatus::Canceled
        | ModelAttemptStatus::Interrupted => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelAttemptStatus, durable_retry_index};

    #[test]
    fn a_failed_attempt_announces_the_retry_that_follows_it() {
        assert_eq!(durable_retry_index(0, ModelAttemptStatus::Failed), 1);
        assert_eq!(durable_retry_index(4, ModelAttemptStatus::Failed), 5);
    }

    #[test]
    fn a_running_attempt_reports_the_retry_it_is_executing() {
        assert_eq!(durable_retry_index(0, ModelAttemptStatus::Running), 0);
        assert_eq!(durable_retry_index(3, ModelAttemptStatus::Running), 3);
    }

    #[test]
    fn settled_attempts_do_not_leave_a_reconnect_notice_behind() {
        for status in [
            ModelAttemptStatus::Succeeded,
            ModelAttemptStatus::Canceled,
            ModelAttemptStatus::Interrupted,
        ] {
            assert_eq!(durable_retry_index(7, status), 0, "{status:?}");
        }
    }
}
