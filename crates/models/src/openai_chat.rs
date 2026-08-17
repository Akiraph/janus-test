//! OpenAI Chat Completions streaming adapter (SSE).

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

use super::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    StreamChannel, TokenUsage, ToolCallDelta, ToolSpec, append_reasoning_summary,
    truncated_stream_event,
};

pub fn build_chat_body(req: &ModelRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(message_to_openai).collect();
    let mut body = json!({
        "model": req.upstream_model_id,
        "stream": true,
        "messages": messages,
    });
    if let Some(reasoning_effort) = req
        .parameters
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .filter(|value| *value != "none")
    {
        body["reasoning_effort"] = json!(reasoning_effort);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": openai_tool_name(&t.name),
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

fn message_to_openai(msg: &ChatMessage) -> Value {
    let role = match msg.role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    };
    // Prefer simple string content when all text; array for multimodal.
    let all_text = msg
        .parts
        .iter()
        .all(|p| matches!(p, ContentPart::Text { .. }));
    let mut v = if all_text {
        let text = msg
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        json!({"role": role, "content": text})
    } else {
        let content: Vec<Value> = msg
            .parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => json!({"type": "text", "text": text}),
                ContentPart::Image { mime, bytes, .. } => {
                    let b64 = B64.encode(bytes);
                    json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{mime};base64,{b64}")}
                    })
                }
            })
            .collect();
        json!({"role": role, "content": content})
    };
    if let Some(id) = &msg.tool_call_id {
        v["tool_call_id"] = json!(id);
    }
    if !msg.tool_calls.is_empty() {
        v["tool_calls"] = json!(
            msg.tool_calls
                .iter()
                .map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": openai_tool_name(&call.name),
                        "arguments": call.arguments_json,
                    },
                }))
                .collect::<Vec<_>>()
        );
    }
    // Thinking-mode providers require the assistant's previous reasoning to be
    // echoed back as `reasoning_content`; without it they reject the request
    // with "The `reasoning_content` in the thinking mode must be passed back".
    if matches!(msg.role, ChatRole::Assistant)
        && let Some(reasoning) = msg.reasoning_content.as_deref()
    {
        v["reasoning_content"] = json!(reasoning);
    }
    v
}

/// Mutable state while consuming OpenAI chat.completion.chunk SSE.
#[derive(Default)]
pub struct OpenaiChatAssembler {
    pub text: String,
    /// Display-formatted reasoning summary (newlines inserted between
    /// sentences). Never echoed back to the provider.
    pub reasoning: String,
    /// Raw reasoning deltas concatenated verbatim, for echo-back on the next
    /// request. Thinking-mode providers reject reformatted reasoning with 400.
    pub raw_reasoning: String,
    /// Whether the provider sent the `reasoning_content` field at least once.
    /// An empty field is still meaningful in thinking mode and must be echoed.
    saw_reasoning_content: bool,
    pub tool_args: Vec<(Option<String>, Option<String>, String)>, // id, name, args
    pub seq: u64,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<String>,
    pub reasoning_duration_ms: Option<u64>,
    saw_terminal: bool,
    tool_names: HashMap<String, String>,
}

impl OpenaiChatAssembler {
    pub fn for_tools(tools: &[ToolSpec]) -> Self {
        Self {
            tool_names: tools
                .iter()
                .map(|tool| (openai_tool_name(&tool.name), tool.name.clone()))
                .collect(),
            ..Self::default()
        }
    }

    pub fn ingest_data(
        &mut self,
        attempt_id: &str,
        data: &str,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        if data.trim() == "[DONE]" {
            self.saw_terminal = true;
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| format!("openai chunk json: {e}"))?;
        let mut out = Vec::new();

        if let Some(usage) = v.get("usage") {
            let cache_tokens = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .or_else(|| usage.get("cached_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let usage = TokenUsage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0)
                    .saturating_sub(cache_tokens),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                cache_tokens,
            };
            self.usage = Some(usage.clone());
            // Emit a usage-only delta so the UI can show live token counts.
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

        let choices = v
            .get("choices")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for choice in choices {
            if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str())
                && fr != "null"
            {
                self.finish_reason = Some(fr.to_owned());
            }
            let delta = choice.get("delta").cloned().unwrap_or(json!({}));
            if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
                self.saw_reasoning_content = true;
                if !reasoning.is_empty() {
                    append_reasoning_summary(&mut self.reasoning, reasoning);
                    self.raw_reasoning.push_str(reasoning);
                    self.seq += 1;
                    out.push(ModelStreamEvent::Delta {
                        attempt_id: attempt_id.to_owned(),
                        sequence: self.seq,
                        channel: StreamChannel::ReasoningSummary,
                        text: reasoning.to_owned(),
                        provisional: true,
                        usage: None,
                    });
                }
            }
            if let Some(content) = delta.get("content").and_then(|c| c.as_str())
                && !content.is_empty()
            {
                self.text.push_str(content);
                self.seq += 1;
                out.push(ModelStreamEvent::Delta {
                    attempt_id: attempt_id.to_owned(),
                    sequence: self.seq,
                    channel: StreamChannel::Text,
                    text: content.to_owned(),
                    provisional: true,
                    usage: None,
                });
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    while self.tool_args.len() <= index {
                        self.tool_args.push((None, None, String::new()));
                    }
                    let entry = &mut self.tool_args[index];
                    if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                        entry.0 = Some(id.to_owned());
                    }
                    if let Some(name) = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                    {
                        entry.1 = Some(name.to_owned());
                    }
                    let args_delta = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("");
                    if !args_delta.is_empty() {
                        entry.2.push_str(args_delta);
                    }
                    self.seq += 1;
                    out.push(ModelStreamEvent::ToolCallDelta {
                        attempt_id: attempt_id.to_owned(),
                        sequence: self.seq,
                        delta: ToolCallDelta {
                            index: index as u32,
                            id: entry.0.clone(),
                            name: entry.1.clone(),
                            arguments_delta: if args_delta.is_empty() {
                                None
                            } else {
                                Some(args_delta.to_owned())
                            },
                        },
                        provisional: true,
                    });
                }
            }
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
            .enumerate()
            .filter_map(|(i, (id, name, args))| {
                let wire_name = name.as_ref()?;
                let name = self
                    .tool_names
                    .get(wire_name)
                    .cloned()
                    .unwrap_or_else(|| wire_name.clone());
                Some(CompletedToolCall {
                    id: id.clone().unwrap_or_else(|| format!("call_{i}")),
                    name,
                    arguments_json: if args.is_empty() {
                        "{}".into()
                    } else {
                        args.clone()
                    },
                })
            })
            .collect();
        ModelStreamEvent::Completed {
            attempt_id: attempt_id.to_owned(),
            usage: self.usage.clone().unwrap_or(TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_tokens: 0,
            }),
            stop_reason: self.finish_reason.clone(),
            tool_calls,
            text: self.text.clone(),
            reasoning: self.reasoning.clone(),
            reasoning_content: self
                .saw_reasoning_content
                .then(|| self.raw_reasoning.clone()),
            reasoning_duration_ms: self.reasoning_duration_ms,
        }
    }
}

fn openai_tool_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{OpenaiChatAssembler, build_chat_body};
    use crate::stream_types::{
        ChatMessage, ChatRole, ContentPart, ModelRequest, ModelStreamEvent, ToolSpec,
    };
    use serde_json::json;

    #[test]
    fn eof_without_done_is_failed() {
        let mut assembler = OpenaiChatAssembler::default();
        assembler
            .ingest_data(
                "attempt",
                r#"{"choices":[{"delta":{"content":"partial"},"index":0}]}"#,
            )
            .expect("valid event");

        assert!(matches!(
            assembler.finish("attempt"),
            ModelStreamEvent::Failed { ref code, .. } if code == "PROVIDER_STREAM_FAILED"
        ));
    }

    #[test]
    fn done_is_required_for_completion() {
        let mut assembler = OpenaiChatAssembler::default();
        assembler
            .ingest_data("attempt", "[DONE]")
            .expect("valid terminal sentinel");

        assert!(matches!(
            assembler.finish("attempt"),
            ModelStreamEvent::Completed { .. }
        ));
    }

    fn request(messages: Vec<ChatMessage>) -> ModelRequest {
        ModelRequest {
            owner_id: "owner".into(),
            provider_id: "provider".into(),
            upstream_model_id: "deepseek-reasoner".into(),
            parameters: json!({"reasoning_effort": "high"}),
            messages,
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            round_id: None,
            project_id: None,
            session_id: None,
            turn_id: None,
        }
    }

    fn assistant_message(content: &str, reasoning: Option<&str>) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            parts: vec![ContentPart::Text {
                text: content.into(),
            }],
            tool_call_id: None,
            tool_calls: Vec::new(),
            reasoning_content: reasoning.map(str::to_owned),
        }
    }

    #[test]
    fn thinking_reasoning_is_echoed_back_as_reasoning_content() {
        let body = build_chat_body(&request(vec![
            assistant_message(
                "I checked the config.",
                Some("Summarizing the workspace state\nDetailing relevant files"),
            ),
            ChatMessage {
                role: ChatRole::User,
                parts: vec![ContentPart::Text {
                    text: "proceed".into(),
                }],
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            },
        ]));
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(
            messages[0]["reasoning_content"],
            "Summarizing the workspace state\nDetailing relevant files"
        );
        // Non-assistant / reasoning-less messages never carry the field.
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn reasoning_content_is_omitted_when_absent() {
        let body = build_chat_body(&request(vec![assistant_message("plain answer", None)]));
        let messages = body["messages"].as_array().expect("messages array");
        assert!(messages[0].get("reasoning_content").is_none());
        assert_eq!(messages[0]["content"], "plain answer");
    }

    #[test]
    fn empty_reasoning_content_is_echoed_when_explicitly_present() {
        let body = build_chat_body(&request(vec![assistant_message("answer", Some(""))]));
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages[0]["reasoning_content"], "");
    }

    #[test]
    fn completed_carries_verbatim_raw_reasoning_separate_from_display_summary() {
        let mut assembler = OpenaiChatAssembler::default();
        assembler
            .ingest_data(
                "attempt",
                r#"{"choices":[{"delta":{"reasoning_content":"Summarizing the workspace state"}}]}"#,
            )
            .expect("valid event");
        assembler
            .ingest_data(
                "attempt",
                r#"{"choices":[{"delta":{"reasoning_content":"Detailing relevant files"}}]}"#,
            )
            .expect("valid event");
        assembler
            .ingest_data("attempt", "[DONE]")
            .expect("valid terminal sentinel");
        let event = assembler.finish("attempt");
        let ModelStreamEvent::Completed {
            reasoning,
            reasoning_content,
            ..
        } = event
        else {
            panic!("expected Completed");
        };
        // Display summary inserts a sentence boundary newline...
        assert_eq!(
            reasoning,
            "Summarizing the workspace state\nDetailing relevant files"
        );
        // ...but the raw content is concatenated verbatim for echo-back.
        assert_eq!(
            reasoning_content.as_deref(),
            Some("Summarizing the workspace stateDetailing relevant files")
        );
    }

    #[test]
    fn completed_omits_reasoning_content_when_provider_sends_none() {
        let mut assembler = OpenaiChatAssembler::default();
        assembler
            .ingest_data(
                "attempt",
                r#"{"choices":[{"delta":{"content":"plain answer"}}]}"#,
            )
            .expect("valid event");
        assembler
            .ingest_data("attempt", "[DONE]")
            .expect("valid terminal sentinel");
        let ModelStreamEvent::Completed {
            reasoning_content, ..
        } = assembler.finish("attempt")
        else {
            panic!("expected Completed");
        };
        assert_eq!(reasoning_content, None);
    }

    #[test]
    fn empty_provider_reasoning_content_is_preserved_for_echo_back() {
        let mut assembler = OpenaiChatAssembler::default();
        assembler
            .ingest_data(
                "attempt",
                r#"{"choices":[{"delta":{"reasoning_content":""}}]}"#,
            )
            .expect("valid empty reasoning delta");
        assembler
            .ingest_data("attempt", "[DONE]")
            .expect("valid terminal sentinel");
        let ModelStreamEvent::Completed {
            reasoning_content, ..
        } = assembler.finish("attempt")
        else {
            panic!("expected Completed");
        };
        let body = build_chat_body(&request(vec![assistant_message(
            "tool result",
            reasoning_content.as_deref(),
        )]));
        assert_eq!(reasoning_content.as_deref(), Some(""));
        assert_eq!(body["messages"][0]["reasoning_content"], "");
    }
}
