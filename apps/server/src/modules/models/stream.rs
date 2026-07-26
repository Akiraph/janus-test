//! Stream completion entry + attempt/usage ledger (M3 Stage 3).

use chrono::Utc;
use futures_util::StreamExt;

use crate::platform::{clock::format_utc, id::AttemptId, secret::Secret};

use super::anthropic::{AnthropicAssembler, build_messages_body};
use super::interface::{ModelsError, ModelsInterface, ProviderKind};
use super::openai_chat::{OpenaiChatAssembler, build_chat_body};
use super::sse::SseParser;
use super::stream_types::{ModelRequest, ModelStreamEvent};

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
        let started = format_utc(Utc::now());
        self.insert_attempt_running(
            &attempt_id,
            req.round_id.as_deref().unwrap_or("round-unset"),
            &req.provider_id,
            &req.upstream_model_id,
            &started,
        )
        .await?;

        let result = match kind {
            ProviderKind::OpenaiChat | ProviderKind::OpenaiResponses => {
                self.stream_openai_chat(&req, &row.base_url, key.as_ref(), &attempt_id)
                    .await
            }
            ProviderKind::Anthropic => {
                self.stream_anthropic(&req, &row.base_url, key.as_ref(), &attempt_id)
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
                    status,
                    usage.as_ref().map(|u| u.input_tokens as i64),
                    usage.as_ref().map(|u| u.output_tokens as i64),
                    err_json.as_ref(),
                    &req,
                )
                .await?;
                Ok(events)
            }
            Err(e) => {
                let detail = e.to_string();
                // Never log secrets — detail comes from our mapping, not raw headers.
                let failed = ModelStreamEvent::Failed {
                    attempt_id: attempt_id.clone(),
                    code: "PROVIDER_STREAM_FAILED".into(),
                    detail: detail.clone(),
                };
                self.finalize_attempt(
                    &attempt_id,
                    "failed",
                    None,
                    None,
                    Some(&serde_json::json!({"code": "PROVIDER_STREAM_FAILED", "detail": detail})),
                    &req,
                )
                .await?;
                Ok(vec![failed])
            }
        }
    }

    async fn stream_openai_chat(
        &self,
        req: &ModelRequest,
        base_url: &str,
        key: Option<&Secret>,
        attempt_id: &str,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError> {
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
            return Ok(vec![ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_AUTH_FAILED".into(),
                detail: "provider rejected credentials".into(),
            }]);
        }
        if !status.is_success() {
            return Ok(vec![ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_STREAM_FAILED".into(),
                detail: format!("provider HTTP {}", status.as_u16()),
            }]);
        }

        let mut parser = SseParser::new();
        let mut assembler = OpenaiChatAssembler::default();
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
                events.extend(more);
            }
        }
        events.push(assembler.finish(attempt_id));
        Ok(events)
    }

    async fn stream_anthropic(
        &self,
        req: &ModelRequest,
        base_url: &str,
        key: Option<&Secret>,
        attempt_id: &str,
    ) -> Result<Vec<ModelStreamEvent>, ModelsError> {
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
            return Ok(vec![ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_AUTH_FAILED".into(),
                detail: "provider rejected credentials".into(),
            }]);
        }
        if !status.is_success() {
            return Ok(vec![ModelStreamEvent::Failed {
                attempt_id: attempt_id.to_owned(),
                code: "PROVIDER_STREAM_FAILED".into(),
                detail: format!("provider HTTP {}", status.as_u16()),
            }]);
        }

        let mut parser = SseParser::new();
        let mut assembler = AnthropicAssembler::default();
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
                events.extend(more);
            }
        }
        events.push(assembler.finish(attempt_id));
        Ok(events)
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

fn parse_kind_pub(value: &str) -> Result<ProviderKind, ModelsError> {
    match value {
        "anthropic" => Ok(ProviderKind::Anthropic),
        "openai_chat" => Ok(ProviderKind::OpenaiChat),
        "openai_responses" => Ok(ProviderKind::OpenaiResponses),
        _ => Err(ModelsError::Validation("unknown provider kind".into())),
    }
}
