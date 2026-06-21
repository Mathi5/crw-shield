//! Reactive profile rotation in response to detected anti-bot blocks.
//!
//! ## The L0–L3 ladder
//!
//! | Level | Trigger | Action |
//! |-------|---------|--------|
//! | L0 Accept | HTML looks like real content | Return as-is |
//! | L1 ClearAndRetry | First block on a host | Clear cookies + storage, retry on the same profile (~1 s, no restart) |
//! | L2 Rotate | Second+ block on the same host | 15 s cooldown → kill TLS proxy → spawn new proxy → kill Chromium tree → launch fresh Chromium on the next profile dir (~30–45 s) |
//! | L3 Fail | `MAX_ROTATIONS_PER_HOST` L2s reached on this host | Return HTTP 403, give up |
//!
//! ## Counter state
//!
//! - Rotation counts are tracked **per host**, not globally. A block on
//!   leboncoin doesn't deplete our rotation budget for amazon.
//! - On L1 we do **not** increment the counter — only L2 does. L1 is a
//!   cheap "maybe stale cookies" attempt, not a rotation.
//! - `l1_attempted` is sticky for the process lifetime, so the second
//!   consecutive block on the same host escalates straight to L2.
//!
//! **Adapted from cortex-bridge `src/rotation.rs` (MIT, CyrilLeblanc).**
//! The L0-L3 ladder structure is preserved verbatim; the first L2 jumps
//! to a Firefox profile (NSS vs BoringSSL fingerprint diversity).

use std::time::Duration;

use crate::block_detection::{counter_for, detect, BlockSignal, HostCounters};

/// Maximum number of L2 rotations we'll perform on a single host before
/// failing (L3). Per the user spec: 3.
pub const MAX_ROTATIONS_PER_HOST: u64 = 3;

/// How long to wait between L2 rotations, to simulate "user switched device".
pub const L2_COOLDOWN: Duration = Duration::from_secs(15);

/// What the rotation loop should do next, given the current state.
#[derive(Debug)]
pub enum RotationDecision {
    /// L0: no rotation needed, return the response to the user as-is.
    Accept,
    /// L1: first block on this host. Clear cookies + IndexedDB +
    /// localStorage in the same browser session and retry once. If the
    /// retry succeeds, the user gets the result. If it fails again,
    /// escalate to L2.
    ClearAndRetry {
        signal: BlockSignal,
    },
    /// L2: second+ block. Rotate to the next profile + restart Chrome
    /// + 15 s wait. The caller is expected to invoke the rotation
    /// handler and then re-scrape.
    Rotate {
        signal: BlockSignal,
        next_profile_idx: usize,
    },
    /// L3: we've rotated `MAX_ROTATIONS_PER_HOST` times on this host. Give up.
    Fail {
        signal: BlockSignal,
        rotations_used: u64,
    },
}

/// Decide what to do given an HTML response and our current state.
///
/// # Arguments
///
/// * `html` — the rendered HTML returned by Chrome for this attempt
/// * `title` — extracted page title (used as a stronger block signal)
/// * `host` — bare hostname (used as the per-host counter key)
/// * `profile_idx` — index of the profile that produced this response
///   (used to advance to `idx + 1` on L2)
/// * `counters` — shared per-host rotation state
/// * `total_profiles` — size of the profile pool (used to cycle on L2)
pub fn decide(
    html: &str,
    title: &str,
    host: &str,
    profile_idx: usize,
    counters: &HostCounters,
    total_profiles: usize,
) -> RotationDecision {
    let signal = match detect(html, title) {
        Some(s) => s,
        None => return RotationDecision::Accept,
    };

    let counter = counter_for(host, counters);
    let rotations_used = counter.rotations();

    // L3: too many rotations on this host, give up.
    if rotations_used >= MAX_ROTATIONS_PER_HOST {
        return RotationDecision::Fail {
            signal,
            rotations_used,
        };
    }

    // L1: only on the FIRST block on this host. After we mark L1 attempted,
    // any subsequent block on the same host (even in the same scrape loop)
    // escalates directly to L2 — that's what prevents the infinite L1 loop
    // when clearing cookies doesn't fix the block.
    if !counter.l1_attempted() {
        counter.mark_l1_attempted();
        return RotationDecision::ClearAndRetry { signal };
    }

    // L2: rotate to next profile. First L2 jump goes to a Firefox profile
    // (lower indices in our combined pool) so we hit anti-bot stacks with a
    // completely different TLS stack (NSS vs BoringSSL) before cycling
    // through the Chrome variants. Subsequent L2s cycle normally.
    //
    // Note: this assumes the pool layout is `[chrome_120, chrome_124, ...,
    // firefox_128, firefox_133, firefox_123]` — i.e. the Firefox profiles
    // occupy the last slots. The caller is responsible for that layout.
    let next = if rotations_used == 0 && total_profiles >= 6 {
        // First L2: jump to the first Firefox profile in the pool
        // (5 Chrome profiles, so Firefox starts at index 5).
        5
    } else {
        (profile_idx + 1) % total_profiles
    };
    RotationDecision::Rotate {
        signal,
        next_profile_idx: next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_detection::{BlockKind, HostCounters};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn empty_counters() -> HostCounters {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn clean_html_returns_accept() {
        let counters = empty_counters();
        let html = "<html><body><h1>Real content here</h1></body></html>";
        let decision = decide(html, "Real", "example.com", 0, &counters, 7);
        assert!(matches!(decision, RotationDecision::Accept));
    }

    #[test]
    fn first_block_returns_clear_and_retry() {
        let counters = empty_counters();
        let html = "<html><body>Just a moment... cf-ray challenge</body></html>";
        let decision = decide(html, "Just a moment...", "blocked.com", 0, &counters, 7);
        assert!(matches!(decision, RotationDecision::ClearAndRetry { .. }));
        // After L1, counter is marked
        let c = counter_for("blocked.com", &counters);
        assert!(c.l1_attempted());
    }

    #[test]
    fn second_block_returns_rotate() {
        let counters = empty_counters();
        let html = "<html><body>Just a moment...</body></html>";
        // First call: L1
        let _ = decide(html, "Just a moment...", "blocked.com", 0, &counters, 7);
        // Second call: L2
        let decision = decide(html, "Just a moment...", "blocked.com", 0, &counters, 7);
        match decision {
            RotationDecision::Rotate { next_profile_idx, .. } => {
                // First L2 should jump to index 5 (Firefox)
                assert_eq!(next_profile_idx, 5);
            }
            other => panic!("expected Rotate, got {other:?}"),
        }
    }

    #[test]
    fn max_rotations_returns_fail() {
        let counters = empty_counters();
        let html = "<html><body>Just a moment...</body></html>";
        // Mark L1 attempted + record MAX rotations
        let c = counter_for("blocked.com", &counters);
        c.mark_l1_attempted();
        for _ in 0..MAX_ROTATIONS_PER_HOST {
            c.record_rotation();
        }
        let decision = decide(html, "Just a moment...", "blocked.com", 0, &counters, 7);
        match decision {
            RotationDecision::Fail { rotations_used, .. } => {
                assert_eq!(rotations_used, MAX_ROTATIONS_PER_HOST);
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn rotation_jumps_to_firefox_on_first_l2() {
        let counters = empty_counters();
        // Use a Cloudflare-IUAM-shaped payload so `detect()` returns
        // Some(...). With a tiny/HTML-looking body, `detect` returns None
        // (looks_like_html bypasses the Empty branch) and `decide` would
        // resolve to Accept — never Rotate.
        let html = "<html><body>cf-mitigated Just a moment...</body></html>";
        let c = counter_for("blocked.com", &counters);
        c.mark_l1_attempted();
        // First L2
        let decision = decide(html, "Just a moment...", "blocked.com", 0, &counters, 7);
        match decision {
            RotationDecision::Rotate {
                next_profile_idx,
                ..
            } => {
                assert_eq!(next_profile_idx, 5, "should jump to Firefox");
            }
            other => panic!("expected Rotate, got {other:?}"),
        }
    }

    #[test]
    fn empty_block_signature_returns_empty_block_kind() {
        // Tiny body with no HTML signature → BlockKind::Empty
        let counters = empty_counters();
        let decision = decide("blocked", "", "blocked.com", 0, &counters, 7);
        match decision {
            RotationDecision::ClearAndRetry { signal } => {
                assert_eq!(signal.kind, BlockKind::Empty);
            }
            _ => panic!("expected ClearAndRetry"),
        }
    }
}