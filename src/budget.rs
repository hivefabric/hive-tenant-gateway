//! Per-tenant budget context.
//!
//! Phase 2.0 carries a budget through every tool call but does NOT enforce a
//! running balance — the Honey Ledger isn't here yet. Calls are reserved-and-
//! refunded in-memory only. Phase 2.2 swaps in the Ledger.

use serde::{Deserialize, Serialize};

/// Defaults baked into the tenant record. Used when a request omits a budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetDefaults {
    /// Max credits per call when the request omits one.
    pub max_credits_per_call: u64,
    /// TTL applied to a reservation when the request omits one.
    pub ttl_secs: u64,
}

impl Default for BudgetDefaults {
    fn default() -> Self {
        // Free-plan defaults: enough for a few hundred SLM calls.
        Self {
            max_credits_per_call: 1000,
            ttl_secs: 60,
        }
    }
}

/// One call's budget. Travels in the request body and propagates downstream
/// in the ACP envelope (Phase 2 envelope wrap pending).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetContext {
    pub max_credits: u64,
    pub ttl_secs: u64,
}

impl BudgetContext {
    pub fn from_defaults(d: &BudgetDefaults) -> Self {
        Self {
            max_credits: d.max_credits_per_call,
            ttl_secs: d.ttl_secs,
        }
    }
}
