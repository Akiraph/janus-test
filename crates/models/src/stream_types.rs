//! Normalized model streaming request and response types.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One content part in a model message. Image bytes are held only for the
/// duration of a Provider request (transport encoding); they are never
/// persisted as Base64 in Janus history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// In-memory image payload for the current attempt only.
    Image {
        mime: String,
        /// Raw image bytes (not Base64). Adapter may encode for wire format.
        #[serde(skip)]
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub parts: Vec<ContentPart>,
    /// Optional tool call id when role is Tool.
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<CompletedToolCall>,
    /// Assistant reasoning from the provider. Thinking-mode providers
    /// (OpenAI-compatible `reasoning_content`) require this to be echoed back
    /// verbatim on the next request; dropping it makes the provider reject the
    /// conversation with an HTTP 400. Populated only for assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub owner_id: String,
    pub provider_id: String,
    pub upstream_model_id: String,
    pub parameters: serde_json::Value,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    /// Correlation for attempt ledger (optional for pure stream tests).
    pub round_id: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamChannel {
    Text,
    ReasoningSummary,
    ToolCallPreview,
}

/// Join provider reasoning-summary deltas without relying on provider or API
/// kind. Some upstreams emit each summary sentence as a separate delta and
/// omit the whitespace that was present in the original summary.
pub fn append_reasoning_summary(previous: &mut String, delta: &str) {
    let delta_starts_without_whitespace = delta
        .chars()
        .next()
        .is_some_and(|character| !character.is_whitespace());
    let previous_ends_without_whitespace = previous
        .chars()
        .last()
        .is_some_and(|character| !character.is_whitespace());
    let looks_like_new_summary_sentence = delta.len() >= 20
        && delta
            .split_whitespace()
            .next()
            .is_some_and(|word| word.chars().next().is_some_and(|ch| ch.is_uppercase()));

    if !previous.is_empty()
        && delta_starts_without_whitespace
        && previous_ends_without_whitespace
        && looks_like_new_summary_sentence
    {
        previous.push('\n');
    }
    previous.push_str(delta);
}

/// Measures the model reasoning interval independently from answer/tool
/// output. Provider adapters observe deltas before awaiting event persistence.
#[derive(Debug, Default)]
pub struct StreamTiming {
    reasoning_started_at: Option<Instant>,
    output_started_at: Option<Instant>,
}

impl StreamTiming {
    pub fn observe(&mut self, event: &ModelStreamEvent) {
        let now = Instant::now();
        match event {
            ModelStreamEvent::Delta {
                channel: StreamChannel::ReasoningSummary,
                text,
                ..
            } if !text.is_empty() => {
                self.reasoning_started_at.get_or_insert(now);
            }
            ModelStreamEvent::Delta {
                channel: StreamChannel::Text,
                text,
                ..
            } if !text.is_empty() => {
                if self.reasoning_started_at.is_some() {
                    self.output_started_at.get_or_insert(now);
                }
            }
            ModelStreamEvent::ToolCallDelta { .. } if self.reasoning_started_at.is_some() => {
                self.output_started_at.get_or_insert(now);
            }
            _ => {}
        }
    }

    pub fn reasoning_duration_ms(&self) -> Option<u64> {
        let started = self.reasoning_started_at?;
        let ended = self.output_started_at.unwrap_or_else(Instant::now);
        u64::try_from(ended.saturating_duration_since(started).as_millis()).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedToolCall {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
pub struct TokenUsage {
    /// Input tokens that were not served from the provider cache.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from the provider cache. This is accounting
    /// metadata; it is never included in `input_tokens`.
    #[serde(default)]
    pub cache_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    /// Provisional delta; never execute tools from this alone.
    Delta {
        attempt_id: String,
        sequence: u64,
        channel: StreamChannel,
        text: String,
        provisional: bool,
        /// Provider usage snapshot carried alongside a delta for durable
        /// accounting. Absent until the provider reports usage (Anthropic
        /// `message_start`/`message_delta`, OpenAI final chunk).
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    ToolCallDelta {
        attempt_id: String,
        sequence: u64,
        delta: ToolCallDelta,
        provisional: bool,
    },
    Completed {
        attempt_id: String,
        usage: TokenUsage,
        stop_reason: Option<String>,
        tool_calls: Vec<CompletedToolCall>,
        /// Final assistant text assembled from deltas (for Round commit).
        text: String,
        /// Display-formatted reasoning summary (readable line breaks injected).
        reasoning: String,
        /// Raw reasoning deltas echoed back verbatim. Thinking-mode providers
        /// require the exact reasoning to be passed back on the next request;
        /// this must never be reformatted. None when the provider does not
        /// expose raw reasoning.
        reasoning_content: Option<String>,
        /// Time from the first reasoning delta to the first answer/tool delta.
        /// If there is no answer/tool delta, this ends at stream completion.
        reasoning_duration_ms: Option<u64>,
    },
    Failed {
        attempt_id: String,
        code: String,
        detail: String,
    },
    /// Emitted just before an in-Round retry. `attempt` is the retry index the
    /// model_attempts ledger is about to record (1-based; there is no retry cap),
    /// and `detail` is the human-facing failure reason for the attempt that just
    /// failed. The model stream publisher forwards it as `model.attempt_retrying`
    /// so the UI can render the retry count without an artificial maximum.
    Retrying {
        attempt_id: String,
        attempt: usize,
        detail: String,
        retry_after_ms: u64,
    },
}

pub(crate) fn truncated_stream_event(attempt_id: &str) -> ModelStreamEvent {
    ModelStreamEvent::Failed {
        attempt_id: attempt_id.to_owned(),
        code: "PROVIDER_STREAM_FAILED".into(),
        detail: "provider stream ended before its terminal frame".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::{ModelStreamEvent, StreamChannel, StreamTiming, append_reasoning_summary};

    fn delta(channel: StreamChannel, text: &str) -> ModelStreamEvent {
        ModelStreamEvent::Delta {
            attempt_id: "attempt".into(),
            sequence: 0,
            channel,
            text: text.into(),
            provisional: true,
            usage: None,
        }
    }

    #[test]
    fn reasoning_duration_stops_at_first_answer_delta() {
        let mut timing = StreamTiming::default();
        timing.observe(&delta(StreamChannel::ReasoningSummary, "thought"));
        timing.observe(&delta(StreamChannel::Text, "answer"));
        let measured = timing
            .reasoning_duration_ms()
            .expect("reasoning should have a duration after an answer delta");

        thread::sleep(Duration::from_millis(5));
        assert_eq!(timing.reasoning_duration_ms(), Some(measured));

        timing.observe(&delta(StreamChannel::Text, " more answer"));
        assert_eq!(timing.reasoning_duration_ms(), Some(measured));
    }

    #[test]
    fn reasoning_duration_is_absent_without_reasoning() {
        let mut timing = StreamTiming::default();
        timing.observe(&delta(StreamChannel::Text, "answer"));
        assert_eq!(timing.reasoning_duration_ms(), None);
    }

    #[test]
    fn reasoning_summary_deltas_are_separated_by_content_shape() {
        let mut summary = String::from("Summarizing the workspace state");
        append_reasoning_summary(&mut summary, "Detailing the relevant files");
        assert_eq!(
            summary,
            "Summarizing the workspace state\nDetailing the relevant files"
        );

        append_reasoning_summary(&mut summary, " with more context");
        assert_eq!(
            summary,
            "Summarizing the workspace state\nDetailing the relevant files with more context"
        );
    }
}
