//! End-to-end HTTP-level tests with a stubbed `McpTools`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use hive_mcp_gateway::tools::{
    DescribeClusterResponse, EstimateCostRequest, EstimateCostResponse, McpTools,
    RunSubagentRequest, RunSubagentResponse,
};
use hive_mcp_gateway::GatewayError as McpGatewayError;
use hive_tenant_gateway::frontier::{
    ChatMessage, ChatResponse, FrontierLlm, FrontierLlmError, FrontierLlmFactory,
    LlmProviderConfig, ToolCall, ToolDef,
};
use hive_tenant_gateway::{
    router, tenant::ApiKeyScope, AppState, InMemoryTenantStore, TenantStore,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

/// Records the `tenant_id` the gateway forwarded on each `run_subagent` call.
/// Lets us assert that the bearer's tenant id is what reaches Honeycomb.
#[derive(Default)]
struct StubTools {
    last_tenant_id: Mutex<Option<Uuid>>,
}

/// FrontierLlm stub. Returns a scripted sequence of `ChatResponse`s — one per
/// `chat()` call. Lets tests drive the orchestration loop deterministically
/// without hitting the network.
struct ScriptedFrontier {
    script: Mutex<std::collections::VecDeque<ChatResponse>>,
}

impl ScriptedFrontier {
    fn new(script: Vec<ChatResponse>) -> Self {
        Self {
            script: Mutex::new(script.into()),
        }
    }
}

#[async_trait]
impl FrontierLlm for ScriptedFrontier {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDef],
    ) -> Result<ChatResponse, FrontierLlmError> {
        match self.script.lock().unwrap().pop_front() {
            Some(r) => Ok(r),
            None => Err(FrontierLlmError::InvalidResponse(
                "scripted frontier ran out of canned responses".into(),
            )),
        }
    }
}

struct FixedFactory(std::sync::Arc<dyn FrontierLlm>);

impl FrontierLlmFactory for FixedFactory {
    fn build(
        &self,
        _config: &LlmProviderConfig,
    ) -> Result<Box<dyn FrontierLlm>, FrontierLlmError> {
        // Box::new on an Arc<dyn>: clone Arc, return as Box wrapping a tiny shim.
        struct Forward(std::sync::Arc<dyn FrontierLlm>);
        #[async_trait]
        impl FrontierLlm for Forward {
            async fn chat(
                &self,
                m: &[ChatMessage],
                t: &[ToolDef],
            ) -> Result<ChatResponse, FrontierLlmError> {
                self.0.chat(m, t).await
            }
        }
        Ok(Box::new(Forward(self.0.clone())))
    }
}

#[async_trait]
impl McpTools for StubTools {
    async fn describe_cluster(&self) -> Result<DescribeClusterResponse, McpGatewayError> {
        Ok(DescribeClusterResponse {
            capabilities: vec![],
        })
    }
    async fn run_subagent(
        &self,
        req: RunSubagentRequest,
    ) -> Result<RunSubagentResponse, McpGatewayError> {
        *self.last_tenant_id.lock().unwrap() = req.tenant_id;
        Ok(RunSubagentResponse {
            task_id: Uuid::nil(),
            status: "completed".into(),
            output: Some(json!({"echo": req.prompt})),
            error: None,
        })
    }
    async fn estimate_cost(
        &self,
        _req: EstimateCostRequest,
    ) -> Result<EstimateCostResponse, McpGatewayError> {
        Err(McpGatewayError::Unsupported("stub"))
    }
}

async fn build_app() -> (axum::Router, String, Uuid, Arc<StubTools>) {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants
        .mint_api_key(
            t.id,
            vec![ApiKeyScope::ToolsInvoke, ApiKeyScope::Orchestrate],
        )
        .await
        .unwrap();
    let stub = Arc::new(StubTools::default());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = stub.clone();
    // Orchestrate tests that need a scripted frontier supply their own state
    // via `build_app_with_script`; this default factory always errors, which
    // is fine because no test in this default path hits orchestrate.
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier);
    (router(state), mint.plaintext, t.id, stub)
}

struct FailFactory;
impl FrontierLlmFactory for FailFactory {
    fn build(
        &self,
        _config: &LlmProviderConfig,
    ) -> Result<Box<dyn FrontierLlm>, FrontierLlmError> {
        Err(FrontierLlmError::UnsupportedProvider(
            "tests must inject a scripted frontier".into(),
        ))
    }
}

async fn build_app_with_script(
    script: Vec<ChatResponse>,
) -> (axum::Router, String, Uuid, Arc<StubTools>) {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants
        .mint_api_key(
            t.id,
            vec![ApiKeyScope::ToolsInvoke, ApiKeyScope::Orchestrate],
        )
        .await
        .unwrap();
    let stub = Arc::new(StubTools::default());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = stub.clone();
    let scripted: Arc<dyn FrontierLlm> = Arc::new(ScriptedFrontier::new(script));
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FixedFactory(scripted));
    let state = AppState::new(tenants, tools, frontier);
    (router(state), mint.plaintext, t.id, stub)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn healthz_is_public() {
    let (app, _key, _id, _stub) = build_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn whoami_returns_tenant_for_valid_bearer() {
    let (app, key, tenant_id, _stub) = build_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/whoami")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["tenant_id"], tenant_id.to_string());
    assert_eq!(body["plan"], "free");
}

#[tokio::test]
async fn whoami_rejects_missing_authorization() {
    let (app, _key, _id, _stub) = build_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_rejects_bad_bearer() {
    let (app, _key, _id, _stub) = build_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/whoami")
                .header("authorization", "Bearer hf_not-a-real-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tools_call_run_subagent_round_trips_through_stub() {
    let (app, key, _id, _stub) = build_app().await;
    let body = json!({
        "name": "run_subagent",
        "arguments": {
            "prompt": "Classify: 'great game!' as positive | negative.",
            "model_id": "qwen2.5:0.5b"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp/tools/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(
        body["output"]["echo"],
        "Classify: 'great game!' as positive | negative."
    );
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_400() {
    let (app, key, _id, _stub) = build_app().await;
    let body = json!({"name": "definitely_not_a_tool", "arguments": {}});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp/tools/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn tools_call_missing_scope_returns_403() {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants.mint_api_key(t.id, vec![]).await.unwrap();
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools::default());
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier);
    let app = router(state);
    let body = json!({"name": "describe_cluster", "arguments": {}});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp/tools/call")
                .header("authorization", format!("Bearer {}", mint.plaintext))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_create_tenant_and_use_returned_key() {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools::default());
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier).with_admin_key("admin-secret".into());
    let app = router(state);

    let create_body = json!({"name": "newco"});
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/v1/tenants")
                .header("content-type", "application/json")
                .header("x-admin-key", "admin-secret")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created = body_json(resp).await;
    let key = created["initial_api_key"]["plaintext"].as_str().unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/whoami")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["tenant_name"], "newco");
}

#[tokio::test]
async fn revoking_a_key_invalidates_subsequent_calls() {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants
        .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke])
        .await
        .unwrap();
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools::default());
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier).with_admin_key("admin-secret".into());
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/v1/api-keys/{}", mint.api_key.id))
                .header("x-admin-key", "admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/whoami")
                .header("authorization", format!("Bearer {}", mint.plaintext))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn run_subagent_propagates_authenticated_tenant_id() {
    let (app, key, tenant_id, stub) = build_app().await;
    let body = json!({
        "name": "run_subagent",
        "arguments": {
            "prompt": "x",
            "model_id": "qwen2.5:0.5b"
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp/tools/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(*stub.last_tenant_id.lock().unwrap(), Some(tenant_id));
}

#[tokio::test]
async fn run_subagent_overrides_caller_provided_tenant_id() {
    // Customer tries to spoof a different tenant_id in the request body.
    // The gateway must override it with the bearer's authenticated tenant.
    let (app, key, tenant_id, stub) = build_app().await;
    let spoof = Uuid::new_v4();
    assert_ne!(spoof, tenant_id);
    let body = json!({
        "name": "run_subagent",
        "arguments": {
            "prompt": "x",
            "model_id": "qwen2.5:0.5b",
            "tenant_id": spoof
        }
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp/tools/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(*stub.last_tenant_id.lock().unwrap(), Some(tenant_id));
}

// ────────────────────────────────────────────────────────────────────────────
// /v1/orchestrate
// ────────────────────────────────────────────────────────────────────────────

use hive_tenant_gateway::frontier::AssistantBlock;

fn dummy_llm_config() -> Value {
    json!({
        "provider": "anthropic",
        "model": "claude-3-5-sonnet-latest",
        "api_key": "sk-ant-test"
    })
}

#[tokio::test]
async fn orchestrate_returns_final_message_when_llm_does_not_call_tools() {
    let script = vec![ChatResponse::Final {
        text: "All done.".into(),
    }];
    let (app, key, _id, _stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "say hi"}],
        "llm": dummy_llm_config(),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["final_message"], "All done.");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["trace"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn orchestrate_runs_tool_loop_and_returns_final_after_dispatch() {
    // Turn 1: LLM asks for run_subagent.
    // Turn 2: LLM emits final text.
    let script = vec![
        ChatResponse::Tools {
            calls: vec![ToolCall {
                id: "toolu_1".into(),
                name: "run_subagent".into(),
                arguments: json!({
                    "model_id": "qwen2.5:0.5b",
                    "prompt": "Classify: 'great game!'"
                }),
            }],
            assistant_blocks: vec![AssistantBlock::ToolUse {
                id: "toolu_1".into(),
                name: "run_subagent".into(),
                input: json!({
                    "model_id": "qwen2.5:0.5b",
                    "prompt": "Classify: 'great game!'"
                }),
            }],
        },
        ChatResponse::Final {
            text: "Sentiment is positive.".into(),
        },
    ];
    let (app, key, tenant_id, stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "What's the sentiment of 'great game!'?"}],
        "llm": dummy_llm_config(),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["final_message"], "Sentiment is positive.");
    assert_eq!(body["stop_reason"], "end_turn");

    // Trace records both turns.
    let trace = body["trace"].as_array().unwrap();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0]["kind"], "tool_turn");
    assert_eq!(trace[1]["kind"], "final_turn");

    // run_subagent went out scoped to the authed tenant, not whatever the LLM
    // emitted in arguments.
    assert_eq!(*stub.last_tenant_id.lock().unwrap(), Some(tenant_id));
}

#[tokio::test]
async fn orchestrate_caps_at_max_iterations() {
    // Five identical tool turns; max_iterations=3 should terminate before the
    // sequence runs out.
    let one = || ChatResponse::Tools {
        calls: vec![ToolCall {
            id: "toolu_x".into(),
            name: "describe_cluster".into(),
            arguments: json!({}),
        }],
        assistant_blocks: vec![AssistantBlock::ToolUse {
            id: "toolu_x".into(),
            name: "describe_cluster".into(),
            input: json!({}),
        }],
    };
    let script = vec![one(), one(), one(), one(), one()];
    let (app, key, _id, _stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "hi"}],
        "llm": dummy_llm_config(),
        "max_iterations": 3
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["stop_reason"], "max_iterations");
    assert_eq!(body["final_message"], "");
    assert_eq!(body["trace"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn orchestrate_requires_orchestrate_scope() {
    // Tenant has only ToolsInvoke — no Orchestrate.
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants
        .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke])
        .await
        .unwrap();
    let stub = Arc::new(StubTools::default());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = stub.clone();
    let scripted: Arc<dyn FrontierLlm> =
        Arc::new(ScriptedFrontier::new(vec![ChatResponse::Final {
            text: "ok".into(),
        }]));
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FixedFactory(scripted));
    let state = AppState::new(tenants, tools, frontier);
    let app = router(state);

    let body = json!({
        "messages": [{"role": "user", "content": "x"}],
        "llm": dummy_llm_config(),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {}", mint.plaintext))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn orchestrate_feeds_tool_error_back_to_llm_and_keeps_looping() {
    // Turn 1: LLM calls a tool that errors (unknown tool).
    // Turn 2: LLM emits final text — meaning it saw the error and recovered.
    let script = vec![
        ChatResponse::Tools {
            calls: vec![ToolCall {
                id: "toolu_e".into(),
                name: "totally_made_up".into(),
                arguments: json!({}),
            }],
            assistant_blocks: vec![AssistantBlock::ToolUse {
                id: "toolu_e".into(),
                name: "totally_made_up".into(),
                input: json!({}),
            }],
        },
        ChatResponse::Final {
            text: "I'll stop trying.".into(),
        },
    ];
    let (app, key, _id, _stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "x"}],
        "llm": dummy_llm_config(),
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["final_message"], "I'll stop trying.");
    let trace = body["trace"].as_array().unwrap();
    assert_eq!(trace[0]["kind"], "tool_turn");
    let tools = trace[0]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "totally_made_up");
    assert!(tools[0]["error"].is_string());
}

// ────────────────────────────────────────────────────────────────────────────
// Admin auth gate (HF_ADMIN_KEY / x-admin-key)
// ────────────────────────────────────────────────────────────────────────────

fn admin_app_with_key(key: &str) -> axum::Router {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools::default());
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier).with_admin_key(key.to_string());
    router(state)
}

fn admin_app_without_key() -> axum::Router {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools::default());
    let frontier: Arc<dyn FrontierLlmFactory> = Arc::new(FailFactory);
    let state = AppState::new(tenants, tools, frontier);
    router(state)
}

#[tokio::test]
async fn admin_endpoint_returns_503_when_admin_key_unset() {
    // No admin_key on AppState => admin surface disabled.
    let app = admin_app_without_key();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/v1/tenants")
                .header("content-type", "application/json")
                .header("x-admin-key", "anything-goes")
                .body(Body::from(serde_json::to_vec(&json!({"name":"x"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn admin_endpoint_rejects_missing_header_when_admin_key_set() {
    let app = admin_app_with_key("admin-secret");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/v1/tenants")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({"name":"x"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_endpoint_rejects_wrong_admin_key() {
    let app = admin_app_with_key("admin-secret");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/v1/tenants")
                .header("content-type", "application/json")
                .header("x-admin-key", "guess-no")
                .body(Body::from(serde_json::to_vec(&json!({"name":"x"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn orchestrate_works_with_openai_provider_config() {
    // Provider field is "openai" instead of "anthropic". The route handler
    // passes the config to the factory; tests inject a scripted frontier that
    // ignores provider — what we're checking is that an `openai` config
    // deserialises and reaches the loop intact.
    let script = vec![ChatResponse::Final {
        text: "Sure thing.".into(),
    }];
    let (app, key, _id, _stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "say hi"}],
        "llm": {
            "provider": "openai",
            "model": "gpt-4o",
            "api_key": "sk-test"
        },
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["final_message"], "Sure thing.");
}

#[tokio::test]
async fn orchestrate_accepts_openai_compatible_base_url() {
    // base_url override → routes to an OpenAI-compatible endpoint
    // (Together, Groq, vLLM, Ollama). Same scripted frontier; we're just
    // exercising deserialisation + factory dispatch.
    let script = vec![ChatResponse::Final { text: "ok".into() }];
    let (app, key, _id, _stub) = build_app_with_script(script).await;

    let body = json!({
        "messages": [{"role": "user", "content": "x"}],
        "llm": {
            "provider": "openai",
            "model": "llama3.1:70b",
            "api_key": "sk-anything",
            "base_url": "https://api.together.xyz"
        },
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orchestrate")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
