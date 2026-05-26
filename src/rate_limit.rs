//! Per-tenant token-bucket rate limiter.
//!
//! Uses a fixed-window counter per tenant. Configured via:
//!   TENANT_RATE_LIMIT_RPM   — max requests per minute (default: 300)
//!
//! The limiter is shared across all request handlers via AppState.
//! When a tenant exceeds their limit the request is rejected with 429
//! and a `Retry-After: 60` header.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use uuid::Uuid;

struct Window {
    count: u32,
    reset_at: Instant,
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<Uuid, Window>>>,
    max_per_window: u32,
    window_size: Duration,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, window_size: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
            window_size,
        }
    }

    /// Build from `TENANT_RATE_LIMIT_RPM` env var, defaulting to 300 rpm.
    pub fn from_env() -> Self {
        let rpm = std::env::var("TENANT_RATE_LIMIT_RPM")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(300);
        Self::new(rpm, Duration::from_secs(60))
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    pub fn check(&self, tenant_id: Uuid) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        let window = map.entry(tenant_id).or_insert_with(|| Window {
            count: 0,
            reset_at: now + self.window_size,
        });

        if now >= window.reset_at {
            window.count = 0;
            window.reset_at = now + self.window_size;
        }

        if window.count >= self.max_per_window {
            return false;
        }
        window.count += 1;
        true
    }

    /// Seconds until the current window resets for `tenant_id`.
    /// Returns 60 if no window exists (safe default for Retry-After header).
    pub fn retry_after_secs(&self, tenant_id: Uuid) -> u64 {
        let now = Instant::now();
        let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.get(&tenant_id)
            .map(|w| {
                w.reset_at
                    .checked_duration_since(now)
                    .map(|d| d.as_secs().max(1))
                    .unwrap_or(1)
            })
            .unwrap_or(60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn allows_requests_under_limit() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        let id = Uuid::new_v4();
        for _ in 0..5 {
            assert!(limiter.check(id), "should allow request under limit");
        }
    }

    #[test]
    fn blocks_request_over_limit() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let id = Uuid::new_v4();
        for _ in 0..3 {
            limiter.check(id);
        }
        assert!(!limiter.check(id), "should block 4th request");
    }

    #[test]
    fn different_tenants_are_independent() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        limiter.check(a);
        limiter.check(a);
        // a is now at limit; b should still be allowed
        assert!(!limiter.check(a), "a should be blocked");
        assert!(limiter.check(b), "b should still be allowed");
    }
}
