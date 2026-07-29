//! OpenAI Chat Completions streaming adapter (SSE).

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};

use super::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    StreamChannel, TokenUsage, ToolCallDelta,
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
                        "name": t.name,
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
    if all_text {
        let text = msg
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let mut v = json!({"role": role, "content": text});
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
                            "name": call.name,
                            "arguments": call.arguments_json,
                        },
                    }))
                    .collect::<Vec<_>>()
            );
        }
        return v;
    }
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
    let mut value = json!({"role": role, "content": content});
    if let Some(id) = &msg.tool_call_id {
        value["tool_call_id"] = json!(id);
    }
    if !msg.tool_calls.is_empty() {
        value["tool_calls"] = json!(
            msg.tool_calls
                .iter()
                .map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments_json,
                    },
                }))
                .collect::<Vec<_>>()
        );
    }
    value
}

/// Mutable state while consuming OpenAI chat.completion.chunk SSE.
#[derive(Default)]
pub struct OpenaiChatAssembler {
    pub text: String,
    pub tool_args: Vec<(Option<String>, Option<String>, String)>, // id, name, args
    pub seq: u64,
    pub usage: Option<TokenUsage>,
    pub finish_reason: Option<String>,
}

impl OpenaiChatAssembler {
    pub fn ingest_data(
        &mut self,
        attempt_id: &str,
        data: &str,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        if data.trim() == "[DONE]" {
            return Ok(Vec::new());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| format!("openai chunk json: {e}"))?;
        let mut out = Vec::new();

        if let Some(usage) = v.get("usage") {
            self.usage = Some(TokenUsage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
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
        let tool_calls = self
            .tool_args
            .iter()
            .enumerate()
            .filter_map(|(i, (id, name, args))| {
                let name = name.clone()?;
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
            }),
            stop_reason: self.finish_reason.clone(),
            tool_calls,
            text: self.text.clone(),
        }
    }
}
