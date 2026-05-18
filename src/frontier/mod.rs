//! Frontier LLM adapters — bring your own model.
//!
//! The orchestration path (`POST /v1/orchestrate`) drives a multi-turn tool
//! loop using whichever frontier LLM the tenant chose: Claude (Anthropic),
//! GPT (OpenAI), Gemini (Google), Bedrock, or any OpenAI-compatible endpoint
//! (vLLM, Together, etc.).
//!
//! Each adapter implements [`FrontierLlm`] — a single async method that takes
//! the conversation so far and the tool catalogue, and returns either a final
//! assistant message or a list of tool calls the LLM wants the gateway to
//! dispatch.
//!
//! Adapter selection is per-request: the customer sends a `LlmProviderConfig`
//! in the request body specifying provider + model + API key. A future
//! Phase 2.2 batch adds a server-side `tenant_llm_providers` registry so
//! customers can store provider configs once and reference them by id.

pub mod anthropic;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::error::GatewayError;

/// Errors returned by a frontier-LLM adapter.
#[derive(Debug, Error)]
pub enum FrontierLlmError {
    #[error("frontier LLM HTTP error: {0}")]
    Http(String),
    #[error("invalid response from frontier LLM: {0}")]
    InvalidResponse(String),
    #[error("frontier LLM rejected the request: {0}")]
    UpstreamRejected(String),
    #[error("provider not implemented: {0}")]
    UnsupportedProvider(String),
}

impl From<FrontierLlmError> for GatewayError {
    fn from(value: FrontierLlmError) -> Self {
        // Customer-facing: 502 with the upstream's reason.
        GatewayError::Internal(format!("frontier LLM: {value}"))
    }
}

/// Per-request LLM provider config sent in the `POST /v1/orchestrate` body.
///
/// Phase 2.1 ships only the Anthropic provider; the enum is open so the
/// remaining providers can land additively without a wire-format break.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum LlmProviderConfig {
    Anthropic {
        model: String,
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    // Phase 2.2: Openai { model, api_key, base_url? },
    // Phase 2.2: Gemini { model, api_key },
    // Phase 2.2: Bedrock { model, region, ... },
    // Phase 2.2: OpenaiCompatible { model, base_url, api_key? },
}

/// A single tool the LLM can call. Mirrors Anthropic's tool definition shape;
/// adapters translate to per-provider formats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// One element of the conversation passed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    /// User-side text (or any client-supplied content).
    User { content: String },
    /// Prior assistant turn — text-only or with embedded tool calls.
    Assistant { content: AssistantContent },
    /// Tool result for a prior tool_use, by id.
    Tool {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContent {
    /// Plain text completion.
    Text(String),
    /// Mixed: zero-or-more text blocks plus one-or-more tool_use blocks.
    /// The order matters when replaying transcripts to the model.
    Blocks(Vec<AssistantBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantBlock {
    Text { text: String },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// One tool invocation the LLM is asking the gateway to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// What the LLM returned this turn.
#[derive(Debug, Clone)]
pub enum ChatResponse {
    /// Conversation is over; here is the final assistant text.
    Final { text: String },
    /// LLM wants the gateway to dispatch one or more tool calls and feed the
    /// results back. The `assistant_blocks` field carries the raw assistant
    /// content (text + tool_use) so the next call can replay it verbatim.
    Tools {
        calls: Vec<ToolCall>,
        assistant_blocks: Vec<AssistantBlock>,
    },
}

/// One frontier-LLM adapter — Anthropic, OpenAI, Gemini, etc.
#[async_trait]
pub trait FrontierLlm: Send + Sync {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ChatResponse, FrontierLlmError>;
}

/// Builds a [`FrontierLlm`] from a per-request config. Pulled out into a
/// trait so tests can inject a stubbed factory and not hit the network.
pub trait FrontierLlmFactory: Send + Sync {
    fn build(
        &self,
        config: &LlmProviderConfig,
    ) -> Result<Box<dyn FrontierLlm>, FrontierLlmError>;
}

/// Default factory: instantiates real provider adapters from config.
pub struct DefaultFrontierLlmFactory;

impl FrontierLlmFactory for DefaultFrontierLlmFactory {
    fn build(
        &self,
        config: &LlmProviderConfig,
    ) -> Result<Box<dyn FrontierLlm>, FrontierLlmError> {
        match config {
            LlmProviderConfig::Anthropic {
                model,
                api_key,
                base_url,
            } => Ok(Box::new(anthropic::AnthropicAdapter::new(
                model.clone(),
                api_key.clone(),
                base_url.clone(),
            ))),
        }
    }
}
