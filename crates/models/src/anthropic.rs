//! Anthropic Messages API streaming adapter (SSE).

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

use super::stream_types::{
    ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent, StreamChannel,
    TokenUsage, ToolCallDelta, append_reasoning_summary, truncated_stream_event,
};

pub fn build_messages_body(req: &ModelRequest) -> Value {
    let mut system = String::new();
    let mut messages: Vec<Value> = Vec::new();
    for msg in &req.messages {
        match msg.role {
            ChatRole::System => {
                for p in &msg.parts {
                    if let ContentPart::Text { text } = p {
                        if !system.is_empty() {
                            system.push('\n');
                        }
                        system.push_str(text);
                    }
                }
            }
            ChatRole::User | ChatRole::Assistant => {
                let role = if matches!(msg.role, ChatRole::User) {
                    "user"
                } else {
                    "assistant"
                };
                let mut content: Vec<Value> = msg
                    .parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => json!({"type": "text", "text": text}),
                        ContentPart::Image { mime, bytes, .. } => {
                            let b64 = B64.encode(bytes);
                            json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mime,
                                    "data": b64,
                                }
                            })
                        }
                    })
                    .collect();
                content.extend(msg.tool_calls.iter().map(|call| {
                    let input = serde_json::from_str::<Value>(&call.arguments_json)
                        .unwrap_or_else(|_| json!({}));
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input,
                    })
                }));
                messages.push(json!({"role": role, "content": content}));
            }
            ChatRole::Tool => {
                // Tool results as user content blocks in Anthropic Messages.
                let content: Vec<Value> = msg
                    .parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => json!({
                            "type": "text",
                            "text": text,
                        }),
                        ContentPart::Image { mime, bytes, .. } => json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mime,
                                "data": B64.encode(bytes),
                            }
                        }),
                    })
                    .collect();
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    }]
                }));
            }
        }
    }

    let mut body = json!({
        "model": req.upstream_model_id,
        "max_tokens": 4096,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }

    // Map reasoning_effort to Anthropic thinking + output_config
    if let Some(reasoning_effort) = req
        .parameters
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        && reasoning_effort != "none"
    {
        body["thinking"] = json!({"type": "adaptive"});
        body["output_config"] = json!({"effort": reasoning_effort});
    }

    body
}

#[derive(Default)]
pub struct AnthropicAssembler {
    pub text: String,
    pub reasoning: String,
    /// index -> (id, name, partial json)
    pub tool_args: Vec<(String, String, String)>,
    pub seq: u64,
    pub usage: Option<TokenUsage>,
    pub stop_reason: Option<String>,
    pub reasoning_duration_ms: Option<u64>,
    saw_terminal: bool,
}

impl AnthropicAssembler {
    pub fn ingest(
        &mut self,
        attempt_id: &str,
        event_name: &str,
        data: &str,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        if data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| format!("anthropic json: {e}"))?;
        let mut out = Vec::new();
        match event_name {
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(json!({}));
                let dtype = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if dtype == "text_delta" {
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        self.text.push_str(text);
                        self.seq += 1;
                        out.push(ModelStreamEvent::Delta {
                            attempt_id: attempt_id.to_owned(),
                            sequence: self.seq,
                            channel: StreamChannel::Text,
                            text: text.to_owned(),
                            provisional: true,
                            usage: None,
                        });
                    }
                } else if dtype == "thinking_delta" {
                    if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                        append_reasoning_summary(&mut self.reasoning, text);
                        self.seq += 1;
                        out.push(ModelStreamEvent::Delta {
                            attempt_id: attempt_id.to_owned(),
                            sequence: self.seq,
                            channel: StreamChannel::ReasoningSummary,
                            text: text.to_owned(),
                            provisional: true,
                            usage: None,
                        });
                    }
                } else if dtype == "input_json_delta" {
                    let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    let partial = delta
                        .get("partial_json")
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    if let Some(entry) = self.tool_args.get_mut(index) {
                        entry.2.push_str(partial);
                        self.seq += 1;
                        out.push(ModelStreamEvent::ToolCallDelta {
                            attempt_id: attempt_id.to_owned(),
                            sequence: self.seq,
                            delta: ToolCallDelta {
                                index: index as u32,
                                id: Some(entry.0.clone()),
                                name: Some(entry.1.clone()),
                                arguments_delta: Some(partial.to_owned()),
                            },
                            provisional: true,
                        });
                    }
                }
            }
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or(json!({}));
                let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if btype == "tool_use" {
                    let id = block
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let name = block
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    self.tool_args
                        .push((id.clone(), name.clone(), String::new()));
                    let index = self.tool_args.len() as u32 - 1;
                    self.seq += 1;
                    out.push(ModelStreamEvent::ToolCallDelta {
                        attempt_id: attempt_id.to_owned(),
                        sequence: self.seq,
                        delta: ToolCallDelta {
                            index,
                            id: Some(id),
                            name: Some(name),
                            arguments_delta: None,
                        },
                        provisional: true,
                    });
                }
                // "thinking" blocks are acknowledged but no initialization needed —
                // the reasoning accumulator is shared and will be populated by
                // subsequent "thinking_delta" events.
            }
            "message_delta" => {
                if let Some(usage) = v.get("usage") {
                    let out_tok = usage
                        .get("output_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    let input = self.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0);
                    let cache_tokens = self.usage.as_ref().map(|u| u.cache_tokens).unwrap_or(0);
                    let usage = TokenUsage {
                        input_tokens: input,
                        output_tokens: out_tok,
                        cache_tokens,
                    };
                    self.usage = Some(usage.clone());
                    // Emit a usage-only delta so the UI can show live output token count.
                    self.seq += 1;
                    out.push(ModelStreamEvent::Delta {
                        attempt_id: attempt_id.to_owned(),
                        sequence: self.seq,
                        channel: StreamChannel::Text,
                        text: String::new(),
                        provisional: true,
                        usage: Some(usage),
                    });
                }
                if let Some(sr) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str())
                {
                    self.stop_reason = Some(sr.to_owned());
                }
            }
            "message_stop" => {
                self.saw_terminal = true;
            }
            "message_start" => {
                if let Some(usage) = v.get("message").and_then(|m| m.get("usage")) {
                    let cache_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0)
                        .saturating_add(
                            usage
                                .get("cache_creation_input_tokens")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0),
                        );
                    let usage = TokenUsage {
                        input_tokens: usage
                            .get("input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                        output_tokens: usage
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                        cache_tokens,
                    };
                    self.usage = Some(usage.clone());
                    // Emit a usage-only delta so the UI can show baseline input token count.
                    self.seq += 1;
                    out.push(ModelStreamEvent::Delta {
                        attempt_id: attempt_id.to_owned(),
                        sequence: self.seq,
                        channel: StreamChannel::Text,
                        text: String::new(),
                        provisional: true,
                        usage: Some(usage),
                    });
                }
            }
            _ => {}
        }
        Ok(out)
    }

    pub fn finish(&self, attempt_id: &str) -> ModelStreamEvent {
        if !self.saw_terminal {
            return truncated_stream_event(attempt_id);
        }
        let tool_calls = self
            .tool_args
            .iter()
            .map(|(id, name, args)| CompletedToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments_json: if args.is_empty() {
                    "{}".into()
                } else {
                    args.clone()
                },
            })
            .collect();
        ModelStreamEvent::Completed {
            attempt_id: attempt_id.to_owned(),
            usage: self.usage.clone().unwrap_or(TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
            }),
            stop_reason: self.stop_reason.clone(),
            tool_calls,
            text: self.text.clone(),
            reasoning: self.reasoning.clone(),
            reasoning_duration_ms: self.reasoning_duration_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AnthropicAssembler;
    use crate::stream_types::ModelStreamEvent;

    #[test]
    fn eof_without_message_stop_is_failed() {
        let mut assembler = AnthropicAssembler::default();
        assembler
            .ingest(
                "attempt",
                "content_block_delta",
                r#"{"delta":{"type":"text_delta","text":"partial"}}"#,
            )
            .expect("valid event");

        assert!(matches!(
            assembler.finish("attempt"),
            ModelStreamEvent::Failed { ref code, .. } if code == "PROVIDER_STREAM_FAILED"
        ));
    }

    #[test]
    fn message_stop_is_required_for_completion() {
        let mut assembler = AnthropicAssembler::default();
        assembler
            .ingest("attempt", "message_stop", r#"{"type":"message_stop"}"#)
            .expect("valid terminal event");

        assert!(matches!(
            assembler.finish("attempt"),
            ModelStreamEvent::Completed { .. }
        ));
    }
}
