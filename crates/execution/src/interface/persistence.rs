//! Turn/job/ask persistence: loading queries and transactional writes.
use super::*;

impl ExecutionInterface {
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
        let rows = self.sessions.context_messages(session_id, turn_id).await?;
        let mut out = Vec::new();
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
            out.push(ChatMessage {
                role,
                parts: self
                    .message_parts(session_id, &row.body, include_images)
                    .await?,
                tool_call_id,
                tool_calls,
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
                Some("ask_answer") => Some("[ask answer] "),
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
        if let Some(job_id) = summary.get("job_id").and_then(Value::as_str) {
            sqlx::query("UPDATE tool_calls SET job_id = ? WHERE id = ? AND status = 'running'")
                .bind(job_id)
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
            sqlx::query("DELETE FROM asks WHERE turn_id = ?")
                .bind(turn_id.to_string())
                .execute(&mut *tx)
                .await?;
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

    pub async fn close_open_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
        closure: AskClosure,
        now: &str,
    ) -> Result<u64, ExecutionError> {
        self.close_asks_in_tx(tx, Some(turn_id), closure, now).await
    }

    pub(crate) async fn close_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: Option<TurnId>,
        closure: AskClosure,
        now: &str,
    ) -> Result<u64, ExecutionError> {
        let turn_id = turn_id.map(|value| value.to_string());
        let result = sqlx::query(
            "UPDATE asks SET status = ?, closure_reason = ?, version = ?, updated_at = ? \
             WHERE status = ? AND (? IS NULL OR turn_id = ?)",
        )
        .bind(closure.status().as_str())
        .bind(closure.reason())
        .bind(format!("v_{}", AskId::new()))
        .bind(now)
        .bind(AskStatus::Open.as_str())
        .bind(turn_id.as_deref())
        .bind(turn_id.as_deref())
        .execute(&mut *tx)
        .await?;
        Ok(result.rows_affected())
    }

    /// Create an Ask row inside a shared transaction. The Turn's move to
    /// `waiting_for_ask` is performed by `sessions::pause_turn_for` (sessions
    /// owns turns); the coordinator opens one tx, writes the Ask here, then
    /// pauses the Turn. Returns nothing — the caller already knows the ask id.
    pub async fn create_ask_in_tx(
        &self,
        tx: &mut sqlx::sqlite::SqliteConnection,
        request: &AskRequest,
        now: &str,
    ) -> Result<bool, ExecutionError> {
        let inserted = sqlx::query(
            "INSERT INTO asks \
             (id, turn_id, tool_call_id, mode, prompt_json, choices_json, default_json, \
              answer_json, status, expires_at, answered_at, version, created_at, updated_at) \
             SELECT ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, ?, ?, ? \
             WHERE EXISTS( \
                 SELECT 1 FROM tool_calls AS call \
                 JOIN rounds AS round ON round.id = call.round_id \
                 WHERE call.id = ? AND round.turn_id = ? \
                   AND call.status = 'waiting' \
             )",
        )
        .bind(request.id.to_string())
        .bind(request.turn_id.to_string())
        .bind(request.tool_call_id.to_string())
        .bind(request.mode.storage_str())
        .bind(request.prompt.to_string())
        .bind(request.choices.to_string())
        .bind(request.default.as_ref().map(Value::to_string))
        .bind(AskStatus::Open.as_str())
        .bind(request.expires_at.as_deref())
        .bind(format!("v_{}", AskId::new()))
        .bind(now)
        .bind(now)
        .bind(request.tool_call_id.to_string())
        .bind(request.turn_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(ExecutionError::Storage)?;
        Ok(inserted.rows_affected() == 1)
    }

    pub async fn answer_ask_in_tx(
        &self,
        tx: &mut SqliteConnection,
        ask_id: AskId,
        answer: &Value,
        now: &str,
    ) -> Result<AskAnswer, ExecutionError> {
        use sqlx::Row;
        let row = sqlx::query("SELECT turn_id, status FROM asks WHERE id = ?")
            .bind(ask_id.to_string())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ExecutionError::AskNotFound)?;
        let turn_id = row
            .try_get::<String, _>("turn_id")?
            .parse::<TurnId>()
            .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
        let stored_status = row.try_get::<String, _>("status")?;
        let status = AskStatus::try_from(stored_status.as_str()).map_err(|()| {
            ExecutionError::Internal(anyhow::anyhow!(
                "invalid Ask status in storage: {stored_status}"
            ))
        })?;
        if status != AskStatus::Open {
            return Ok(AskAnswer {
                ask_id,
                turn_id,
                disposition: if status == AskStatus::Answered {
                    AskAnswerDisposition::Duplicate
                } else {
                    AskAnswerDisposition::Late
                },
                tool_call: None,
            });
        }
        let result = sqlx::query(
            "UPDATE asks SET status = ?, answer_json = ?, answered_at = ?, updated_at = ?, \
                    version = ? WHERE id = ? AND status = ?",
        )
        .bind(AskStatus::Answered.as_str())
        .bind(answer.to_string())
        .bind(now)
        .bind(now)
        .bind(format!("v_{}", AskId::new()))
        .bind(ask_id.to_string())
        .bind(AskStatus::Open.as_str())
        .execute(&mut *tx)
        .await?;
        let disposition = if result.rows_affected() == 1 {
            AskAnswerDisposition::Accepted
        } else {
            let stored_status =
                sqlx::query_scalar::<_, String>("SELECT status FROM asks WHERE id = ?")
                    .bind(ask_id.to_string())
                    .fetch_one(&mut *tx)
                    .await?;
            let status = AskStatus::try_from(stored_status.as_str()).map_err(|()| {
                ExecutionError::Internal(anyhow::anyhow!(
                    "invalid Ask status in storage: {stored_status}"
                ))
            })?;
            if status == AskStatus::Answered {
                AskAnswerDisposition::Duplicate
            } else {
                AskAnswerDisposition::Late
            }
        };
        let tool_call = if disposition == AskAnswerDisposition::Accepted {
            Some(
                self.settle_ask_tool_call_in_tx(tx, ask_id, AskStatus::Answered, now)
                    .await?,
            )
        } else {
            None
        };
        Ok(AskAnswer {
            ask_id,
            turn_id,
            disposition,
            tool_call,
        })
    }

    async fn settle_ask_tool_call_in_tx(
        &self,
        tx: &mut SqliteConnection,
        ask_id: AskId,
        ask_status: AskStatus,
        now: &str,
    ) -> Result<ToolCallSettlement, ExecutionError> {
        let row: Option<SettledAskToolCallRow> = sqlx::query_as(
            "SELECT ask.turn_id, ask.tool_call_id, call.tool_name, call.provider_call_id, \
                    call.input_json, ask.answer_json \
             FROM asks AS ask \
             JOIN tool_calls AS call ON call.id = ask.tool_call_id \
             JOIN rounds AS round ON round.id = call.round_id \
             WHERE ask.id = ? AND ask.status = ? AND round.turn_id = ask.turn_id \
               AND call.status = 'waiting'",
        )
        .bind(ask_id.to_string())
        .bind(ask_status.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((
            source_turn_id,
            tool_call_id,
            tool_name,
            provider_call_id,
            input_json,
            answer_json,
        )) = row
        else {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "settled Ask has no matching waiting Tool Call"
            )));
        };
        let provider_call_id = provider_call_id.ok_or_else(|| {
            ExecutionError::Internal(anyhow::anyhow!(
                "waiting Ask Tool Call has no Provider call id"
            ))
        })?;
        let input = serde_json::from_str::<Value>(&input_json).unwrap_or_else(|_| json!({}));
        let answer = answer_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        let mode = match input.get("mode").and_then(Value::as_str) {
            Some("best_effort") | Some("nonblocking") | Some("non_blocking") => "non_blocking",
            Some(value) => value,
            None => "blocking",
        };
        let answer_text = format_ask_answer(&answer);
        let summary = json!({
            "ask_id": ask_id.to_string(),
            "mode": mode,
            "prompt": input.get("prompt").and_then(Value::as_str).unwrap_or_default(),
            "choices": input.get("choices").cloned().unwrap_or_else(|| json!([])),
            "multiple": input.get("multiple").and_then(Value::as_bool).unwrap_or(false),
            "answer": answer.clone(),
            "status": ask_status.as_str(),
        });
        let result_text = if answer.is_null() {
            format!(
                "ask_user {} (ask_id={ask_id}): no answer was provided",
                ask_status.as_str()
            )
        } else {
            format!(
                "ask_user {} (ask_id={ask_id}): {answer_text}",
                ask_status.as_str()
            )
        };
        let mut outcome = ToolOutcome {
            disposition: ToolExecutionDisposition::Succeeded,
            parts: vec![ToolResultPart::Text { text: result_text }],
            summary,
            error_code: None,
            finish_summary: None,
            wait: None,
        };
        attach_tool_display(&tool_name, &input, &mut outcome);
        let summary = outcome.summary.clone();
        let (_, model_parts) = tool_result_message(&outcome, &provider_call_id);
        let changed = sqlx::query(
            "UPDATE tool_calls SET status = ?, result_summary_json = ?, error_code = NULL, \
                    ended_at = ?, version = ? WHERE id = ? AND status = 'waiting'",
        )
        .bind(ToolCallStatus::Succeeded.as_str())
        .bind(summary.to_string())
        .bind(now)
        .bind(format!("v_{}", ToolCallId::new()))
        .bind(&tool_call_id)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() != 1 {
            return Err(ExecutionError::Internal(anyhow::anyhow!(
                "waiting Ask Tool Call changed during settlement"
            )));
        }
        Ok(ToolCallSettlement {
            tool_call_id,
            source_turn_id,
            provider_call_id,
            tool_name,
            status: ToolCallStatus::Succeeded,
            summary,
            model_parts,
        })
    }

    pub async fn has_open_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        turn_id: TurnId,
    ) -> Result<bool, ExecutionError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM asks \
             WHERE turn_id = ? AND status = ?",
        )
        .bind(turn_id.to_string())
        .bind(AskStatus::Open.as_str())
        .fetch_one(&mut *tx)
        .await?;
        Ok(count > 0)
    }

    pub async fn has_due_non_blocking_asks(&self, now: &str) -> Result<bool, ExecutionError> {
        let due: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM asks \
             WHERE status = ? AND mode = 'best_effort' \
               AND expires_at IS NOT NULL AND expires_at <= ?)",
        )
        .bind(AskStatus::Open.as_str())
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        Ok(due != 0)
    }

    pub async fn expire_due_asks_in_tx(
        &self,
        tx: &mut SqliteConnection,
        now: &str,
        limit: u32,
    ) -> Result<Vec<ExpiredAsk>, ExecutionError> {
        let rows = sqlx::query(
            "SELECT id, turn_id, default_json FROM asks \
             WHERE status = ? AND mode = 'best_effort' \
               AND expires_at IS NOT NULL AND expires_at <= ? \
             ORDER BY expires_at, id LIMIT ?",
        )
        .bind(AskStatus::Open.as_str())
        .bind(now)
        .bind(i64::from(limit.clamp(1, 500)))
        .fetch_all(&mut *tx)
        .await?;
        use sqlx::Row;
        let mut out = Vec::new();
        for row in rows {
            let ask_id = row
                .try_get::<String, _>("id")?
                .parse::<AskId>()
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
            let turn_id = row
                .try_get::<String, _>("turn_id")?
                .parse::<TurnId>()
                .map_err(|error| ExecutionError::Internal(anyhow::anyhow!(error)))?;
            let default = row
                .try_get::<Option<String>, _>("default_json")?
                .map(|value| serde_json::from_str::<Value>(&value))
                .transpose()?;
            let changed = sqlx::query(
                "UPDATE asks SET status = ?, answer_json = COALESCE(answer_json, ?), \
                        version = ?, updated_at = ? WHERE id = ? AND status = ?",
            )
            .bind(AskStatus::Expired.as_str())
            .bind(default.as_ref().map(Value::to_string))
            .bind(format!("v_{}", AskId::new()))
            .bind(now)
            .bind(ask_id.to_string())
            .bind(AskStatus::Open.as_str())
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() == 1 {
                let tool_call = self
                    .settle_ask_tool_call_in_tx(tx, ask_id, AskStatus::Expired, now)
                    .await?;
                out.push(ExpiredAsk {
                    ask_id,
                    turn_id,
                    default,
                    tool_call,
                });
            }
        }
        Ok(out)
    }
}

