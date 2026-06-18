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
}
