//! OpenAI Responses streaming adapter.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::stream_types::{
    ChatMessage, ChatRole, CompletedToolCall, ContentPart, ModelRequest, ModelStreamEvent,
    StreamChannel, TokenUsage, ToolCallDelta, ToolSpec, append_reasoning_summary,
    truncated_stream_event,
};

pub fn build_responses_body(req: &ModelRequest) -> Value {
    let mut input = Vec::new();
    for message in &req.messages {
        match message.role {
            ChatRole::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.as_deref().unwrap_or("unknown_call"),
                "output": message_text(message),
            })),
            ChatRole::Assistant if !message.tool_calls.is_empty() => {
                let text = message_text(message);
                if !text.is_empty() {
                    input.push(json!({"role": "assistant", "content": text}));
                }
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": openai_tool_name(&call.name),
                        "arguments": call.arguments_json,
                    }));
                }
            }
            _ => input.push(json!({
                "role": responses_role(&message.role),
                "content": responses_content(&message.parts),
            })),
        }
    }

    let mut body = json!({
        "model": req.upstream_model_id,
        "stream": true,
        "input": input,
    });
    if let Some(reasoning_effort) = req
        .parameters
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .filter(|value| *value != "none")
    {
        body["reasoning"] = json!({"effort": reasoning_effort});
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(req.tools.iter().map(response_tool).collect());
    }
    body
}

fn responses_role(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn responses_content(parts: &[ContentPart]) -> Value {
    if parts
        .iter()
        .all(|part| matches!(part, ContentPart::Text { .. }))
    {
        return Value::String(
            parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        );
    }
    Value::Array(
        parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => json!({"type": "input_text", "text": text}),
                ContentPart::Image { mime, bytes, .. } => {
                    use base64::{Engine, engine::general_purpose::STANDARD as B64};
                    json!({
                        "type": "input_image",
                        "image_url": format!("data:{mime};base64,{}", B64.encode(bytes)),
                    })
                }
            })
            .collect(),
    )
}

fn message_text(message: &ChatMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn response_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": openai_tool_name(&tool.name),
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

#[derive(Debug, Default)]
pub struct OpenaiResponsesAssembler {
    pub text: String,
    pub reasoning: String,
    pub seq: u64,
    pub usage: Option<TokenUsage>,
    pub reasoning_duration_ms: Option<u64>,
    pub failed: Option<(String, String)>,
    saw_terminal: bool,
    tools: Vec<ResponseToolCall>,
    tool_indexes: HashMap<String, usize>,
    tool_names: HashMap<String, String>,
}

#[derive(Debug, Default)]
struct ResponseToolCall {
    /// Canonical provider call id (`call_id`), falling back to the item id.
    id: String,
    name: String,
    arguments: String,
    /// Every alias the provider used for this call (`id`, `call_id`). Events
    /// can arrive keyed by either, so the aliases all resolve here.
    aliases: Vec<String>,
}

impl ResponseToolCall {
    fn alias(&self) -> &str {
        self.aliases.first().map(String::as_str).unwrap_or(&self.id)
    }
}

impl OpenaiResponsesAssembler {
    pub fn for_tools(tools: &[ToolSpec]) -> Self {
        Self {
            tool_names: tools
                .iter()
                .map(|tool| (openai_tool_name(&tool.name), tool.name.clone()))
                .collect(),
            ..Self::default()
        }
    }

    pub fn ingest_event(
        &mut self,
        attempt_id: &str,
        event_name: &str,
        data: &str,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        if data.trim() == "[DONE]" {
            self.saw_terminal = true;
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|error| format!("openai responses event json: {error}"))?;
        let event_name = if event_name.is_empty() {
            value.get("type").and_then(Value::as_str).unwrap_or("")
        } else {
            event_name
        };
        match event_name {
            "response.output_text.delta" => self.text_delta(attempt_id, &value),
            "response.reasoning_summary_text.delta" | "response.reasoning_summary_text.done" => {
                self.reasoning_delta(attempt_id, &value)
            }
            "response.function_call_arguments.delta" => {
                self.function_arguments_delta(attempt_id, &value)
            }
            "response.output_item.added" | "response.output_item.done" => {
                self.output_item(&value);
                Ok(Vec::new())
            }
            "response.completed" => {
                self.saw_terminal = true;
                if let Some(response) = value.get("response") {
                    self.update_usage(response);
                    self.output_items(response.get("output"));
                }
                Ok(Vec::new())
            }
            "response.failed" | "response.error" => {
                let detail = error_detail(&value);
                self.failed = Some(("PROVIDER_STREAM_FAILED".into(), detail.clone()));
                Ok(vec![ModelStreamEvent::Failed {
                    attempt_id: attempt_id.to_owned(),
                    code: "PROVIDER_STREAM_FAILED".into(),
                    detail,
                }])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn text_delta(
        &mut self,
        attempt_id: &str,
        value: &Value,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
        if delta.is_empty() {
            return Ok(Vec::new());
        }
        self.text.push_str(delta);
        self.seq += 1;
        Ok(vec![ModelStreamEvent::Delta {
            attempt_id: attempt_id.to_owned(),
            sequence: self.seq,
            channel: StreamChannel::Text,
            text: delta.to_owned(),
            provisional: true,
            usage: None,
        }])
    }

    fn reasoning_delta(
        &mut self,
        attempt_id: &str,
        value: &Value,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
        if delta.is_empty() {
            return Ok(Vec::new());
        }
        append_reasoning_summary(&mut self.reasoning, delta);
        self.seq += 1;
        Ok(vec![ModelStreamEvent::Delta {
            attempt_id: attempt_id.to_owned(),
            sequence: self.seq,
            channel: StreamChannel::ReasoningSummary,
            text: delta.to_owned(),
            provisional: true,
            usage: None,
        }])
    }

    fn function_arguments_delta(
        &mut self,
        attempt_id: &str,
        value: &Value,
    ) -> Result<Vec<ModelStreamEvent>, String> {
        let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
        let item_id = value.get("item_id").and_then(Value::as_str);
        let index = self.ensure_tool(item_id, None, None);
        self.tools[index].arguments.push_str(delta);
        self.seq += 1;
        Ok(vec![ModelStreamEvent::ToolCallDelta {
            attempt_id: attempt_id.to_owned(),
            sequence: self.seq,
            delta: ToolCallDelta {
                index: index as u32,
                id: Some(self.tools[index].alias().to_owned()),
                name: Some(self.tools[index].name.clone()),
                arguments_delta: (!delta.is_empty()).then(|| delta.to_owned()),
            },
            provisional: true,
        }])
    }

    fn output_item(&mut self, value: &Value) {
        self.merge_tool_item(value.get("item").unwrap_or(value));
    }

    fn output_items(&mut self, value: Option<&Value>) {
        if let Some(items) = value.and_then(Value::as_array) {
            for item in items {
                self.merge_tool_item(item);
            }
        }
    }

    fn merge_tool_item(&mut self, value: &Value) {
        if value.get("type").and_then(Value::as_str) != Some("function_call") {
            return;
        }
        let item_id = value.get("id").and_then(Value::as_str);
        let call_id = value.get("call_id").and_then(Value::as_str);
        let name = value.get("name").and_then(Value::as_str);
        let index = self.ensure_tool(item_id, call_id, name);
        // The canonical id is the call_id the API expects on function_call_output.
        if let Some(call_id) = call_id {
            self.tools[index].id = call_id.to_owned();
        }
        if let Some(arguments) = value.get("arguments").and_then(Value::as_str) {
            self.tools[index].arguments = arguments.to_owned();
        }
    }

    /// Resolve or create the tool slot for one provider function call. The
    /// Responses API identifies a call by both `id` (the item id, e.g.
    /// `fc_1`) and `call_id` (e.g. `call_1`), and different event shapes
    /// carry different subsets, so every non-empty alias is registered to the
    /// same slot. Calls with neither id get a per-slot synthesized id so
    /// multiple anonymous calls can never collide downstream (they used to
    /// all share `"call_unknown"` and violate the tool_calls unique index).
    fn ensure_tool(
        &mut self,
        item_id: Option<&str>,
        call_id: Option<&str>,
        name: Option<&str>,
    ) -> usize {
        let mut keys: Vec<String> = Vec::with_capacity(2);
        if let Some(item_id) = item_id.filter(|value| !value.is_empty()) {
            keys.push(item_id.to_owned());
        }
        if let Some(call_id) = call_id.filter(|value| !value.is_empty()) {
            keys.push(call_id.to_owned());
        }
        // Try every alias before creating a slot.
        for key in &keys {
            if let Some(index) = self.tool_indexes.get(key).copied() {
                // A later event can reveal the other alias; link it too so all
                // future events resolve to this slot regardless of shape.
                for other in keys.iter().filter(|candidate| candidate != &key) {
                    self.tool_indexes
                        .entry(other.clone())
                        .or_insert_with(|| index);
                    self.tools[index].aliases.push(other.clone());
                }
                if let Some(name) = name {
                    self.tools[index].name = name.to_owned();
                }
                return index;
            }
        }
        let id = call_id
            .or(item_id)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("call_{}", self.tools.len()));
        let aliases = keys.clone();
        let index = self.tools.len();
        self.tools.push(ResponseToolCall {
            id,
            name: name.unwrap_or("").to_owned(),
            arguments: String::new(),
            aliases,
        });
        for key in keys {
            self.tool_indexes.insert(key, index);
        }
        index
    }

    fn update_usage(&mut self, value: &Value) {
        let usage = value.get("usage").unwrap_or(value);
        let cache_tokens = usage
            .pointer("/input_tokens_details/cached_tokens")
            .or_else(|| usage.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(cache_tokens);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if usage.get("input_tokens").is_some() || usage.get("output_tokens").is_some() {
            self.usage = Some(TokenUsage {
                input_tokens,
                output_tokens,
                cache_tokens,
            });
        }
    }

    pub fn finish(&self, attempt_id: &str) -> ModelStreamEvent {
        if !self.saw_terminal {
            return truncated_stream_event(attempt_id);
        }
        // The provider can emit the same logical call through several event
        // shapes (item added + completed output list). Merge aliases into one
        // CompletedToolCall per canonical id so the durable tool_calls unique
        // index (round_id, provider_call_id) can never collide.
        let mut merged: Vec<CompletedToolCall> = Vec::new();
        for tool in self.tools.iter().filter(|tool| !tool.name.is_empty()) {
            let call = CompletedToolCall {
                id: tool.id.clone(),
                name: self
                    .tool_names
                    .get(&tool.name)
                    .cloned()
                    .unwrap_or_else(|| tool.name.clone()),
                arguments_json: if tool.arguments.is_empty() {
                    "{}".into()
                } else {
                    tool.arguments.clone()
                },
            };
            if let Some(existing) = merged.iter_mut().find(|candidate| candidate.id == call.id) {
                // Keep the longest arguments seen; shorter ones are prefixes
                // from partial argument streams.
                if call.arguments_json.len() > existing.arguments_json.len() {
                    existing.arguments_json = call.arguments_json;
                }
            } else {
                merged.push(call);
            }
        }
        ModelStreamEvent::Completed {
            attempt_id: attempt_id.to_owned(),
            usage: self.usage.clone().unwrap_or_default(),
            stop_reason: None,
            tool_calls: merged,
            text: self.text.clone(),
            reasoning: self.reasoning.clone(),
            reasoning_content: None,
            reasoning_duration_ms: self.reasoning_duration_ms,
        }
    }
}

fn error_detail(value: &Value) -> String {
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/response/error/message"))
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or("Responses stream failed")
        .to_owned()
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
    use super::{OpenaiResponsesAssembler, build_responses_body};
    use crate::stream_types::{ChatMessage, ChatRole, ContentPart, ModelRequest, ToolSpec};
    use serde_json::json;

    fn request() -> ModelRequest {
        ModelRequest {
            owner_id: "owner".into(),
            provider_id: "provider".into(),
            upstream_model_id: "gpt-test".into(),
            parameters: json!({"reasoning_effort": "low"}),
            messages: vec![ChatMessage {
                role: ChatRole::User,
                parts: vec![ContentPart::Text {
                    text: "hello".into(),
                }],
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning_content: None,
            }],
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

    #[test]
    fn responses_body_uses_input_and_function_tools() {
        let body = build_responses_body(&request());
        assert_eq!(body["input"][0]["content"], "hello");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert_eq!(body["reasoning"]["effort"], "low");
    }

    #[test]
    fn responses_events_assemble_text_reasoning_and_tools() {
        let mut assembler = OpenaiResponsesAssembler::for_tools(&request().tools);
        assembler
            .ingest_event(
                "attempt",
                "response.reasoning_summary_text.delta",
                r#"{"delta":"Summarizing the workspace state"}"#,
            )
            .expect("valid Responses SSE event");
        assembler
            .ingest_event(
                "attempt",
                "response.reasoning_summary_text.delta",
                r#"{"delta":"Detailing the relevant files"}"#,
            )
            .expect("valid Responses SSE event");
        assembler
            .ingest_event(
                "attempt",
                "response.output_text.delta",
                r#"{"delta":"done"}"#,
            )
            .expect("valid Responses SSE event");
        assembler
            .ingest_event(
                "attempt",
                "response.output_item.added",
                r#"{"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}"#,
            )
            .expect("valid Responses SSE event");
        assembler
            .ingest_event(
                "attempt",
                "response.function_call_arguments.delta",
                r#"{"item_id":"fc_1","delta":"{\"path\":\"README.md\"}"}"#,
            )
            .expect("valid Responses SSE event");
        assembler
            .ingest_event(
                "attempt",
                "response.completed",
                r#"{"type":"response.completed"}"#,
            )
            .expect("valid Responses SSE event");

        let completed = assembler.finish("attempt");
        match completed {
            crate::stream_types::ModelStreamEvent::Completed {
                text,
                reasoning,
                tool_calls,
                ..
            } => {
                assert_eq!(text, "done");
                assert_eq!(
                    reasoning,
                    "Summarizing the workspace state\nDetailing the relevant files"
                );
                assert_eq!(tool_calls[0].name, "read_file");
                assert_eq!(tool_calls[0].id, "call_1");
            }
            _ => panic!("expected completed response"),
        }
    }

    #[test]
    fn failed_response_does_not_finish_as_completed() {
        let mut assembler = OpenaiResponsesAssembler::default();
        let events = assembler
            .ingest_event(
                "attempt",
                "response.failed",
                r#"{"error":{"message":"invalid request"}}"#,
            )
            .expect("valid Responses SSE event");

        assert!(matches!(
            events.as_slice(),
            [crate::stream_types::ModelStreamEvent::Failed { .. }]
        ));
        assert!(assembler.failed.is_some());
    }

    #[test]
    fn response_usage_excludes_cached_input_tokens() {
        let mut assembler = OpenaiResponsesAssembler::default();
        assembler
            .ingest_event(
                "attempt",
                "response.completed",
                r#"{"response":{"usage":{"input_tokens":100,"output_tokens":8,"input_tokens_details":{"cached_tokens":60}}}}"#,
            )
            .expect("valid Responses SSE event");

        match assembler.finish("attempt") {
            crate::stream_types::ModelStreamEvent::Completed { usage, .. } => {
                assert_eq!(usage.input_tokens, 40);
                assert_eq!(usage.output_tokens, 8);
                assert_eq!(usage.cache_tokens, 60);
            }
            other => panic!("expected completed response, got {other:?}"),
        }
    }

    #[test]
    fn eof_without_response_completed_is_failed() {
        let mut assembler = OpenaiResponsesAssembler::default();
        assembler
            .ingest_event(
                "attempt",
                "response.output_text.delta",
                r#"{"delta":"partial"}"#,
            )
            .expect("valid event");

        assert!(matches!(
            assembler.finish("attempt"),
            crate::stream_types::ModelStreamEvent::Failed { ref code, .. }
                if code == "PROVIDER_STREAM_FAILED"
        ));
    }

    #[test]
    fn response_completed_is_required_for_completion() {
        let mut assembler = OpenaiResponsesAssembler::default();
        assembler
            .ingest_event(
                "attempt",
                "response.completed",
                r#"{"type":"response.completed"}"#,
            )
            .expect("valid terminal event");

        assert!(matches!(
            assembler.finish("attempt"),
            crate::stream_types::ModelStreamEvent::Completed { .. }
        ));
    }

    #[test]
    fn item_id_and_call_id_aliases_merge_into_one_call() {
        // Regression for the tool_calls UNIQUE(round_id, provider_call_id)
        // violation: arguments streamed by item_id must merge with the
        // completed output item identified by call_id instead of producing
        // two entries that share an id (or the "call_unknown" placeholder).
        let mut assembler = OpenaiResponsesAssembler::for_tools(&request().tools);
        assembler
            .ingest_event(
                "attempt",
                "response.output_item.added",
                r#"{"item":{"type":"function_call","id":"fc_1","name":"read_file"}}"#,
            )
            .expect("valid event");
        assembler
            .ingest_event(
                "attempt",
                "response.function_call_arguments.delta",
                r#"{"item_id":"fc_1","delta":"{\"path\":\"README.md\"}"}"#,
            )
            .expect("valid event");
        // The completed output arrives keyed by both aliases with the call_id
        // spelling — previously this created a second, duplicate slot.
        assembler
            .ingest_event(
                "attempt",
                "response.completed",
                r#"{"response":{"output":[{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"README.md\"}"}]}}"#,
            )
            .expect("valid event");

        match assembler.finish("attempt") {
            crate::stream_types::ModelStreamEvent::Completed { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1, "aliases must merge into one call");
                assert_eq!(tool_calls[0].id, "call_1");
                assert_eq!(tool_calls[0].name, "read_file");
            }
            other => panic!("expected completed response, got {other:?}"),
        }
    }

    #[test]
    fn anonymous_calls_get_unique_ids_and_duplicate_ids_merge() {
        // Two function calls that carry no ids at all must not collide on a
        // shared placeholder id, and two events describing the same id must
        // collapse into one CompletedToolCall.
        let mut assembler =
            OpenaiResponsesAssembler::for_tools(&[request().tools[0].clone()]);
        assembler
            .ingest_event(
                "attempt",
                "response.output_item.added",
                r#"{"item":{"type":"function_call","name":"read_file","arguments":"{}"}}"#,
            )
            .expect("valid event");
        assembler
            .ingest_event(
                "attempt",
                "response.output_item.added",
                r#"{"item":{"type":"function_call","name":"read_file","arguments":"{}"}}"#,
            )
            .expect("valid event");
        assembler
            .ingest_event(
                "attempt",
                "response.completed",
                r#"{"type":"response.completed"}"#,
            )
            .expect("valid event");

        match assembler.finish("attempt") {
            crate::stream_types::ModelStreamEvent::Completed { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 2, "anonymous calls must stay distinct");
                let ids: Vec<&str> = tool_calls.iter().map(|c| c.id.as_str()).collect();
                assert_eq!(ids.len(), 2);
                assert_ne!(ids[0], ids[1], "anonymous ids must be unique");
                assert!(ids.iter().all(|id| *id != "call_unknown"));
            }
            other => panic!("expected completed response, got {other:?}"),
        }
    }
}
