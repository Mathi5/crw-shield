//! Challenge detection — identifies anti-bot providers from HTML payloads.
//!
//! In Phase 1 we only detect (so the scrape can fail gracefully); bypass /
//! rendering through CDP happens in Phase 2.
//!
//! **Phase B**: this module is now a thin compatibility shim over the
//! structured `situation::diagnose` function. New code should call
//! `situation::diagnose` directly to get a full `SituationReport` with
//! evidence and a suggested ladder step. The old `detect_challenge` /
//! `detect_empty_or_blocked` functions are kept so that ladder.rs (and
//! other callers) don't need to change immediately.

use crate::situation::{diagnose, SituationKind};

/// Returns the human-readable name of the detected challenge provider, or
/// `None` if no anti-bot fingerprint is found in the HTML. The set of
/// recognised names has been extended in Phase B but the return type is
/// unchanged for backward compatibility.
pub fn detect_challenge(html: &str) -> Option<String> {
    let report = diagnose(html, None, None);
    if report.is_anti_bot() {
        // Map back to the legacy provider names so existing logs / tests
        // keep working. New `data-*` attributes in API output use
        // `kind.as_str()` directly.
        Some(report.kind.as_str().to_string())
    } else {
        None
    }
}

/// Returns `true` when the HTML body looks like a "blocked but undetected" page —
/// a tiny payload, an obvious anti-bot landing page, or a SPA that hasn't
/// rendered its real content yet. Used by the FetchLadder to decide whether
/// CDP / FlareSolverr should take over even when `detect_challenge` returned
/// `None`.
pub fn detect_empty_or_blocked(html: &str) -> bool {
    let report = diagnose(html, None, None);
    matches!(
        report.kind,
        SituationKind::JsOnly
            | SituationKind::ServerError
            | SituationKind::RateLimited
            | SituationKind::SoftNotFound
    ) || (report.kind != SituationKind::CleanSuccess && report.kind.is_anti_bot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cloudflare() {
        assert_eq!(
            detect_challenge("<script src='https://challenges.cloudflare.com/...' ></script>"),
            Some("cloudflare_iuam".to_string())
        );
        assert_eq!(
            detect_challenge("var __cf_chl_jschl_tk__ = 'abc';"),
            Some("cloudflare_iuam".to_string())
        );
    }

    #[test]
    fn detects_hcaptcha() {
        assert_eq!(
            detect_challenge("<div class='h-captcha'></div>"),
            Some("hcaptcha".to_string())
        );
        assert_eq!(
            detect_challenge("https://hcaptcha.com/1/api.js"),
            Some("hcaptcha".to_string())
        );
    }

    #[test]
    fn detects_recaptcha() {
        assert_eq!(
            detect_challenge("grecaptcha.render('container')"),
            Some("recaptcha".to_string())
        );
    }

    #[test]
    fn detects_perimeterx() {
        assert_eq!(
            detect_challenge("px-captcha iframe loaded"),
            Some("perimeterx".to_string())
        );
    }

    #[test]
    fn detects_datadome() {
        assert_eq!(
            detect_challenge("datadome.co challenge page"),
            Some("datadome_captcha".to_string())
        );
    }

    #[test]
    fn returns_none_for_clean_html() {
        let html = "<html><head><title>Hi</title></head><body><p>Hello</p></body></html>";
        assert_eq!(detect_challenge(html), None);
    }

    #[test]
    fn case_insensitive() {
        let html = "<HTML><BODY>CHALLENGES.CLOUDFLARE.COM</BODY></HTML>";
        assert_eq!(detect_challenge(html), Some("cloudflare_iuam".to_string()));
    }

    #[test]
    fn empty_or_blocked_detects_small_payload() {
        let html = "<html><body>nope</body></html>";
        assert!(detect_empty_or_blocked(html));
        // Just above the 200-char threshold should not be flagged.
        let padded = format!("<html><body>{}</body></html>", "x".repeat(300));
        assert!(!detect_empty_or_blocked(&padded));
    }

    #[test]
    fn empty_or_blocked_detects_incapsula_page() {
        let html = r#"<!DOCTYPE html><html><body>
            <h1>Pardon Our Interruption</h1>
            <p>You have been blocked by the network security.</p>
            <script src="/_Incapsula_Resource"></script>
        </body></html>"#;
        assert!(detect_empty_or_blocked(html));
    }

    #[test]
    fn empty_or_blocked_detects_empty_spa_shell() {
        let html = r#"<!DOCTYPE html>
<html><head><title>App</title></head><body>
<script src="/_next/static/chunks/main.js"></script>
<script src="/_next/static/chunks/app.js"></script>
<script src="/_next/static/chunks/framework.js"></script>
<script src="/_next/static/chunks/webpack.js"></script>
<script src="/_next/static/chunks/pages/_app.js"></script>
<script src="/_next/static/chunks/pages/index.js"></script>
<script src="/_next/static/chunks/extra1.js"></script>
<script src="/_next/static/chunks/extra2.js"></script>
</body></html>"#;
        assert!(detect_empty_or_blocked(html));
    }

    #[test]
    fn empty_or_blocked_detects_akamai_block_page() {
        let html = r#"<!DOCTYPE html><html><body>
            <h1>Access Denied</h1>
            <p>Reference ID: x-amz-rid=ABC123</p>
            <p>This request was blocked.</p>
        </body></html>"#;
        assert!(detect_empty_or_blocked(html));
    }

    #[test]
    fn empty_or_blocked_returns_false_for_real_page() {
        let html = r#"<!DOCTYPE html>
<html><head><title>Product Page</title></head>
<body>
<header><nav><a href="/">Home</a></nav></header>
<main>
<h1>Awesome Product</h1>
<p>This is a real product page with enough content to look legitimate.
It has multiple paragraphs of useful text describing what the product does,
its features, pricing, and customer reviews. Definitely not a bot block page.</p>
<p>Here is another paragraph with even more information to make the body content
long enough that our heuristic should not flag it as suspicious.</p>
</main>
<footer><p>Copyright 2026</p></footer>
</body></html>"#;
        assert!(!detect_empty_or_blocked(html));
    }
}
