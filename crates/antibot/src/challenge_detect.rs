//! Challenge detection — identifies anti-bot providers from HTML payloads.
//!
//! In Phase 1 we only detect (so the scrape can fail gracefully); bypass /
//! rendering through CDP happens in Phase 2.

/// Returns the name of the challenge provider when one is detected, `None` otherwise.
pub fn detect_challenge(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    if lower.contains("challenges.cloudflare.com")
        || lower.contains("cf-chl-bypass")
        || lower.contains("__cf_chl_jschl_tk__")
    {
        return Some("Cloudflare".to_string());
    }
    if lower.contains("hcaptcha.com") || lower.contains("h-captcha") {
        return Some("hCaptcha".to_string());
    }
    if lower.contains("recaptcha") || lower.contains("grecaptcha") {
        return Some("reCAPTCHA".to_string());
    }
    if lower.contains("perimeterx") || lower.contains("px-captcha") {
        return Some("PerimeterX".to_string());
    }
    if lower.contains("datadome.co") || lower.contains("dd-captcha") {
        return Some("DataDome".to_string());
    }
    None
}

/// Returns `true` when the HTML body looks like a "blocked but undetected" page —
/// a tiny payload, an obvious anti-bot landing page, or a SPA that hasn't
/// rendered its real content yet. Used by the FetchLadder to decide whether
/// CDP / FlareSolverr should take over even when `detect_challenge` returned
/// `None`.
pub fn detect_empty_or_blocked(html: &str) -> bool {
    let trimmed = html.trim();
    let len = trimmed.len();

    // 1. Suspiciously small payloads (< 200 chars after trim). Real pages are
    //    always larger, even tiny single-product pages. Anti-bot blocks like
    //    "Access Denied" or empty SPA shells are usually under 200 chars.
    if len < 200 {
        return true;
    }

    // 2. Hard anti-bot landing pages that don't trigger `detect_challenge`.
    let lower = trimmed.to_ascii_lowercase();
    const BLOCK_PHRASES: &[&str] = &[
        "you have been blocked",
        "access denied",
        "please verify you are a human",
        "checking your browser before accessing",
        "ddc-captcha", // DataDome captcha container
        "geo.captcha-delivery",
        "cf-mitigated", // Cloudflare mitigation wrapper
        "pardon our interruption",
        "request rejected",
        "this request was blocked",
        "bot detection",
        "incapsula",
        "_Incapsula_Resource",
        "akamai",                // Akamai bot manager generic
        "x-amz-rid",             // Amazon request ID hint
        "security verification", // StackOverflow Cloudflare
        "performing security verification",
        "verifying you are human",
        "attention required",
        "ray id", // Cloudflare Ray ID
        "cf-chl-bypass",
    ];
    for phrase in BLOCK_PHRASES {
        if lower.contains(phrase) {
            return true;
        }
    }

    // 3. JSON-only SPA shells with no rendered DOM content. These are <script>
    //    tags that bootstrap a React/Next.js page but the body is still empty.
    //    We measure only the text OUTSIDE of tags (a src= URL inside <script>
    //    doesn't count as visible content).
    let has_doctype = lower.starts_with("<!doctype") || lower.starts_with("<html");
    let body_tag_open = lower.find("<body");
    if has_doctype && body_tag_open.is_some() {
        let after_body_start = body_tag_open.unwrap();
        let body_close = lower[after_body_start..].find("</body>");
        if let Some(close_offset) = body_close {
            let body_inner = &lower[after_body_start..after_body_start + close_offset];
            let tag_count = body_inner.matches('<').count();
            // Strip all tags from the body and measure what's left.
            let stripped: String = body_inner
                .split('<')
                .filter_map(|s| s.find('>').map(|i| &s[i + 1..]))
                .collect();
            let visible_chars: usize = stripped.chars().filter(|c| !c.is_whitespace()).count();
            // If less than 100 visible chars (outside of tags) and many tags,
            // it's an empty SPA shell.
            if visible_chars < 100 && tag_count > 5 {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cloudflare() {
        assert_eq!(
            detect_challenge("<script src='https://challenges.cloudflare.com/...' ></script>"),
            Some("Cloudflare".to_string())
        );
        assert_eq!(
            detect_challenge("var __cf_chl_jschl_tk__ = 'abc';"),
            Some("Cloudflare".to_string())
        );
    }

    #[test]
    fn detects_hcaptcha() {
        assert_eq!(
            detect_challenge("<div class='h-captcha'></div>"),
            Some("hCaptcha".to_string())
        );
        assert_eq!(
            detect_challenge("https://hcaptcha.com/1/api.js"),
            Some("hCaptcha".to_string())
        );
    }

    #[test]
    fn detects_recaptcha() {
        assert_eq!(
            detect_challenge("grecaptcha.render('container')"),
            Some("reCAPTCHA".to_string())
        );
    }

    #[test]
    fn detects_perimeterx() {
        assert_eq!(
            detect_challenge("px-captcha iframe loaded"),
            Some("PerimeterX".to_string())
        );
    }

    #[test]
    fn detects_datadome() {
        assert_eq!(
            detect_challenge("datadome.co challenge page"),
            Some("DataDome".to_string())
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
        assert_eq!(detect_challenge(html), Some("Cloudflare".to_string()));
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
        // SPA shell with 8 script tags but virtually no visible text content.
        // Body visible chars < 50 with many tags > 5 should be flagged.
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
