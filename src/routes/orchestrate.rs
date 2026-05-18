//! `POST /v1/orchestrate` — gateway-driven multi-turn tool loop.
//!
//! The customer sends a conversation + LLM provider config. The gateway runs
//! the loop: call LLM → if tool calls, dispatch to McpTools (the same
//! `describe_cluster` / `run_subagent` / `estimate_cost` surface), append
//! tool_result, repeat. Returns the final assistant text + a trace.
//!
//! This is the "we run the loop" convenience layer. The "tenant runs the
//! loop" path remains `POST /v1/mcp/tools/call`.

use axum::{extract::State, routing::post, Json, Router};
use hive_mcp_gateway::tools::{EstimateCostRequest, RunSubagentRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::frontier::{
    AssistantContent, ChatMessage, ChatResponse, LlmProviderConfig, ToolCall, ToolDef,
};
use crate::tenant::ApiKeyScope;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/orchestrate", post(orchestrate))
}

const DEFAULT_MAX_ITERATIONS: u32 = 10;
const HARD_MAX_ITERATIONS: u32 = 50;

#[derive(Debug, Deserialize)]
struct OrchestrateRequest {
    /// Conversation history. Typically starts with one or more user messages.
    /// Including prior `assistant` and `tool` messages is supported for
    /// resumed conversations.
    messages: Vec<ChatMessage>,
    /// Which provider + model to use. The customer's own API key.
    llm: LlmProviderConfig,
    /// Subset of tools to expose. `None` means all available tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<String>>,
    /// Hard cap on tool-loop iterations. Default 10, max 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_iterations: Option<u32>,
}

#[derive(Debug, Serialize)]
struct OrchestrateResponse {
    /// Final assistant text, or an empty string if the loop terminated for
    /// another reason (max_iterations).
    final_message: String,
    /// Whether we exited because the LLM said it was done, hit the iteration
    /// cap, or the customer-supplied tool list rejected an LLM-requested tool.
    stop_reason: &'static str,
    /// Per-iteration trace of LLM responses + tool dispatch outcomes.
    trace: Vec<TraceEntry>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TraceEntry {
    /// LLM turn that produced tool calls. We log the calls and their dispatched
    /// outcomes inline so the trace reads top-to-bottom.
    ToolTurn {
        iteration: u32,
        tools: Vec<ToolDispatchTrace>,
    },
    /// LLM turn that produced final text.
    FinalTurn { iteration: u32, text: String },
}

#[derive(Debug, Serialize)]
struct ToolDispatchTrace {
    tool_use_id: String,
    name: String,
    arguments: Value,
    /// `Some` on success. `None` if the tool failed; see `error`.
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn orchestrate(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<OrchestrateRequest>,
) -> GatewayResult<Json<OrchestrateResponse>> {
    auth.require_scope(ApiKeyScope::Orchestrate)?;

    let max_iterations = req
        .max_iterations
        .unwrap_or(DEFAULT_MAX_ITERATIONS)
        .clamp(1, HARD_MAX_ITERATIONS);

    let llm = state
        .frontier_factory
        .build(&req.llm)
        .map_err(GatewayError::from)?;

    let tools = filter_tools(req.tools.as_deref());
    let mut messages = req.messages;
    let mut trace: Vec<TraceEntry> = Vec::new();

    for iteration in 1..=max_iterations {
        let span = tracing::info_span!(
            "tenant_gateway.orchestrate",
            hivefabric.tenant_id = %auth.tenant.id,
            iteration,
        );
        let _e = span.enter();

        let resp = llm.chat(&messages, &tools).await.map_err(GatewayError::from)?;
        match resp {
            ChatResponse::Final { text } => {
                trace.push(TraceEntry::FinalTurn {
                    iteration,
                    text: text.clone(),
                });
                return Ok(Json(OrchestrateResponse {
                    final_message: text,
                    stop_reason: "end_turn",
                    trace,
                }));
            }
            ChatResponse::Tools {
                calls,
                assistant_blocks,
            } => {
                // Persist the assistant turn so the next call has full
                // context (matches Anthropic's transcript replay rules).
                messages.push(ChatMessage::Assistant {
                    content: AssistantContent::Blocks(assistant_blocks),
                });

                let mut dispatched = Vec::with_capacity(calls.len());
                for call in calls {
                    let dispatch = dispatch_tool(&state, &auth, &call).await;
                    match &dispatch {
                        Ok(value) => {
                            messages.push(ChatMessage::Tool {
                                tool_use_id: call.id.clone(),
                                content: value.to_string(),
                            });
                            dispatched.push(ToolDispatchTrace {
                                tool_use_id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                result: Some(value.clone()),
                                error: None,
                            });
                        }
                        Err(e) => {
                            // Feed the error back to the LLM as a tool_result
                            // so it can recover. Keep the loop going.
                            let err_msg = e.to_string();
                            messages.push(ChatMessage::Tool {
                                tool_use_id: call.id.clone(),
                                content: format!("error: {err_msg}"),
                            });
                            dispatched.push(ToolDispatchTrace {
                                tool_use_id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                result: None,
                                error: Some(err_msg),
                            });
                        }
                    }
                }
                trace.push(TraceEntry::ToolTurn {
                    iteration,
                    tools: dispatched,
                });
            }
        }
    }

    Ok(Json(OrchestrateResponse {
        final_message: String::new(),
        stop_reason: "max_iterations",
        trace,
    }))
}

/// Available tool catalogue. Matches the tool list `mcp_stdio::handle_tools_list`
/// and `routes::mcp::tools_list` advertise.
fn all_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "describe_cluster".into(),
            description:
                "List the capabilities (workloads) HiveFabric can serve.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDef {
            name: "run_subagent".into(),
            description: "Run a generic-inference task on the HiveFabric network. Pick a model (model_id or capability_urn) and send a prompt. The 'what' lives in the prompt.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "model_id": { "type": "string" },
                    "capability_urn": { "type": "string" },
                    "prompt": { "type": "string" },
                    "profile": { "type": "string", "default": "default" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "default": 60 }
                },
                "required": ["prompt"]
            }),
        },
        ToolDef {
            name: "estimate_cost".into(),
            description: "Pre-execution cost estimate (Phase 2 — requires Honey Ledger).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "capability_urn": { "type": "string" },
                    "input_size_tokens": { "type": "integer", "minimum": 0 }
                },
                "required": ["capability_urn", "input_size_tokens"]
            }),
        },
    ]
}

fn filter_tools(allow: Option<&[String]>) -> Vec<ToolDef> {
    let all = all_tools();
    match allow {
        None => all,
        Some(list) => all
            .into_iter()
            .filter(|t| list.iter().any(|n| n == &t.name))
            .collect(),
    }
}

/// Dispatch one tool call. Returns the raw JSON result that gets fed back
/// to the LLM as `tool_result.content`.
async fn dispatch_tool(
    state: &AppState,
    auth: &AuthedTenant,
    call: &ToolCall,
) -> GatewayResult<Value> {
    match call.name.as_str() {
        "describe_cluster" => {
            let resp = state.tools.describe_cluster().await?;
            serde_json::to_value(resp)
                .map_err(|e| GatewayError::Internal(format!("serialize: {e}")))
        }
        "run_subagent" => {
            let mut typed: RunSubagentRequest =
                serde_json::from_value(call.arguments.clone())
                    .map_err(|e| GatewayError::Invalid(format!("run_subagent args: {e}")))?;
            // Same rule as the tools/call path: tenant_id is forced from the
            // bearer, never accepted from the LLM.
            typed.tenant_id = Some(auth.tenant.id);
            let resp = state.tools.run_subagent(typed).await?;
            serde_json::to_value(resp)
                .map_err(|e| GatewayError::Internal(format!("serialize: {e}")))
        }
        "estimate_cost" => {
            let typed: EstimateCostRequest =
                serde_json::from_value(call.arguments.clone())
                    .map_err(|e| GatewayError::Invalid(format!("estimate_cost args: {e}")))?;
            let resp = state.tools.estimate_cost(typed).await?;
            serde_json::to_value(resp)
                .map_err(|e| GatewayError::Internal(format!("serialize: {e}")))
        }
        other => Err(GatewayError::Invalid(format!("unknown tool: {other}"))),
    }
}
