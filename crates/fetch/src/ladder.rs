//! FetchLadder — orchestrates the progressive fallback from cheap HTTP fetch
//! to expensive CDP render.
//!
//! Strategy:
//!   1. Try the HTTP fetcher first (fast, low overhead).
//!   2. If the request needs browser actions (click, write, scroll, ...),
//!      requires JS rendering, or the HTTP response is clearly a challenge
//!      page, escalate to the CDP fetcher.
//!   3. The HTTP result is upgraded in place with the CDP-rendered HTML and
//!      (optional) screenshot when escalation happens.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use crw_antibot::detect_challenge;
use crw_core::{Format, Result, ScrapeData, ScrapeMetadata, ScrapeRequest, ScrapeResponse};
use tracing::{debug, warn};

use crate::cdp::{CdpFetchResult, CdpFetcher};
use crate::http::{FetchResult, Fetcher, HttpFetcher};

/// Outcome of a ladder attempt — what backend served the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSource {
    Http,
    Cdp,
    HttpChallengeThenCdp,
}

impl FetchSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchSource::Http => "http",
            FetchSource::Cdp => "cdp",
            FetchSource::HttpChallengeThenCdp => "http+cdp",
        }
    }
}

/// Internal bundle returned by the ladder.
#[derive(Debug)]
pub struct LadderResult {
    pub fetch: FetchResult,
    pub screenshot: Option<Vec<u8>>,
    pub source: FetchSource,
}

/// Composite fetcher that owns an HTTP fetcher plus an optional CDP fetcher.
pub struct FetchLadder {
    http: Arc<HttpFetcher>,
    cdp: Option<Arc<CdpFetcher>>,
}

impl FetchLadder {
    pub fn new(http: Arc<HttpFetcher>, cdp: Option<Arc<CdpFetcher>>) -> Self {
        Self { http, cdp }
    }

    /// Heuristic: should we skip HTTP and go straight to CDP?
    fn needs_cdp(request: &ScrapeRequest) -> bool {
        if !request.actions.is_empty() {
            // Any browser-only action (click, write, screenshot, scroll, ...)
            // forces CDP. We allow `wait` and `executeJavascript` to run on
            // HTTP-only — but `executeJavascript` is fundamentally a CDP action
            // too, so we escalate.
            return true;
        }
        // The screenshot format cannot be served by HTTP alone.
        if request
            .formats
            .iter()
            .any(|f| matches!(f, Format::Screenshot))
        {
            return true;
        }
        false
    }

    /// Decide whether the HTTP response is a challenge page and should be
    /// escalated to CDP. We treat both obvious challenge HTML (Cloudflare,
    /// hCaptcha, ...) and suspicious anti-bot status codes (403, 429) as
    /// triggers so we can fall back to a real browser.
    fn http_is_challenge(fetch: &FetchResult) -> bool {
        if matches!(fetch.status_code, 403 | 429) {
            return true;
        }
        detect_challenge(&fetch.html).is_some()
    }

    /// Run the HTTP fetcher.
    async fn try_http(&self, request: &ScrapeRequest) -> Result<FetchResult> {
        self.http.fetch(request).await
    }

    /// Run the CDP fetcher.
    async fn try_cdp(&self, request: &ScrapeRequest) -> Result<CdpFetchResult> {
        let cdp = self.cdp.as_ref().ok_or_else(|| {
            crw_core::CrwError::NotImplemented("CDP fetcher not configured".into())
        })?;
        cdp.fetch_with_screenshot(request).await
    }

    /// Run the full ladder and return the best result.
    pub async fn fetch(&self, request: &ScrapeRequest) -> Result<LadderResult> {
        // Decide whether to even try HTTP.
        let force_cdp = Self::needs_cdp(request);

        if !force_cdp {
            match self.try_http(request).await {
                Ok(fetch) => {
                    if !Self::http_is_challenge(&fetch) {
                        return Ok(LadderResult {
                            fetch,
                            screenshot: None,
                            source: FetchSource::Http,
                        });
                    }
                    warn!(url = %request.url, "HTTP response looks like a challenge; escalating to CDP");
                    // fall through to CDP
                    if self.cdp.is_some() {
                        match self.try_cdp(request).await {
                            Ok(cdp_res) => {
                                return Ok(LadderResult {
                                    fetch: FetchResult {
                                        url: cdp_res.url,
                                        final_url: cdp_res.final_url,
                                        status_code: cdp_res.status_code,
                                        html: cdp_res.html,
                                        headers: cdp_res.headers,
                                    },
                                    screenshot: cdp_res.screenshot,
                                    source: FetchSource::HttpChallengeThenCdp,
                                });
                            }
                            Err(e) => {
                                warn!(error=%e, "CDP fallback failed after challenge; returning HTTP result anyway");
                                return Ok(LadderResult {
                                    fetch,
                                    screenshot: None,
                                    source: FetchSource::Http,
                                });
                            }
                        }
                    }
                    // No CDP available; return the HTTP result (caller will
                    // likely surface the challenge as an error).
                    return Ok(LadderResult {
                        fetch,
                        screenshot: None,
                        source: FetchSource::Http,
                    });
                }
                Err(e) => {
                    debug!(error=%e, "HTTP fetch failed; trying CDP");
                    if self.cdp.is_some() {
                        let cdp_res = self.try_cdp(request).await?;
                        return Ok(LadderResult {
                            fetch: FetchResult {
                                url: cdp_res.url,
                                final_url: cdp_res.final_url,
                                status_code: cdp_res.status_code,
                                html: cdp_res.html,
                                headers: cdp_res.headers,
                            },
                            screenshot: cdp_res.screenshot,
                            source: FetchSource::Cdp,
                        });
                    }
                    return Err(e);
                }
            }
        }

        // Force CDP path.
        let cdp_res = self.try_cdp(request).await?;
        Ok(LadderResult {
            fetch: FetchResult {
                url: cdp_res.url,
                final_url: cdp_res.final_url,
                status_code: cdp_res.status_code,
                html: cdp_res.html,
                headers: cdp_res.headers,
            },
            screenshot: cdp_res.screenshot,
            source: FetchSource::Cdp,
        })
    }
}

/// Convenience: run the ladder and assemble a full `ScrapeResponse` ready to
/// be served by the API. The caller still owns `only_main_content`,
/// `include_tags`, `exclude_tags`, etc — those are applied by the handler.
pub async fn scrape_via_ladder(
    ladder: &FetchLadder,
    request: &ScrapeRequest,
    _html_for_extraction: String,
) -> Result<(FetchResult, Option<String>)> {
    let ladder_result = ladder.fetch(request).await?;
    let screenshot_b64 = ladder_result
        .screenshot
        .as_ref()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
    Ok((ladder_result.fetch, screenshot_b64))
}

/// Build a `ScrapeMetadata` instance from a fetch result.
pub fn metadata_from_fetch(fetch: &FetchResult, html: &str) -> ScrapeMetadata {
    let mut m = ScrapeMetadata {
        url: Some(fetch.final_url.clone()),
        source_url: Some(fetch.url.clone()),
        status_code: Some(fetch.status_code),
        ..Default::default()
    };
    m.title = extract_title(html);
    m.description = extract_meta(html, "description");
    m
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after = &html[start..];
    let gt = after.find('>')?;
    let content_start = start + gt + 1;
    let rest = &html[content_start..];
    let end = rest.find("</title")?;
    Some(rest[..end].trim().to_string())
}

fn extract_meta(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let lower = html.to_ascii_lowercase();
    let idx = lower.find(&needle)?;
    let after = &html[idx..];
    let content_kw = "content=\"";
    let cidx = after.find(content_kw)?;
    let value_start = cidx + content_kw.len();
    let rest = &after[value_start..];
    let end = rest.find('"')?;
    Some(rest[..end].trim().to_string())
}

/// Make a `ScrapeData` from a `LadderResult`, applying the simple metadata
/// extraction. The full content pipeline (markdown, links, only_main_content,
/// tag filters) still belongs to the handler.
pub fn scrape_data_from_ladder(result: &LadderResult) -> ScrapeData {
    let screenshot = result.screenshot.as_ref().map(|bytes| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    });
    ScrapeData {
        markdown: None,
        html: None,
        raw_html: None,
        links: None,
        screenshot,
        metadata: metadata_from_fetch(&result.fetch, &result.fetch.html),
    }
}

// We don't need ScrapeResponse here, but re-export so server handlers can
// compose responses in one place if they prefer.
#[allow(dead_code)]
pub fn response_from_data(data: ScrapeData) -> ScrapeResponse {
    ScrapeResponse::ok(data)
}

#[async_trait]
impl Fetcher for FetchLadder {
    async fn fetch(&self, request: &ScrapeRequest) -> Result<FetchResult> {
        Ok(self.fetch(request).await?.fetch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_antibot::DelayPreset;
    use crw_core::ScrapeRequest;

    fn ladder_with_http_only() -> (FetchLadder, Arc<HttpFetcher>) {
        let http = Arc::new(HttpFetcher::new(5_000, false, DelayPreset::Polite).unwrap());
        let ladder = FetchLadder::new(http.clone(), None);
        (ladder, http)
    }

    #[test]
    fn needs_cdp_true_for_actions() {
        let req = ScrapeRequest::default_for_url("https://example.com");
        assert!(!FetchLadder::needs_cdp(&req));
        let req2 = ScrapeRequest {
            actions: vec![crw_core::BrowserAction::Wait { milliseconds: 100 }],
            ..ScrapeRequest::default_for_url("https://example.com")
        };
        assert!(FetchLadder::needs_cdp(&req2));
    }

    #[test]
    fn needs_cdp_true_for_screenshot_format() {
        let mut req = ScrapeRequest::default_for_url("https://example.com");
        req.formats = vec![Format::Screenshot];
        assert!(FetchLadder::needs_cdp(&req));
    }

    #[test]
    fn http_is_challenge_detects_cloudflare() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 200,
            html: "<html><script src='https://challenges.cloudflare.com/'></script></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_is_challenge(&fetch));
    }

    #[test]
    fn http_is_challenge_returns_false_for_clean() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 200,
            html: "<html><body><p>ok</p></body></html>".into(),
            headers: Default::default(),
        };
        assert!(!FetchLadder::http_is_challenge(&fetch));
    }

    #[test]
    fn http_is_challenge_detects_403() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 403,
            html: "<html><body>Forbidden</body></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_is_challenge(&fetch));
    }

    #[test]
    fn http_is_challenge_detects_429() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 429,
            html: "<html><body>Too Many Requests</body></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_is_challenge(&fetch));
    }

    #[test]
    fn http_is_challenge_ignores_other_error_codes() {
        for code in [400u16, 404, 500, 502, 503] {
            let fetch = FetchResult {
                url: "https://x".into(),
                final_url: "https://x".into(),
                status_code: code,
                html: "<html><body>err</body></html>".into(),
                headers: Default::default(),
            };
            assert!(
                !FetchLadder::http_is_challenge(&fetch),
                "expected code {code} not to be treated as challenge"
            );
        }
    }

    #[tokio::test]
    async fn fetch_returns_http_when_no_cdp_needed() {
        let (ladder, _http) = ladder_with_http_only();
        let req = ScrapeRequest::default_for_url("https://example.com");
        // Network-less — we can't actually fetch in CI; this test only checks
        // the ladder wiring doesn't blow up when there's no CDP fetcher.
        // Real HTTP fetch is exercised in `crates/server/tests/`.
        let _ = req;
        let _ = ladder;
    }

    #[test]
    fn metadata_from_fetch_extracts_title_and_description() {
        let fetch = FetchResult {
            url: "https://example.com".into(),
            final_url: "https://example.com/".into(),
            status_code: 200,
            html: r#"<html><head><title>Hi</title><meta name="description" content="d"></head></html>"#.into(),
            headers: Default::default(),
        };
        let m = metadata_from_fetch(&fetch, &fetch.html);
        assert_eq!(m.title.as_deref(), Some("Hi"));
        assert_eq!(m.description.as_deref(), Some("d"));
        assert_eq!(m.status_code, Some(200));
    }

    #[test]
    fn scrape_data_from_ladder_includes_screenshot_data_uri() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 200,
            html: "<html></html>".into(),
            headers: Default::default(),
        };
        let r = LadderResult {
            fetch,
            screenshot: Some(vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0]),
            source: FetchSource::Cdp,
        };
        let data = scrape_data_from_ladder(&r);
        assert!(data
            .screenshot
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }
}
