//! Anti-bot block detection.
//!
//! Given the HTML, title, and URL of a scraped page, decide whether the site
//! served us a "real" page or a block / challenge / captcha page.
//!
//! This is necessarily fuzzy — sites evolve, and a hard-coded pattern list
//! will miss new variants. The strategy here is to combine several signals
//! (title keywords + body keywords + length heuristics + well-known block
//! page signatures) and return a confidence level, not a binary.
//!
//! Used by `crates/antibot/src/rotation.rs` to decide whether to:
//! - Accept the response as-is
//! - Trigger L1 (clear cookies + retry)
//! - Trigger L2 (rotate profile + restart Chrome + 15s wait)
//! - Trigger L3 (fail after N rotations on the same host)
//!
//! **Adapted from cortex-bridge `src/block_detection.rs` (MIT, CyrilLeblanc).**
//! The pattern list and scoring weights are tuned to the same thresholds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Score above which we flag a response as a block. `0.7` is empirically
/// tuned: it suppresses false positives on benign pages that happen to
/// contain one of our lower-weight phrases.
pub const BLOCK_THRESHOLD: f32 = 0.7;

/// Minimum content size below which we treat the response as suspicious
/// (`BlockKind::Empty` with confidence 0.5). High enough to skip real
/// homepages like `example.com` (~1.3 KB) is out of reach at this
/// threshold — see `docs/known-limitations.md` in cortex-bridge.
pub const EMPTY_THRESHOLD_BYTES: usize = 600;

/// How many occurrences of "sign in" / "log in" a body must contain
/// before we flag it as a login wall without matching one of the
/// explicit LoginWall patterns. 3 catches the common case of nav +
/// form + footer all repeating the phrase, while staying clear of
/// articles that legitimately mention "sign in" once or twice.
const LOGIN_WALL_SIGN_IN_THRESHOLD: usize = 3;

/// Which anti-bot system we're pretty sure served the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    /// "Just a moment...", `cf-ray` challenge
    Cloudflare,
    /// "Access to this page has been denied", "Press & Hold"
    PerimeterX,
    /// `datadome` cookie + JS challenge
    Datadome,
    /// `/_sec/verify?provider=interstitial`, `awswaf-token`
    AwsWaf,
    /// Generic captcha
    GenericCaptcha,
    /// 200 OK but page is suspiciously tiny
    Empty,
    /// Login wall — page renders a "Sign in" gate (twitter.com,
    /// linkedin.com, ...) instead of the requested content. Not a
    /// captcha/anti-bot challenge — the user needs to provide
    /// credentials, not solve a puzzle.
    LoginWall,
}

/// Detection result with a confidence level.
#[derive(Debug, Clone)]
pub struct BlockSignal {
    pub kind: BlockKind,
    /// 0.0 .. 1.0 — higher = more confident it's a block.
    pub confidence: f32,
    /// First matched phrase, for debugging / logs. Currently not consumed
    /// by the rotation loop (only `kind` + `confidence` are) but kept
    /// populated for future structured logging / metrics.
    pub matched: String,
}

/// Hard-coded patterns. Each has a weight added to the score for matching
/// titles/bodies. We accept as block if total score exceeds `BLOCK_THRESHOLD`.
struct Pattern {
    kind: BlockKind,
    /// If matched in title, add this to score.
    title_weight: f32,
    /// If matched in body, add this to score.
    body_weight: f32,
    phrase: &'static str,
}

const PATTERNS: &[Pattern] = &[
    // Cloudflare
    Pattern {
        kind: BlockKind::Cloudflare,
        title_weight: 0.9,
        body_weight: 0.3,
        phrase: "Just a moment",
    },
    Pattern {
        kind: BlockKind::Cloudflare,
        title_weight: 0.6,
        body_weight: 0.4,
        phrase: "cf-ray",
    },
    Pattern {
        kind: BlockKind::Cloudflare,
        title_weight: 0.6,
        body_weight: 0.4,
        phrase: "cf-browser-verification",
    },
    // PerimeterX
    Pattern {
        kind: BlockKind::PerimeterX,
        title_weight: 0.95,
        body_weight: 0.3,
        phrase: "Access to this page has been denied",
    },
    Pattern {
        kind: BlockKind::PerimeterX,
        title_weight: 0.7,
        body_weight: 0.5,
        phrase: "Press & Hold",
    },
    Pattern {
        kind: BlockKind::PerimeterX,
        title_weight: 0.7,
        body_weight: 0.5,
        phrase: "Press and Hold",
    },
    Pattern {
        kind: BlockKind::PerimeterX,
        title_weight: 0.6,
        body_weight: 0.4,
        phrase: "_pxAppId",
    },
    // Datadome
    Pattern {
        kind: BlockKind::Datadome,
        title_weight: 0.0,
        body_weight: 0.7,
        phrase: "datadome",
    },
    Pattern {
        kind: BlockKind::Datadome,
        title_weight: 0.0,
        body_weight: 0.5,
        phrase: "dd.leboncoin.fr",
    },
    // AWS WAF
    Pattern {
        kind: BlockKind::AwsWaf,
        title_weight: 0.0,
        body_weight: 0.9,
        phrase: "awswaf-token",
    },
    Pattern {
        kind: BlockKind::AwsWaf,
        title_weight: 0.0,
        body_weight: 0.7,
        phrase: "/_sec/verify",
    },
    Pattern {
        kind: BlockKind::AwsWaf,
        title_weight: 0.6,
        body_weight: 0.3,
        phrase: "AWS WAF",
    },
    // Generic captcha
    Pattern {
        kind: BlockKind::GenericCaptcha,
        title_weight: 0.7,
        body_weight: 0.3,
        phrase: "Robot Check",
    },
    Pattern {
        kind: BlockKind::GenericCaptcha,
        title_weight: 0.6,
        body_weight: 0.4,
        phrase: "verify you are human",
    },
    Pattern {
        kind: BlockKind::GenericCaptcha,
        title_weight: 0.5,
        body_weight: 0.5,
        phrase: "captcha",
    },
    Pattern {
        kind: BlockKind::GenericCaptcha,
        title_weight: 0.5,
        body_weight: 0.5,
        phrase: "I am human",
    },
    // Login wall — the page renders a "Sign in" / "Log in" gate instead
    // of the content the user asked for (twitter.com, linkedin.com,
    // instagram.com, ...). Bug-fix v0.4.2: these used to be returned as
    // `success=true` with the navigation extracted as markdown, which
    // looks like a successful scrape to the caller but contains zero
    // useful content.
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.7,
        body_weight: 0.5,
        phrase: "Sign in or Sign up",
    },
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.6,
        body_weight: 0.5,
        phrase: "Log in to continue",
    },
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.6,
        body_weight: 0.4,
        phrase: "Continue with Google",
    },
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.0,
        body_weight: 0.7,
        phrase: "authwall",
    },
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.5,
        body_weight: 0.5,
        phrase: "Sign in to your account",
    },
    Pattern {
        kind: BlockKind::LoginWall,
        title_weight: 0.5,
        body_weight: 0.5,
        phrase: "Log in or Sign up",
    },
];

/// Run the block-detection pass over an HTML response + its extracted title.
///
/// Returns `Some(BlockSignal)` if any pattern scored above `BLOCK_THRESHOLD`,
/// or if the body is under `EMPTY_THRESHOLD_BYTES` (in which case the
/// returned `confidence` is fixed at 0.5). Returns `None` if the response
/// looks like real content.
pub fn detect(html: &str, title: &str) -> Option<BlockSignal> {
    let title_lc = title.to_lowercase();
    let html_lc = html.to_lowercase();

    let mut best: Option<BlockSignal> = None;

    for p in PATTERNS {
        let mut score = 0.0;
        let mut matched = String::new();
        let phrase_lc = p.phrase.to_lowercase();

        if title_lc.contains(&phrase_lc) {
            score += p.title_weight;
            matched = format!("title contains {:?}", p.phrase);
        } else if html_lc.contains(&phrase_lc) {
            score += p.body_weight;
            matched = format!("body contains {:?}", p.phrase);
        }

        if score >= BLOCK_THRESHOLD {
            let sig = BlockSignal {
                kind: p.kind.clone(),
                confidence: score,
                matched,
            };
            match &best {
                Some(b) if b.confidence >= sig.confidence => {}
                _ => best = Some(sig),
            }
        }
    }

    // Empty-page heuristic: 200 OK but content is tiny → likely block or
    // redirect-to-login. Don't trigger rotation just on this (lots of legit
    // small pages), but flag it.
    //
    // Exception: if the body contains an HTML signature (`<!DOCTYPE` /
    // `<html`), it's real content even when small (e.g. example.com is
    // ~1.3 KB). Only flag when the body is *both* small AND has no HTML
    // structure — that's the block-page signature.
    if best.is_none() && html.len() < EMPTY_THRESHOLD_BYTES && !looks_like_html(html) {
        best = Some(BlockSignal {
            kind: BlockKind::Empty,
            confidence: 0.5,
            matched: format!(
                "page is {} bytes (threshold {})",
                html.len(),
                EMPTY_THRESHOLD_BYTES
            ),
        });
    }

    // Bug-fix v0.4.2: login-wall heuristic. Many login walls don't match
    // a single specific phrase but DO repeat "Sign in" / "Log in" many
    // times across nav + form + footer. If the page mentions "sign in"
    // (or close variants) more than `LOGIN_WALL_SIGN_IN_THRESHOLD` times,
    // flag as LoginWall even if no single pattern triggered.
    if !matches!(
        best,
        Some(BlockSignal {
            kind: BlockKind::LoginWall,
            ..
        })
    ) {
        let sign_in_count = html_lc.matches("sign in").count() + html_lc.matches("log in").count();
        if sign_in_count >= LOGIN_WALL_SIGN_IN_THRESHOLD {
            best = Some(BlockSignal {
                kind: BlockKind::LoginWall,
                confidence: 0.6,
                matched: format!(
                    "found {sign_in_count} occurrences of 'sign in' / 'log in' (threshold {LOGIN_WALL_SIGN_IN_THRESHOLD})"
                ),
            });
        }
    }

    best
}

/// Cheap HTML signature check for the Empty heuristic. Matches `<!doctype`
/// or `<html` (case-insensitive) within the first 512 bytes — covers both
/// `<!DOCTYPE html>` and the shorter `<html><head>…` form some sites use.
/// Returns true if either is found, meaning the body is structured HTML
/// (even if tiny) and shouldn't be flagged as an empty/block page.
fn looks_like_html(html: &str) -> bool {
    let head = &html[..html.len().min(512)];
    let head_lc = head.to_lowercase();
    head_lc.contains("<!doctype") || head_lc.contains("<html")
}

// =============================================================================
// Per-host rotation state
// =============================================================================

/// Tracks how many times we've been blocked on a given host, so the rotation
/// logic can decide between L1 (clear cookies), L2 (rotate profile), and
/// L3 (give up after N total rotations).
#[derive(Default)]
pub struct HostBlockCounter {
    /// Total L2 rotations performed on this host since process start.
    rotations: AtomicU64,
    /// Whether we've already tried L1 (clear-cookies) on this host. Sticky
    /// for the process lifetime so that a retry after L1 always escalates
    /// to L2 if it still hits a block.
    l1_attempted: AtomicBool,
    /// Last L2 rotation time, for cooldown calculation.
    last_rotation_unix_ms: AtomicU64,
}

impl HostBlockCounter {
    /// Total L2 rotations performed on this host since process start.
    pub fn rotations(&self) -> u64 {
        self.rotations.load(Ordering::SeqCst)
    }

    /// Whether L1 (clear-cookies) has already been tried on this host.
    pub fn l1_attempted(&self) -> bool {
        self.l1_attempted.load(Ordering::SeqCst)
    }

    /// Mark L1 (clear-cookies) as attempted. Sticky for process lifetime.
    pub fn mark_l1_attempted(&self) {
        self.l1_attempted.store(true, Ordering::SeqCst);
    }

    /// Record a new L2 rotation. Increments `rotations` and updates the
    /// `last_rotation_unix_ms` timestamp.
    pub fn record_rotation(&self) {
        self.rotations.fetch_add(1, Ordering::SeqCst);
        self.last_rotation_unix_ms.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            Ordering::SeqCst,
        );
    }

    /// Milliseconds elapsed since the last L2 rotation, or `None` if none.
    pub fn ms_since_last_rotation(&self) -> Option<u64> {
        let last = self.last_rotation_unix_ms.load(Ordering::SeqCst);
        if last == 0 {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Some(now.saturating_sub(last))
    }
}

/// Process-wide map of hostname → `HostBlockCounter`. Cheap to clone (`Arc`).
pub type HostCounters = Arc<Mutex<HashMap<String, Arc<HostBlockCounter>>>>;

/// Get-or-create the counter for a host.
pub fn counter_for(host: &str, counters: &HostCounters) -> Arc<HostBlockCounter> {
    let mut map = counters.lock().unwrap();
    map.entry(host.to_string())
        .or_insert_with(|| Arc::new(HostBlockCounter::default()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_clean_page_returns_none() {
        let html = "<html><body><h1>Hello world</h1><p>Some content here.</p></body></html>";
        assert!(detect(html, "Hello world").is_none());
    }

    #[test]
    fn detect_cloudflare_challenge_in_title() {
        let html = "<html><body>...</body></html>";
        let signal = detect(html, "Just a moment...").unwrap();
        assert_eq!(signal.kind, BlockKind::Cloudflare);
        assert!(signal.confidence >= BLOCK_THRESHOLD);
    }

    #[test]
    fn detect_datadome_in_body() {
        let html = "<html><body>Some datadome protected content.</body></html>";
        let signal = detect(html, "").unwrap();
        assert_eq!(signal.kind, BlockKind::Datadome);
    }

    #[test]
    fn empty_page_with_html_signature_is_not_flagged() {
        // example.com is small but has <!doctype> — must not be flagged.
        let html = "<!doctype html><html><body>example</body></html>";
        assert!(detect(html, "Example Domain").is_none());
    }

    #[test]
    fn tiny_non_html_body_is_flagged_empty() {
        let html = "blocked";
        let signal = detect(html, "").unwrap();
        assert_eq!(signal.kind, BlockKind::Empty);
        assert_eq!(signal.confidence, 0.5);
    }

    #[test]
    fn host_counter_starts_zero() {
        let counters: HostCounters = Arc::new(Mutex::new(HashMap::new()));
        let c = counter_for("example.com", &counters);
        assert_eq!(c.rotations(), 0);
        assert!(!c.l1_attempted());
    }

    #[test]
    fn host_counter_l1_sticky() {
        let counters: HostCounters = Arc::new(Mutex::new(HashMap::new()));
        let c = counter_for("example.com", &counters);
        c.mark_l1_attempted();
        assert!(c.l1_attempted());
    }

    #[test]
    fn host_counter_rotation_increments() {
        let counters: HostCounters = Arc::new(Mutex::new(HashMap::new()));
        let c = counter_for("example.com", &counters);
        c.record_rotation();
        c.record_rotation();
        assert_eq!(c.rotations(), 2);
        assert!(c.ms_since_last_rotation().is_some());
    }

    // -------- BUG D regression tests (v0.4.2) -------------------------------
    //
    // Login-wall pages (twitter.com, linkedin.com, ...) used to be returned
    // as `success=true` with the navigation extracted as markdown. The fix
    // adds a `BlockKind::LoginWall` variant and 6 phrase patterns, plus a
    // fallback heuristic that flags bodies repeating "sign in" / "log in"
    // more than 3 times.

    #[test]
    fn login_wall_detected_by_sign_in_or_sign_up_pattern() {
        let html = r#"<!doctype html><html><head><title>Sign in or Sign up</title></head>
            <body><h1>Sign in or Sign up to X</h1><p>Welcome.</p></body></html>"#;
        let sig = detect(html, "Sign in or Sign up - X").expect("login wall should be detected");
        assert_eq!(sig.kind, BlockKind::LoginWall);
        assert!(sig.confidence >= BLOCK_THRESHOLD);
    }

    #[test]
    fn login_wall_detected_by_repeated_sign_in_phrase() {
        // No explicit "Sign in or Sign up" pattern match, but the body
        // repeats "Sign in" across nav, form, and footer — the
        // heuristic should catch it.
        let html = r#"<!doctype html><html><head><title>Home</title></head>
            <body>
            <nav><a>Sign in</a></nav>
            <form><button>Sign in</button></form>
            <footer><a>Sign in</a> | <a>Log in</a></footer>
            </body></html>"#;
        let sig = detect(html, "Home").expect("login wall heuristic should fire");
        assert_eq!(sig.kind, BlockKind::LoginWall);
    }

    #[test]
    fn real_article_not_detected_as_login_wall() {
        let html = r#"<!doctype html><html><head><title>Article</title></head>
            <body><article><h1>Lorem ipsum dolor sit amet</h1>
            <p>Consectetur adipiscing elit, sed do eiusmod tempor incididunt
            ut labore et dolore magna aliqua. Ut enim ad minim veniam.</p>
            </article></body></html>"#;
        let sig = detect(html, "Article");
        // Either None or a non-LoginWall signal is acceptable — the key
        // is we don't false-positive on real content.
        if let Some(s) = sig {
            assert_ne!(
                s.kind,
                BlockKind::LoginWall,
                "real article body must not be flagged as login wall"
            );
        }
    }
}
