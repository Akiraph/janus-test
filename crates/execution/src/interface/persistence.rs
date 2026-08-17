//! Turn and async-task persistence: loading queries and transactional writes.
use super::*;

impl ExecutionInterface {
    pub async fn context_compact_in_progress(
        &self,
        session_id: SessionId,
    ) -> Result<bool, ExecutionError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM context_versions \
             WHERE session_id = ? AND compact_status IN ('scheduled', 'running'))",
        )
        .bind(session_id.to_string())
        .fetch_one(&self.pool)
        .await?;
        Ok(exists != 0)
    }

    pub async fn context_compact_in_progress_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
    ) -> Result<bool, ExecutionError> {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM context_versions \
             WHERE session_id = ? AND compact_status IN ('scheduled', 'running'))",
        )
        .bind(session_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        Ok(exists != 0)
    }

    pub async fn schedule_context_compact_in_tx(
        &self,
        tx: &mut SqliteConnection,
        input: ScheduleCompactInput,
    ) -> Result<String, ExecutionError> {
        super::super::context::schedule_compact_in_tx(tx, input)
            .await
            .map_err(ExecutionError::Internal)
    }

    pub async fn begin_context_compact_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        compact_summary_id: &str,
    ) -> Result<bool, ExecutionError> {
        super::super::context::begin_compact_in_tx(tx, session_id, compact_summary_id)
            .await
            .map_err(ExecutionError::Internal)
    }

    pub async fn complete_context_compact_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        compact_summary_id: &str,
        estimated_input_tokens: i64,
    ) -> Result<bool, ExecutionError> {
        super::super::context::complete_compact_in_tx(
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
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT input_tokens, output_tokens FROM model_usage_ledger \
             WHERE turn_id = ? ORDER BY occurred_at, id",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(aggregate_turn_token_exchange(
            &rows,
            estimated_system_prompt_tokens(),
        ))
    }

    pub async fn latest_context_usage(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ContextUsageView>, ExecutionError> {
        let row: Option<(i64, i64, String, String)> = sqlx::query_as(
            "SELECT estimated_input_tokens, context_limit, compact_status, created_at \
             FROM context_versions WHERE session_id = ? ORDER BY sequence DESC LIMIT 1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(estimated_input_tokens, context_limit, compact_status, created_at)| {
                ContextUsageView {
                    estimated_input_tokens,
                    context_limit,
                    compact_status,
                    created_at,
                }
            },
        ))
    }

    pub async fn latest_model_attempt_for_turn(
        &self,
        turn_id: TurnId,
    ) -> Result<Option<TurnModelAttempt>, ExecutionError> {
        let round_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM rounds WHERE turn_id = ? ORDER BY sequence",
        )
        .bind(turn_id.to_string())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|id| id.parse::<RoundId>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
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
            attempt: attempt.attempt,
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
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        summary: &mut Value,
        now: &str,
    ) -> Result<(), ExecutionError> {
        if let Some(async_task_id) = summary.get("task_id").and_then(Value::as_str) {
            sqlx::query(
                "UPDATE tool_calls SET async_task_id = ? WHERE id = ? AND status = 'running'",
            )
            .bind(async_task_id)
            .bind(tool_call_id.to_string())
            .execute(&mut *tx)
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
        let sequence: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM plan_versions WHERE turn_id = ?",
        )
        .bind(turn_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO plan_versions \
             (id, turn_id, sequence, plan_json, evidence_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(plan_id)
        .bind(turn_id.to_string())
        .bind(sequence)
        .bind(todos.to_string())
        .bind(evidence.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if let Some(object) = summary.as_object_mut() {
            object.insert("plan_version_id".into(), Value::String(plan_id.into()));
            object.insert("sequence".into(), Value::from(sequence));
        }
        Ok(())
    }

    pub async fn delete_session_execution_in_tx(
        &self,
        tx: &mut SqliteConnection,
        session_id: SessionId,
        turn_ids: &[TurnId],
    ) -> Result<(), ExecutionError> {
        for turn_id in turn_ids {
            sqlx::query("DELETE FROM plan_versions WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM rounds WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM context_versions WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM compact_summaries WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(())
    }
}
