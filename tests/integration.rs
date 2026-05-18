//! End-to-end HTTP-level tests with a stubbed `McpTools`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use hive_mcp_gateway::tools::{
    DescribeClusterResponse, EstimateCostRequest, EstimateCostResponse, McpTools,
    RunSubagentRequest, RunSubagentResponse,
};
use hive_mcp_gateway::GatewayError as McpGatewayError;
use hive_tenant_gateway::{
    router, tenant::ApiKeyScope, AppState, InMemoryTenantStore, TenantStore,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

struct StubTools;

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

async fn build_app() -> (axum::Router, String, Uuid) {
    let tenants: Arc<dyn TenantStore> = Arc::new(InMemoryTenantStore::new());
    let t = tenants
        .create_tenant("acme".into(), None, None)
        .await
        .unwrap();
    let mint = tenants
        .mint_api_key(t.id, vec![ApiKeyScope::ToolsInvoke])
        .await
        .unwrap();
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools);
    let state = AppState::new(tenants, tools);
    (router(state), mint.plaintext, t.id)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn healthz_is_public() {
    let (app, _key, _id) = build_app().await;
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
    let (app, key, tenant_id) = build_app().await;
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
    let (app, _key, _id) = build_app().await;
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
    let (app, _key, _id) = build_app().await;
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
    let (app, key, _id) = build_app().await;
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
    let (app, key, _id) = build_app().await;
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
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools);
    let state = AppState::new(tenants, tools);
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
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools);
    let state = AppState::new(tenants, tools);
    let app = router(state);

    let create_body = json!({"name": "newco"});
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/v1/tenants")
                .header("content-type", "application/json")
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
    let tools: Arc<dyn McpTools + Send + Sync + 'static> = Arc::new(StubTools);
    let state = AppState::new(tenants, tools);
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/admin/v1/api-keys/{}", mint.api_key.id))
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
