//! Thin HTTP client to the Honey Ledger service.
//!
//! Used by the run_subagent path to record per-call credit events:
//!   * `debit` before dispatch (best-effort; failure logs but doesn't
//!     block the call — the demo prefers progress over strict billing)
//!   * `refund` if the underlying tool call failed (so the tenant
//!     doesn't lose credits to upstream errors)
//!
//! Production-grade billing would gate dispatch on `debit` returning a
//! non-negative balance and would use a reservation/release pair instead
//! of debit-then-maybe-refund. That's the next iteration; this one wires
//! the audit trail.

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// One ledger event request body.
#[derive(Debug, Serialize)]
struct EventRequest {
    tenant_id: Uuid,
    amount_credits: u64,
    correlation: Option<String>,
    idempotency_key: Option<String>,
    metadata: Option<Value>,
}

#[derive(Clone)]
pub struct LedgerClient {
    base_url: String,
    http: reqwest::Client,
}

impl LedgerClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest build"),
        }
    }

    pub async fn debit(
        &self,
        tenant_id: Uuid,
        amount: u64,
        correlation: &str,
        idempotency_key: &str,
        metadata: Value,
    ) -> Result<Value, String> {
        self.post("debit", tenant_id, amount, Some(correlation), Some(idempotency_key), metadata)
            .await
    }

    /// Fetch the most recent credit events for a tenant (up to `limit`).
    pub async fn events(&self, tenant_id: Uuid, limit: u32) -> Result<Vec<serde_json::Value>, String> {
        let url = format!(
            "{}/v1/credits/{}/events?limit={}",
            self.base_url, tenant_id, limit
        );
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("ledger events request: {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("ledger events body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "ledger events rejected ({status}): {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("ledger events parse: {e}"))?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn balance(&self, tenant_id: Uuid) -> Result<i64, String> {
        let url = format!("{}/v1/credits/{}/balance", self.base_url, tenant_id);
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("ledger balance request: {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("ledger balance body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "ledger balance rejected ({status}): {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("ledger balance parse: {e}"))?;
        v.get("balance")
            .and_then(|b| b.as_i64())
            .ok_or_else(|| "ledger balance missing 'balance' field".to_string())
    }

    pub async fn refund(
        &self,
        tenant_id: Uuid,
        amount: u64,
        correlation: &str,
        idempotency_key: &str,
        metadata: Value,
    ) -> Result<Value, String> {
        self.post("refund", tenant_id, amount, Some(correlation), Some(idempotency_key), metadata)
            .await
    }

    async fn post(
        &self,
        op: &str,
        tenant_id: Uuid,
        amount: u64,
        correlation: Option<&str>,
        idempotency_key: Option<&str>,
        metadata: Value,
    ) -> Result<Value, String> {
        let body = EventRequest {
            tenant_id,
            amount_credits: amount,
            correlation: correlation.map(String::from),
            idempotency_key: idempotency_key.map(String::from),
            metadata: Some(metadata),
        };
        let url = format!("{}/v1/credits/{}", self.base_url, op);
        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("ledger {op} request: {e}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("ledger {op} body: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "ledger {op} rejected ({status}): {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("ledger {op} parse: {e}"))
    }
}
