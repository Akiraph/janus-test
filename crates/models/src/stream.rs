//! Stream completion entry point and attempt/usage ledger writes.

use std::future::Future;

use futures_util::StreamExt;
use janus_infrastructure::clock::now_utc_str;

use super::anthropic::{AnthropicAssembler, build_messages_body};
use super::interface::{AttemptFinalization, ModelsError, ModelsInterface, ProviderKind};
use super::openai_chat::{OpenaiChatAssembler, build_chat_body};
use super::openai_responses::{OpenaiResponsesAssembler, build_responses_body};
use super::sse::SseParser;
use super::stream_types::{ModelRequest, ModelStreamEvent, StreamTiming};
use janus_infrastructure::{id::AttemptId, secrets::Secret};

const MAX_PROVIDER_ERROR_BYTES: usize = 8 * 1024;

fn key_aad(owner_id: &str, id: &str) -> String {
    format!("v1/{owner_id}/model_providers/{id}/api_key")
}

fn append_path_segment(base: &str, segment: &str) -> Result<String, ModelsError> {
    let base = base.trim_end_matches('/');
    Ok(format!("{base}/{segment}"))
}

impl ModelsInterface {
    /// Run one Provider stream attempt. Yields provisional deltas then Completed or Failed.
    /// Failed attempts never produce a Completed event (no formal output commit).
    pub async fn stream_completion(
        &self,
        req: ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError> {
        let mut ignore_event = |_| std::future::ready(());
        self.stream_completion_with(req, &mut ignore_event).await
    }

    pub async fn stream_completion_with<F, Fut>(
        &self,
        req: ModelRequest,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.stream_completion_with_candidate(req, 0, on_event)
            .await
    }

    pub async fn stream_completion_with_candidate<F, Fut>(
        &self,
        req: ModelRequest,
        candidate_order: i64,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let row = self
            .provider_row_public(&req.owner_id, &req.provider_id)
            .await?;
        if row.enabled == 0 {
            return Err(ModelsError::Validation("provider is disabled".into()));
        }
        let kind = parse_kind_pub(&row.kind)?;
        let key = row
            .api_key_ciphertext
            .as_deref()
            .map(|stored| {
                self.cipher_ref()
                    .decrypt(stored, &key_aad(&req.owner_id, &req.provider_id))
            })
            .transpose()
            .map_err(ModelsError::Internal)?;

        let attempt_id = AttemptId::new().to_string();
        let started = now_utc_str();
        self.insert_attempt_running(
            &attempt_id,
            req.round_id.as_deref().unwrap_or("round-unset"),
            &req.provider_id,
            &req.upstream_model_id,
            candidate_order,
            &started,
        )
        .await?;

        let result = match kind {
            ProviderKind::OpenaiChat => {
                self.stream_openai_chat(&req, &row.base_url, key.as_ref(), &attempt_id, on_event)
                    .await
            }
            ProviderKind::OpenaiResponses => {
                self.stream_openai_responses(
                    &req,
                    &row.base_url,
                    key.as_ref(),
                    &attempt_id,
                    on_event,
                )
                .await
            }
            ProviderKind::Anthropic => {
                self.stream_anthropic(&req, &row.base_url, key.as_ref(), &attempt_id, on_event)
                    .await
            }
        };

        match result {
            Ok(events) => {
                let (status, usage, err_json) = match events.last() {
                    Some(ModelStreamEvent::Completed { usage, .. }) => {
                        ("succeeded", Some(usage.clone()), None)
                    }
                    Some(ModelStreamEvent::Failed { code, detail, .. }) => (
                        "failed",
                        None,
                        Some(serde_json::json!({"code": code, "detail": detail})),
                    ),
                    _ => (
                        "failed",
                        None,
                        Some(
                            serde_json::json!({"code": "PROVIDER_STREAM_FAILED", "detail": "stream ended without completion"}),
                        ),
                    ),
                };
                self.finalize_attempt(
                    &attempt_id,
                    AttemptFinalization {
                        status,
                        input_tokens: usage.as_ref().map(|u| u.input_tokens as i64),
                        output_tokens: usage.as_ref().map(|u| u.output_tokens as i64),
                        cache_tokens: usage.as_ref().map(|u| u.cache_tokens as i64),
                        error_json: err_json.as_ref(),
                        request: &req,
                    },
                )
                .await?;
                Ok(events)
            }
            Err(e) => {
                let detail = stream_error_detail(&e);
                // Never log secrets — detail comes from our mapping, not raw headers.
                let failed = ModelStreamEvent::Failed {
                    attempt_id: attempt_id.clone(),
                    code: "PROVIDER_STREAM_FAILED".into(),
                    detail: detail.clone(),
                };
                on_event(failed.clone()).await;
                self.finalize_attempt(
                    &attempt_id,
                    AttemptFinalization {
                        status: "failed",
                        input_tokens: None,
                        output_tokens: None,
                        cache_tokens: None,
                        error_json: Some(&serde_json::json!({
                            "code": "PROVIDER_STREAM_FAILED",
                            "detail": detail
                        })),
                        request: &req,
                    },
                )
                .await?;
                Ok(vec![failed])
            }
        }
    }

    async fn stream_openai_chat<F, Fut>(
        &self,
        req: &ModelRequest,
        base_url: &str,
        key: Option<&Secret>,
        attempt_id: &str,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let url = append_path_segment(base_url, "chat/completions")?;
        let body = build_chat_body(req);
        let mut request = self
            .client_ref()
            .post(url)
            .header("accept", "text/event-stream")
            .json(&body);
        if let Some(key) = key {
            request = request.bearer_auth(key.expose());
        }
        let response = request.send().await.map_err(|e| {
            ModelsError::Internal(anyhow::anyhow!(
                "provider unreachable: {}",
                classify_reqwest(&e)
            ))
        })?;
        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            // Surface the upstream's own error body (sanitized) instead of a
            // hard-coded string, so the user sees why the call was rejected.
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_AUTH_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }
        if !status.is_success() {
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_STREAM_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }

        let mut parser = SseParser::new();
        let mut assembler = OpenaiChatAssembler::for_tools(&req.tools);
        let mut timing = StreamTiming::default();
        let mut events = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                ModelsError::Internal(anyhow::anyhow!("stream read: {}", classify_reqwest(&e)))
            })?;
            for (_ev, data) in parser.push(&chunk) {
                let more = assembler
                    .ingest_data(attempt_id, &data)
                    .map_err(|e| ModelsError::Internal(anyhow::anyhow!(e)))?;
                for event in more {
                    timing.observe(&event);
                    on_event(event.clone()).await;
                    events.push(event);
                }
            }
        }
        assembler.reasoning_duration_ms = timing.reasoning_duration_ms();
        let completed = assembler.finish(attempt_id);
        on_event(completed.clone()).await;
        events.push(completed);
        Ok(events)
    }

    async fn stream_openai_responses<F, Fut>(
        &self,
        req: &ModelRequest,
        base_url: &str,
        key: Option<&Secret>,
        attempt_id: &str,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let url = append_path_segment(base_url, "responses")?;
        let body = build_responses_body(req);
        let mut request = self
            .client_ref()
            .post(url)
            .header("accept", "text/event-stream")
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = key {
            request = request.bearer_auth(key.expose());
        }
        let response = request.send().await.map_err(|e| {
            ModelsError::Internal(anyhow::anyhow!(
                "provider unreachable: {}",
                classify_reqwest(&e)
            ))
        })?;
        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_AUTH_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }
        if !status.is_success() {
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_STREAM_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }

        let mut parser = SseParser::new();
        let mut assembler = OpenaiResponsesAssembler::for_tools(&req.tools);
        let mut timing = StreamTiming::default();
        let mut events = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                ModelsError::Internal(anyhow::anyhow!("stream read: {}", classify_reqwest(&e)))
            })?;
            for (event_name, data) in parser.push(&chunk) {
                let more = assembler
                    .ingest_event(attempt_id, &event_name, &data)
                    .map_err(|e| ModelsError::Internal(anyhow::anyhow!(e)))?;
                for event in more {
                    timing.observe(&event);
                    on_event(event.clone()).await;
                    events.push(event);
                }
            }
        }
        if assembler.failed.is_some() {
            return Ok(events);
        }
        assembler.reasoning_duration_ms = timing.reasoning_duration_ms();
        let completed = assembler.finish(attempt_id);
        on_event(completed.clone()).await;
        events.push(completed);
        Ok(events)
    }

    async fn stream_anthropic<F, Fut>(
        &self,
        req: &ModelRequest,
        base_url: &str,
        key: Option<&Secret>,
        attempt_id: &str,
        on_event: &mut F,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError>
    where
        F: FnMut(ModelStreamEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let url = append_path_segment(base_url, "v1/messages")?;
        // base_url for Anthropic often already ends with /v1 — handle both.
        let url = if base_url.trim_end_matches('/').ends_with("/v1") {
            append_path_segment(base_url, "messages")?
        } else {
            url
        };
        let body = build_messages_body(req);
        let mut request = self
            .client_ref()
            .post(url)
            .header("accept", "text/event-stream")
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = key {
            request = request.header("x-api-key", key.expose());
        }
        let response = request.send().await.map_err(|e| {
            ModelsError::Internal(anyhow::anyhow!(
                "provider unreachable: {}",
                classify_reqwest(&e)
            ))
        })?;
        let status = response.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            // Surface the upstream's own error body (sanitized) instead of a
            // hard-coded string, so the user sees why the call was rejected.
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_AUTH_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }
        if !status.is_success() {
            let detail = provider_http_error(response, status.as_u16(), key).await;
            let failed = ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_STREAM_FAILED".into(),
                detail,
            };
            on_event(failed.clone()).await;
            return Ok(vec![failed]);
        }

        let mut parser = SseParser::new();
        let mut assembler = AnthropicAssembler::default();
        let mut timing = StreamTiming::default();
        let mut events = Vec::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                ModelsError::Internal(anyhow::anyhow!("stream read: {}", classify_reqwest(&e)))
            })?;
            for (ev, data) in parser.push(&chunk) {
                let more = assembler
                    .ingest(attempt_id, &ev, &data)
                    .map_err(|e| ModelsError::Internal(anyhow::anyhow!(e)))?;
                for event in more {
                    timing.observe(&event);
                    on_event(event.clone()).await;
                    events.push(event);
                }
            }
        }
        assembler.reasoning_duration_ms = timing.reasoning_duration_ms();
        let completed = assembler.finish(attempt_id);
        on_event(completed.clone()).await;
        events.push(completed);
        Ok(events)
    }
}

async fn provider_http_error(
    response: reqwest::Response,
    status: u16,
    key: Option<&Secret>,
) -> String {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = MAX_PROVIDER_ERROR_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let message = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|body| {
            body.pointer("/error/message")
                .or_else(|| body.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(|message| sanitize_provider_message(message, key))
        })
        .filter(|message| !message.is_empty());
    match message {
        Some(message) => format!("provider HTTP {status}: {message}"),
        None => format!("provider HTTP {status}"),
    }
}

fn sanitize_provider_message(message: &str, key: Option<&Secret>) -> String {
    let message = message
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .take(512)
        .collect::<String>()
        .trim()
        .to_owned();
    match key.map(Secret::expose).filter(|key| !key.is_empty()) {
        Some(key) => message.replace(key, "[redacted]"),
        None => message,
    }
}

fn classify_reqwest(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "timeout".into()
    } else if err.is_connect() {
        "connect".into()
    } else {
        "transport".into()
    }
}

fn stream_error_detail(error: &ModelsError) -> String {
    match error {
        ModelsError::Internal(source) => format!("{source:#}"),
        _ => error.to_string(),
    }
}

fn parse_kind_pub(value: &str) -> Result<ProviderKind, ModelsError> {
    match value {
        "anthropic" => Ok(ProviderKind::Anthropic),
        "openai_chat" => Ok(ProviderKind::OpenaiChat),
        "openai_responses" => Ok(ProviderKind::OpenaiResponses),
        _ => Err(ModelsError::Validation("unknown provider kind".into())),
    }
}
