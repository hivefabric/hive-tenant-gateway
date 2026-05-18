//! OpenAI Chat Completions adapter.
//!
//! Implements [`super::FrontierLlm`] against `POST {base_url}/v1/chat/completions`.
//! Same wire spec covers OpenAI itself plus any "OpenAI-compatible" provider
//! (Together, Groq, vLLM, Ollama's `/v1/chat/completions`, etc.) — set
//! `base_url` to point elsewhere and the rest works.
//!
//! Wire format reference:
//! https://platform.openai.com/docs/api-reference/chat/create
//!
//! Translation differences vs Anthropic:
//!
//! - **Auth header**: `Authorization: Bearer <key>`, not `x-api-key`.
//! - **Tool definition shape**: wrapped in `{type: "function", function: {...}}`.
//!   Schema field is `parameters`, not `input_schema`.
//! - **Tool calls in assistant turn**: `tool_calls: [{id, type:"function",
//!   function:{name, arguments}}]`. **Critical**: `arguments` is a JSON-
//!   encoded *string*, not an object. Both directions need parse/stringify.
//! - **Tool result** is its own role (`role: "tool"`) with `tool_call_id`,
//!   not a `tool_result` block inside a user turn the way Anthropic does it.
//! - **Stop reason**: `finish_reason` field, value `"tool_calls"` means
//!   "I want you to dispatch these and call me back" (mirror of Anthropic's
//!   `stop_reason: "tool_use"`).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    AssistantBlock, AssistantContent, ChatMessage, ChatResponse, FrontierLlm, FrontierLlmError,
    ToolCall, ToolDef,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

pub struct OpenaiAdapter {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub client: reqwest::Client,
}

impl OpenaiAdapter {
    pub fn new(model: String, api_key: String, base_url: Option<String>) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            client: reqwest::Client::new(),
        }
    }

    /// Translate our `ChatMessage`s into OpenAI's `messages` array.
    fn build_messages(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m {
                ChatMessage::User { content } => json!({
                    "role": "user",
                    "content": content,
                }),
                ChatMessage::Assistant { content } => match content {
                    AssistantContent::Text(text) => json!({
                        "role": "assistant",
                        "content": text,
                    }),
                    AssistantContent::Blocks(blocks) => assistant_with_blocks(blocks),
                },
                ChatMessage::Tool {
                    tool_use_id,
                    content,
                } => json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                }),
            })
            .collect()
    }

    /// Translate our `ToolDef`s into OpenAI's `tools` array.
    fn build_tools(tools: &[ToolDef]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }
}

fn assistant_with_blocks(blocks: &[AssistantBlock]) -> Value {
    let mut text_buf = String::new();
    let mut tool_calls = Vec::new();
    for b in blocks {
        match b {
            AssistantBlock::Text { text } => {
                if !text_buf.is_empty() {
                    text_buf.push('\n');
                }
                text_buf.push_str(text);
            }
            AssistantBlock::ToolUse { id, name, input } => {
                // OpenAI requires `arguments` to be a JSON-encoded *string*.
                let args_str = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args_str }
                }));
            }
        }
    }
    let mut msg = serde_json::Map::new();
    msg.insert("role".into(), Value::String("assistant".into()));
    // OpenAI accepts `content: null` when tool_calls are present.
    if text_buf.is_empty() && !tool_calls.is_empty() {
        msg.insert("content".into(), Value::Null);
    } else {
        msg.insert("content".into(), Value::String(text_buf));
    }
    if !tool_calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    Value::Object(msg)
}

#[derive(Deserialize)]
struct OpenaiResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    finish_reason: Option<String>,
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<RespToolCall>>,
}

#[derive(Deserialize)]
struct RespToolCall {
    id: String,
    function: RespToolFunction,
}

#[derive(Deserialize)]
struct RespToolFunction {
    name: String,
    /// JSON-encoded arguments string. We parse it back to Value before
    /// returning to keep our internal types provider-agnostic.
    arguments: String,
}

#[async_trait]
impl FrontierLlm for OpenaiAdapter {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, FrontierLlmError> {
        let body = json!({
            "model": self.model,
            "messages": Self::build_messages(messages),
            "tools": Self::build_tools(tools),
        });

        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
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
        let parsed: OpenaiResponse = serde_json::from_slice(&bytes).map_err(|e| {
            FrontierLlmError::InvalidResponse(format!(
                "{e}: {}",
                String::from_utf8_lossy(&bytes)
            ))
        })?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| FrontierLlmError::InvalidResponse("no choices".into()))?;

        let tool_calls = choice.message.tool_calls.unwrap_or_default();
        let final_text = choice.message.content.unwrap_or_default();
        let wants_tools = matches!(choice.finish_reason.as_deref(), Some("tool_calls"))
            && !tool_calls.is_empty();

        if wants_tools {
            let mut calls = Vec::with_capacity(tool_calls.len());
            let mut blocks = Vec::with_capacity(tool_calls.len() + 1);
            if !final_text.is_empty() {
                blocks.push(AssistantBlock::Text {
                    text: final_text.clone(),
                });
            }
            for tc in tool_calls {
                let parsed_args: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| Value::String(tc.function.arguments.clone()));
                calls.push(ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: parsed_args.clone(),
                });
                blocks.push(AssistantBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input: parsed_args,
                });
            }
            Ok(ChatResponse::Tools {
                calls,
                assistant_blocks: blocks,
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
    fn build_messages_translates_user_assistant_tool_in_order() {
        let convo = vec![
            ChatMessage::User {
                content: "hi".into(),
            },
            ChatMessage::Assistant {
                content: AssistantContent::Text("hello".into()),
            },
            ChatMessage::Tool {
                tool_use_id: "call_1".into(),
                content: "{\"ok\":true}".into(),
            },
        ];
        let out = OpenaiAdapter::build_messages(&convo);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "hi");
        assert_eq!(out[1]["role"], "assistant");
        assert_eq!(out[1]["content"], "hello");
        assert_eq!(out[2]["role"], "tool");
        assert_eq!(out[2]["tool_call_id"], "call_1");
    }

    #[test]
    fn build_messages_assistant_with_tool_calls_uses_null_content_and_string_args() {
        // Tool-only assistant turn (no text) should serialise as
        // `content: null` + tool_calls with string-encoded arguments.
        let convo = vec![ChatMessage::Assistant {
            content: AssistantContent::Blocks(vec![AssistantBlock::ToolUse {
                id: "call_42".into(),
                name: "run_subagent".into(),
                input: json!({"prompt": "x", "model_id": "qwen2.5:0.5b"}),
            }]),
        }];
        let out = OpenaiAdapter::build_messages(&convo);
        assert_eq!(out[0]["role"], "assistant");
        assert!(out[0]["content"].is_null());
        let calls = out[0]["tool_calls"].as_array().unwrap();
        assert_eq!(calls[0]["id"], "call_42");
        assert_eq!(calls[0]["type"], "function");
        assert_eq!(calls[0]["function"]["name"], "run_subagent");
        // arguments is a JSON-encoded string; parsing it must yield the
        // original input object.
        let raw = calls[0]["function"]["arguments"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed["prompt"], "x");
        assert_eq!(parsed["model_id"], "qwen2.5:0.5b");
    }

    #[test]
    fn build_messages_assistant_with_text_and_tool_calls_keeps_both() {
        let convo = vec![ChatMessage::Assistant {
            content: AssistantContent::Blocks(vec![
                AssistantBlock::Text {
                    text: "I'll dispatch.".into(),
                },
                AssistantBlock::ToolUse {
                    id: "call_1".into(),
                    name: "describe_cluster".into(),
                    input: json!({}),
                },
            ]),
        }];
        let out = OpenaiAdapter::build_messages(&convo);
        assert_eq!(out[0]["content"], "I'll dispatch.");
        assert_eq!(
            out[0]["tool_calls"][0]["function"]["name"],
            "describe_cluster"
        );
    }

    #[test]
    fn build_tools_wraps_each_tool_in_function_envelope() {
        let tools = vec![ToolDef {
            name: "describe_cluster".into(),
            description: "List capabilities.".into(),
            input_schema: json!({"type":"object","properties":{},"required":[]}),
        }];
        let out = OpenaiAdapter::build_tools(&tools);
        assert_eq!(out[0]["type"], "function");
        assert_eq!(out[0]["function"]["name"], "describe_cluster");
        assert_eq!(out[0]["function"]["description"], "List capabilities.");
        // schema goes into `parameters`, not `input_schema`.
        assert_eq!(out[0]["function"]["parameters"]["type"], "object");
    }
}
