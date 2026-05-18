//! Anthropic Messages API adapter.
//!
//! Implements [`super::FrontierLlm`] against `POST /v1/messages`. Translates
//! our internal [`ChatMessage`] / [`ToolDef`] / [`ChatResponse`] types to and
//! from Anthropic's wire format.
//!
//! Wire format reference: https://docs.anthropic.com/en/api/messages
//!
//! Key shapes used here:
//!
//! Request body:
//! ```json
//! {
//!   "model": "claude-3-5-sonnet-latest",
//!   "max_tokens": 4096,
//!   "messages": [
//!     {"role": "user", "content": "Use the run_subagent tool to ..."},
//!     {"role": "assistant", "content": [
//!       {"type": "text", "text": "I'll dispatch a subagent."},
//!       {"type": "tool_use", "id": "toolu_01", "name": "run_subagent",
//!        "input": {"prompt": "...", "model_id": "qwen2.5:0.5b"}}
//!     ]},
//!     {"role": "user", "content": [
//!       {"type": "tool_result", "tool_use_id": "toolu_01",
//!        "content": "{\"output\":...}"}
//!     ]}
//!   ],
//!   "tools": [{"name": "...", "description": "...", "input_schema": {...}}]
//! }
//! ```
//!
//! Response body:
//! ```json
//! {
//!   "stop_reason": "tool_use" | "end_turn" | ...,
//!   "content": [
//!     {"type": "text", "text": "..."},
//!     {"type": "tool_use", "id": "toolu_01", "name": "...", "input": {...}}
//!   ]
//! }
//! ```

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    AssistantBlock, AssistantContent, ChatMessage, ChatResponse, FrontierLlm, FrontierLlmError,
    ToolCall, ToolDef,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicAdapter {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
}

impl AnthropicAdapter {
    pub fn new(model: String, api_key: String, base_url: Option<String>) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            client: reqwest::Client::new(),
        }
    }

    /// Translate our `ChatMessage`s into Anthropic's `messages` array.
    fn build_messages(messages: &[ChatMessage]) -> Vec<Value> {
        // Tool results need to be batched into the next *user* turn, not
        // emitted as their own role. We collect contiguous Tool entries and
        // flush them as a synthetic user message.
        let mut out = Vec::with_capacity(messages.len());
        let mut pending_tool_results: Vec<Value> = Vec::new();

        let flush = |buf: &mut Vec<Value>, sink: &mut Vec<Value>| {
            if !buf.is_empty() {
                sink.push(json!({
                    "role": "user",
                    "content": std::mem::take(buf),
                }));
            }
        };

        for m in messages {
            match m {
                ChatMessage::Tool {
                    tool_use_id,
                    content,
                } => {
                    pending_tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    }));
                }
                ChatMessage::User { content } => {
                    flush(&mut pending_tool_results, &mut out);
                    out.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                ChatMessage::Assistant { content } => {
                    flush(&mut pending_tool_results, &mut out);
                    let blocks = match content {
                        AssistantContent::Text(t) => json!([{"type":"text","text":t}]),
                        AssistantContent::Blocks(blocks) => {
                            let mapped: Vec<Value> = blocks
                                .iter()
                                .map(|b| match b {
                                    AssistantBlock::Text { text } => {
                                        json!({"type":"text","text":text})
                                    }
                                    AssistantBlock::ToolUse { id, name, input } => json!({
                                        "type":"tool_use",
                                        "id":id,
                                        "name":name,
                                        "input":input,
                                    }),
                                })
                                .collect();
                            Value::Array(mapped)
                        }
                    };
                    out.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
            }
        }
        flush(&mut pending_tool_results, &mut out);
        out
    }
}

#[derive(Deserialize)]
struct AnthropicResponse {
    stop_reason: Option<String>,
    content: Vec<AnthropicBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Catch-all so an unknown block type doesn't fail deserialisation; we
    /// just ignore it.
    #[serde(other)]
    Unknown,
}

#[async_trait]
impl FrontierLlm for AnthropicAdapter {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, FrontierLlmError> {
        let body = json!({
            "model": self.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "messages": Self::build_messages(messages),
            "tools": tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<_>>(),
        });

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| FrontierLlmError::Http(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| FrontierLlmError::Http(e.to_string()))?;
        if !status.is_success() {
            let snippet = String::from_utf8_lossy(&bytes).to_string();
            return Err(FrontierLlmError::UpstreamRejected(format!(
                "{status}: {snippet}"
            )));
        }
        let parsed: AnthropicResponse = serde_json::from_slice(&bytes)
            .map_err(|e| FrontierLlmError::InvalidResponse(format!("{e}: {}", String::from_utf8_lossy(&bytes))))?;

        // Walk content blocks: keep all assistant blocks for replay, extract
        // tool_use as ToolCalls, concatenate texts into a final-message
        // candidate.
        let mut tool_calls = Vec::new();
        let mut assistant_blocks = Vec::new();
        let mut final_text = String::new();
        for block in parsed.content {
            match block {
                AnthropicBlock::Text { text } => {
                    if !final_text.is_empty() {
                        final_text.push('\n');
                    }
                    final_text.push_str(&text);
                    assistant_blocks.push(AssistantBlock::Text { text });
                }
                AnthropicBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: input.clone(),
                    });
                    assistant_blocks.push(AssistantBlock::ToolUse { id, name, input });
                }
                AnthropicBlock::Unknown => {}
            }
        }

        // The protocol contract: stop_reason=tool_use means "I want you to
        // dispatch these and call me back." Anything else (end_turn,
        // max_tokens, stop_sequence) means we're done.
        let wants_tools = matches!(parsed.stop_reason.as_deref(), Some("tool_use"))
            && !tool_calls.is_empty();
        if wants_tools {
            Ok(ChatResponse::Tools {
                calls: tool_calls,
                assistant_blocks,
            })
        } else {
            Ok(ChatResponse::Final { text: final_text })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_messages_collapses_tool_results_into_user_turn() {
        let convo = vec![
            ChatMessage::User {
                content: "Run the classifier".into(),
            },
            ChatMessage::Assistant {
                content: AssistantContent::Blocks(vec![
                    AssistantBlock::Text {
                        text: "I'll dispatch.".into(),
                    },
                    AssistantBlock::ToolUse {
                        id: "toolu_1".into(),
                        name: "run_subagent".into(),
                        input: json!({"prompt": "x"}),
                    },
                ]),
            },
            ChatMessage::Tool {
                tool_use_id: "toolu_1".into(),
                content: "{\"label\":\"positive\"}".into(),
            },
        ];
        let out = AnthropicAdapter::build_messages(&convo);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "Run the classifier");
        assert_eq!(out[1]["role"], "assistant");
        assert_eq!(out[2]["role"], "user");
        // Tool result is wrapped as a content block array on the user turn.
        let result_blocks = out[2]["content"].as_array().expect("array");
        assert_eq!(result_blocks.len(), 1);
        assert_eq!(result_blocks[0]["type"], "tool_result");
        assert_eq!(result_blocks[0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn build_messages_handles_text_only_assistant() {
        let convo = vec![
            ChatMessage::User {
                content: "hi".into(),
            },
            ChatMessage::Assistant {
                content: AssistantContent::Text("hello".into()),
            },
        ];
        let out = AnthropicAdapter::build_messages(&convo);
        assert_eq!(out[1]["content"][0]["type"], "text");
        assert_eq!(out[1]["content"][0]["text"], "hello");
    }

    #[test]
    fn build_messages_groups_consecutive_tool_results_into_one_user_turn() {
        let convo = vec![
            ChatMessage::Tool {
                tool_use_id: "a".into(),
                content: "ra".into(),
            },
            ChatMessage::Tool {
                tool_use_id: "b".into(),
                content: "rb".into(),
            },
        ];
        let out = AnthropicAdapter::build_messages(&convo);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"].as_array().unwrap().len(), 2);
    }
}
