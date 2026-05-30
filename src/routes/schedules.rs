//! Scheduled tasks for tenants.
//!
//! Tenants can define cron-based tasks that fire a `run_subagent` prompt on a schedule.
//!
//! GET    /v1/me/schedules            list all schedules for this tenant
//! POST   /v1/me/schedules            create a schedule
//! GET    /v1/me/schedules/:id        get one schedule
//! PATCH  /v1/me/schedules/:id        update (title, cron, prompt, enabled)
//! DELETE /v1/me/schedules/:id        delete a schedule
//!
//! All routes require bearer-token auth. Requires `AppState.pg_pool`; returns 503 in dev mode.

use std::str::FromStr;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
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
        .route("/v1/me/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/v1/me/schedules/:id",
            get(get_schedule).patch(update_schedule).delete(delete_schedule),
        )
}

// ── response / request types ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ScheduleView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: String,
    pub cron: String,
    pub task_payload: serde_json::Value,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub title: Option<String>,
    pub cron: String,
    pub prompt: String,
    pub capability_urn: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateScheduleRequest {
    pub title: Option<String>,
    pub cron: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn require_pool(state: &AppState) -> GatewayResult<&sqlx::postgres::PgPool> {
    state.pg_pool.as_ref().ok_or_else(|| {
        GatewayError::Internal(
            "schedules require a Postgres database (DATABASE_URL not set)".to_string(),
        )
    })
}

fn pg_err(ctx: &str, e: sqlx::Error) -> GatewayError {
    GatewayError::Internal(format!("{ctx}: {e}"))
}

/// Parse a 5-field cron expression and compute the next fire time in UTC.
pub fn next_run_at(cron_expr: &str) -> Option<DateTime<Utc>> {
    cron::Schedule::from_str(cron_expr)
        .ok()
        .and_then(|s| s.upcoming(Utc).next())
        .map(|t| t.with_timezone(&Utc))
}

/// Validate a cron expression, returning a descriptive error if invalid.
fn validate_cron(expr: &str) -> GatewayResult<()> {
    cron::Schedule::from_str(expr).map(|_| ()).map_err(|e| {
        GatewayError::Invalid(format!("invalid cron expression '{}': {}", expr, e))
    })
}

fn row_to_view(row: &sqlx::postgres::PgRow) -> ScheduleView {
    ScheduleView {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        title: row.get("title"),
        cron: row.get("cron"),
        task_payload: row.get("task_payload"),
        enabled: row.get("enabled"),
        next_run_at: row.get::<Option<DateTime<Utc>>, _>("next_run_at"),
        last_run_at: row.get::<Option<DateTime<Utc>>, _>("last_run_at"),
        created_at: row.get::<DateTime<Utc>, _>("created_at"),
        updated_at: row.get::<DateTime<Utc>, _>("updated_at"),
    }
}

// ── handlers ───────────────────────────────────────────────────────────────────

/// GET /v1/me/schedules
async fn list_schedules(
    auth: AuthedTenant,
    State(state): State<AppState>,
) -> GatewayResult<Json<Vec<ScheduleView>>> {
    let pool = require_pool(&state)?;
    let rows = sqlx::query(
        r#"
        SELECT id, tenant_id, title, cron, task_payload, enabled,
               next_run_at, last_run_at, created_at, updated_at
        FROM schedules
        WHERE tenant_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(auth.tenant.id)
    .fetch_all(pool)
    .await
    .map_err(|e| pg_err("list_schedules", e))?;

    Ok(Json(rows.iter().map(row_to_view).collect()))
}

/// POST /v1/me/schedules
async fn create_schedule(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Json(req): Json<CreateScheduleRequest>,
) -> GatewayResult<(StatusCode, Json<ScheduleView>)> {
    let pool = require_pool(&state)?;

    validate_cron(&req.cron)?;

    let title = req.title.unwrap_or_else(|| "Scheduled task".to_string());
    let mut payload = serde_json::json!({ "prompt": req.prompt });
    if let Some(urn) = req.capability_urn {
        payload["capability_urn"] = serde_json::Value::String(urn);
    }
    let next = next_run_at(&req.cron);

    let row = sqlx::query(
        r#"
        INSERT INTO schedules (tenant_id, title, cron, task_payload, next_run_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, tenant_id, title, cron, task_payload, enabled,
                  next_run_at, last_run_at, created_at, updated_at
        "#,
    )
    .bind(auth.tenant.id)
    .bind(&title)
    .bind(&req.cron)
    .bind(&payload)
    .bind(next)
    .fetch_one(pool)
    .await
    .map_err(|e| pg_err("create_schedule", e))?;

    tracing::info!(
        tenant_id = %auth.tenant.id,
        schedule_id = %row.get::<Uuid, _>("id"),
        cron = %req.cron,
        "schedule created"
    );

    Ok((StatusCode::CREATED, Json(row_to_view(&row))))
}

/// GET /v1/me/schedules/:id
async fn get_schedule(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(schedule_id): Path<Uuid>,
) -> GatewayResult<Json<ScheduleView>> {
    let pool = require_pool(&state)?;

    let row = sqlx::query(
        r#"
        SELECT id, tenant_id, title, cron, task_payload, enabled,
               next_run_at, last_run_at, created_at, updated_at
        FROM schedules
        WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(schedule_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("get_schedule", e))?
    .ok_or(GatewayError::TenantNotFound)?;

    Ok(Json(row_to_view(&row)))
}

/// PATCH /v1/me/schedules/:id
async fn update_schedule(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(schedule_id): Path<Uuid>,
    Json(req): Json<UpdateScheduleRequest>,
) -> GatewayResult<Json<ScheduleView>> {
    let pool = require_pool(&state)?;

    // Fetch existing to merge partial updates.
    let existing = sqlx::query(
        r#"
        SELECT id, tenant_id, title, cron, task_payload, enabled,
               next_run_at, last_run_at, created_at, updated_at
        FROM schedules
        WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(schedule_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("update_schedule.fetch", e))?
    .ok_or(GatewayError::TenantNotFound)?;

    let new_title: String = req
        .title
        .unwrap_or_else(|| existing.get::<String, _>("title"));

    let new_cron: String = req
        .cron
        .unwrap_or_else(|| existing.get::<String, _>("cron"));
    validate_cron(&new_cron)?;

    // Merge prompt update into existing task_payload.
    let mut payload: serde_json::Value = existing.get("task_payload");
    if let Some(prompt) = req.prompt {
        payload["prompt"] = serde_json::Value::String(prompt);
    }

    let new_enabled: bool = req
        .enabled
        .unwrap_or_else(|| existing.get::<bool, _>("enabled"));

    // Recompute next_run_at when cron changes or schedule is re-enabled.
    let cron_changed = new_cron != existing.get::<String, _>("cron");
    let was_disabled = !existing.get::<bool, _>("enabled");
    let new_next = if cron_changed || (new_enabled && was_disabled) {
        next_run_at(&new_cron)
    } else {
        existing.get::<Option<DateTime<Utc>>, _>("next_run_at")
    };

    let row = sqlx::query(
        r#"
        UPDATE schedules
        SET title = $1, cron = $2, task_payload = $3, enabled = $4,
            next_run_at = $5, updated_at = NOW()
        WHERE id = $6 AND tenant_id = $7
        RETURNING id, tenant_id, title, cron, task_payload, enabled,
                  next_run_at, last_run_at, created_at, updated_at
        "#,
    )
    .bind(&new_title)
    .bind(&new_cron)
    .bind(&payload)
    .bind(new_enabled)
    .bind(new_next)
    .bind(schedule_id)
    .bind(auth.tenant.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| pg_err("update_schedule", e))?
    .ok_or(GatewayError::TenantNotFound)?;

    Ok(Json(row_to_view(&row)))
}

/// DELETE /v1/me/schedules/:id
async fn delete_schedule(
    auth: AuthedTenant,
    State(state): State<AppState>,
    Path(schedule_id): Path<Uuid>,
) -> GatewayResult<Json<serde_json::Value>> {
    let pool = require_pool(&state)?;

    let res = sqlx::query(
        "DELETE FROM schedules WHERE id = $1 AND tenant_id = $2",
    )
    .bind(schedule_id)
    .bind(auth.tenant.id)
    .execute(pool)
    .await
    .map_err(|e| pg_err("delete_schedule", e))?;

    if res.rows_affected() == 0 {
        return Err(GatewayError::TenantNotFound);
    }

    Ok(Json(serde_json::json!({"ok": true})))
}
