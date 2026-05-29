//! Chat history persistence for tenants.
//!
//! GET    /v1/me/chats                list sessions for the authed tenant
//! POST   /v1/me/chats                create a new chat session
//! GET    /v1/me/chats/:id            get session + messages
//! PATCH  /v1/me/chats/:id            update session title
//! DELETE /v1/me/chats/:id            delete session (cascades to messages)
//! POST   /v1/me/chats/:id/messages   append a message, bump session updated_at
//!
//! All routes require bearer-token auth and are scoped to the authenticated tenant.
//! Requires `AppState.pg_pool` (set when DATABASE_URL is configured). Returns 503
//! in dev/in-memory mode.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::AuthedTenant;
use crate::error::{GatewayError, GatewayResult};
use crate::AppState;

// ── public router ─────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/chats", get(list_sessions).post(create_session))
        .route(
            "/v1/me/chats/:id",
            get(get_session).patch(update_session).delete(delete_session),
        )
        .route("/v1/me/chats/:id/messages", post(append_message))
}

// ── response types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SessionSummary {
    id: Uuid,
    title: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    message_count: i64,
}

#[derive(Debug, Serialize)]
struct SessionDetail {
    id: Uuid,
    title: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    messages: Vec<MessageView>,
}

#[derive(Debug, Serialize)]
struct MessageView {
    id: Uuid,
    role: String,
    content: String,
    status: Option<String>,
    created_at: DateTime<Utc>,
}

// ── request types ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSessionRequest {
    title: String,
}

#[derive(Debug, Deserialize)]
struct AppendMessageRequest {
    role: String,
    content: String,
    status: Option<String>,
}

// ── helpers ────────────────────────────────────────────────────────────────────

/// Return the Postgres pool from AppState, or 503 in dev/in-memory mode.
fn require_pool(state: &AppState) -> GatewayResult<&sqlx::postgres::PgPool> {
    state.pg_pool.as_ref().ok_or_else(|| {
        GatewayError::Internal(
            "chat history requires a Postgres database (DATABASE_URL not set)".to_string(),
        )
    })
}

fn pg_err(ctx: &str, e: sqlx::Error) -> GatewayError {
    GatewayError::Internal(format!("{ctx}: {e}"))
}

// ── handlers ───────────────────────────────────────────────────────────────────

/// GET /v1/me/chats
async fn list_sessions(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<Vec<SessionSummary>>> {
    let pool = require_pool(&state)?;
    let rows = sqlx::query(
        r#"
        SELECT
            s.id,
            s.title,
            s.created_at,
            s.updated_at,
            COUNT(m.id) AS message_count
        FROM chat_sessions s
        LEFT JOIN chat_messages m ON m.session_id = s.id
        WHERE s.tenant_id = $1
        GROUP BY s.id
        ORDER BY s.updated_at DESC
        "#,
    )
    .bind(auth.tenant.id)
    .fetch_all(pool)
    .await
    .map_err(|e| pg_err("list_sessions", e))?;

    let sessions = rows
        .iter()
        .map(|r| SessionSummary {
            id: r.get("id"),
            title: r.get("title"),
            created_at: r.get::<DateTime<Utc>, _>("created_at"),
            updated_at: r.get::<DateTime<Utc>, _>("updated_at"),
            message_count: r.get::<i64, _>("message_count"),
        })
        .collect();

    Ok(Json(sessions))
}

/// POST /v1/me/chats
async fn create_session(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> GatewayResult<Json<serde_json::Value>> {
    let pool = require_pool(&state)?;
    let title = req.title.unwrap_or_else(|| "New chat".to_string());

    let row = sqlx::query(
        r#"
        INSERT INTO chat_sessions (tenant_id, title)
        VALUES ($1, $2)
        RETURNING id, title, created_at, updated_at
        "#,
    )
    .bind(auth.tenant.id)
    .bind(&title)
    .fetch_one(pool)
    .await
    .map_err(|e| pg_err("create_session", e))?;

    Ok(Json(serde_json::json!({
        "id": row.get::<Uuid, _>("id"),
        "title": row.get::<String, _>("title"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
    })))
}

/// GET /v1/me/chats/:id
async fn get_session(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> GatewayResult<Json<SessionDetail>> {
    let pool = require_pool(&state)?;

    let session = sqlx::query(
        r#"
        SELECT id, title, created_at, updated_at
        FROM chat_sessions
        WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(session_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("get_session", e))?
    .ok_or(GatewayError::TenantNotFound)?;

    let message_rows = sqlx::query(
        r#"
        SELECT id, role, content, status, created_at
        FROM chat_messages
        WHERE session_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(|e| pg_err("get_session.messages", e))?;

    Ok(Json(SessionDetail {
        id: session.get("id"),
        title: session.get("title"),
        created_at: session.get::<DateTime<Utc>, _>("created_at"),
        updated_at: session.get::<DateTime<Utc>, _>("updated_at"),
        messages: message_rows
            .iter()
            .map(|m| MessageView {
                id: m.get("id"),
                role: m.get("role"),
                content: m.get("content"),
                status: m.get::<Option<String>, _>("status"),
                created_at: m.get::<DateTime<Utc>, _>("created_at"),
            })
            .collect(),
    }))
}

/// PATCH /v1/me/chats/:id
async fn update_session(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<UpdateSessionRequest>,
) -> GatewayResult<Json<serde_json::Value>> {
    let pool = require_pool(&state)?;

    let row = sqlx::query(
        r#"
        UPDATE chat_sessions
        SET title = $1, updated_at = NOW()
        WHERE id = $2 AND tenant_id = $3
        RETURNING id, title, created_at, updated_at
        "#,
    )
    .bind(&req.title)
    .bind(session_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("update_session", e))?
    .ok_or(GatewayError::TenantNotFound)?;

    Ok(Json(serde_json::json!({
        "id": row.get::<Uuid, _>("id"),
        "title": row.get::<String, _>("title"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
    })))
}

/// DELETE /v1/me/chats/:id
async fn delete_session(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> GatewayResult<Json<serde_json::Value>> {
    let pool = require_pool(&state)?;

    let res = sqlx::query(
        "DELETE FROM chat_sessions WHERE id = $1 AND tenant_id = $2",
    )
    .bind(session_id)
    .bind(auth.tenant.id)
    .execute(pool)
    .await
    .map_err(|e| pg_err("delete_session", e))?;

    if res.rows_affected() == 0 {
        return Err(GatewayError::TenantNotFound);
    }

    Ok(Json(serde_json::json!({"ok": true})))
}

/// POST /v1/me/chats/:id/messages
async fn append_message(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<AppendMessageRequest>,
) -> GatewayResult<Json<serde_json::Value>> {
    let pool = require_pool(&state)?;

    // Validate role.
    if req.role != "user" && req.role != "assistant" {
        return Err(GatewayError::Invalid(
            "role must be 'user' or 'assistant'".to_string(),
        ));
    }

    // Verify session belongs to this tenant.
    let exists = sqlx::query(
        "SELECT 1 FROM chat_sessions WHERE id = $1 AND tenant_id = $2",
    )
    .bind(session_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("append_message.check_session", e))?;

    if exists.is_none() {
        return Err(GatewayError::TenantNotFound);
    }

    let msg = sqlx::query(
        r#"
        INSERT INTO chat_messages (session_id, role, content, status)
        VALUES ($1, $2, $3, $4)
        RETURNING id, role, content, status, created_at
        "#,
    )
    .bind(session_id)
    .bind(&req.role)
    .bind(&req.content)
    .bind(&req.status)
    .fetch_one(pool)
    .await
    .map_err(|e| pg_err("append_message.insert", e))?;

    // Bump session updated_at.
    let _ = sqlx::query("UPDATE chat_sessions SET updated_at = NOW() WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await;

    Ok(Json(serde_json::json!({
        "id": msg.get::<Uuid, _>("id"),
        "session_id": session_id,
        "role": msg.get::<String, _>("role"),
        "content": msg.get::<String, _>("content"),
        "status": msg.get::<Option<String>, _>("status"),
        "created_at": msg.get::<DateTime<Utc>, _>("created_at"),
    })))
}
