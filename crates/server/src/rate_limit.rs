//! Per-server request rate limiter with min_interval + random jitter.
//!
//! Cortex-bridge reference: `src/main.rs:81-82` + `crate::rate_limit::RateLimiter`.
//! Ported to crw-shield as a small, focused module — no external deps beyond
//! `tokio` + `tracing`.
//!
//! Behaviour:
//! - If `min_interval == 0`, `wait().await` is a no-op (jitter-only mode).
//! - Otherwise the limiter records the timestamp of the last `wait()` and
//!   blocks until at least `min_interval + random(0..max_jitter)` has elapsed
//!   since that timestamp.
//! - First call always returns immediately (no previous timestamp to wait for).
//!
//! Ported from `cortex-bridge/src/rate_limit.rs` (MIT, CyrilLeblanc/cortex-bridge,
//! abba6bf). The cortex-bridge version also handles "global vs per-host" —
//! crw-shield keeps it global for now (simpler, sufficient for our benchmarks).

use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::debug;

/// Per-server rate limiter: min interval between scrapes + random jitter.
#[derive(Debug)]
pub struct RateLimiter {
    inner: Mutex<RateLimiterInner>,
    min_interval: Duration,
    max_jitter: Duration,
}

#[derive(Debug)]
struct RateLimiterInner {
    last_call: Option<Instant>,
}

impl RateLimiter {
    /// Build a new rate limiter. `max_jitter = 0` disables the random sleep
    /// (deterministic min_interval only).
    pub fn new(min_interval: Duration, max_jitter: Duration) -> Self {
        Self {
            inner: Mutex::new(RateLimiterInner { last_call: None }),
            min_interval,
            max_jitter,
        }
    }

    /// No-op rate limiter (every `wait().await` returns immediately).
    pub fn disabled() -> Self {
        Self::new(Duration::ZERO, Duration::ZERO)
    }

    /// Wait at least `min_interval + random(0..max_jitter)` since the last
    /// successful wait. First call returns immediately. If the elapsed time
    /// already exceeds `min_interval`, no sleep occurs.
    pub async fn wait(&self) {
        // Early-out without acquiring the lock if both intervals are zero
        // (cheapest path for tests and the disabled() case).
        if self.min_interval.is_zero() && self.max_jitter.is_zero() {
            return;
        }

        let jitter = if self.max_jitter.is_zero() {
            Duration::ZERO
        } else {
            let nanos = self.max_jitter.as_nanos() as u64;
            if nanos == 0 {
                Duration::ZERO
            } else {
                // Cheap pseudo-random without pulling a `rand` dep. The
                // SystemTime nanos + a hash of the address space is enough
                // for jitter distribution purposes — anti-bot heuristics
                // don't care about cryptographic randomness here.
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0)
                    ^ (std::process::id() as u64);
                Duration::from_nanos(seed % nanos)
            }
        };

        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        let elapsed = inner.last_call.map(|t| now.duration_since(t));
        let required = self.min_interval + jitter;

        match elapsed {
            Some(e) if e < required => {
                let sleep_for = required - e;
                debug!(
                    sleep_ms = sleep_for.as_millis() as u64,
                    "rate limiter sleeping"
                );
                drop(inner); // don't hold the lock during the sleep
                tokio::time::sleep(sleep_for).await;
                let mut inner = self.inner.lock().await;
                inner.last_call = Some(Instant::now());
            }
            _ => {
                inner.last_call = Some(now);
            }
        }
    }

    /// Configure from env vars with sensible defaults. Used by `AppState`.
    /// - `RATE_LIMIT_MIN_MS` (default `2000`): minimum interval between scrapes.
    /// - `RATE_LIMIT_JITTER_MS` (default `500`): max random extra sleep.
    /// - Set either to `0` to disable that component.
    pub fn from_env() -> Self {
        let min_ms: u64 = std::env::var("RATE_LIMIT_MIN_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        let jitter_ms: u64 = std::env::var("RATE_LIMIT_JITTER_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);
        Self::new(
            Duration::from_millis(min_ms),
            Duration::from_millis(jitter_ms),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[tokio::test]
    async fn disabled_limiter_is_instant() {
        let l = RateLimiter::disabled();
        let start = Instant::now();
        // run 10 waits back-to-back; should take < 10ms total
        for _ in 0..10 {
            l.wait().await;
        }
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn first_wait_returns_immediately() {
        let l = RateLimiter::new(Duration::from_millis(2000), Duration::ZERO);
        let start = Instant::now();
        l.wait().await;
        // First call should not block
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn second_wait_respects_min_interval() {
        let l = RateLimiter::new(Duration::from_millis(100), Duration::ZERO);
        let start = Instant::now();
        l.wait().await;
        l.wait().await;
        // Two waits should take at least 100ms (the min interval)
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(100),
            "expected >= 100ms, got {}ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn jitter_adds_random_extra_within_range() {
        // 100ms min + up to 50ms jitter
        let l = RateLimiter::new(Duration::from_millis(100), Duration::from_millis(50));
        let start = Instant::now();
        l.wait().await;
        l.wait().await;
        let elapsed = start.elapsed();
        // Should be between 100ms (min) and 100+50+epsilon = 200ms
        assert!(
            elapsed >= Duration::from_millis(100),
            "elapsed {}ms < min",
            elapsed.as_millis()
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "elapsed {}ms exceeds min+jitter+slack",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn no_sleep_when_elapsed_exceeds_min() {
        let l = RateLimiter::new(Duration::from_millis(10), Duration::ZERO);
        let _start = Instant::now();
        l.wait().await;
        // Wait longer than the min interval
        tokio::time::sleep(Duration::from_millis(50)).await;
        let before_second_wait = Instant::now();
        l.wait().await;
        let after_second_wait = Instant::now();
        // The second wait should be near-instant because the elapsed
        // time since the first wait (>= 50ms) exceeds the 10ms min interval.
        // Allow some scheduling overhead.
        assert!(
            after_second_wait.duration_since(before_second_wait) < Duration::from_millis(20),
            "second wait slept too long: {}ms",
            after_second_wait
                .duration_since(before_second_wait)
                .as_millis()
        );
    }
}
