use axum::{routing::get, Json, Router};
use serde_json::json;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/v1/healthz", get(healthz))
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({"ok": true}))
}
