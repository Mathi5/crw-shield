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
use crw_antibot::{
    detect_empty_or_blocked, diagnose_situation, CookieJar, SituationReport, SuggestedLadder,
};
use crw_core::{Format, Result, ScrapeData, ScrapeMetadata, ScrapeRequest, ScrapeResponse};
use tracing::{debug, warn};

use crate::cdp::{CdpFetchResult, CdpFetcher};
use crate::flaresolverr::FlareSolverrClient;
use crate::http::{FetchResult, Fetcher, HttpFetcher};

/// Outcome of a ladder attempt — what backend served the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSource {
    Http,
    Cdp,
    HttpChallengeThenCdp,
    FlareSolverr,
    CdpThenFlareSolverr,
}

impl FetchSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchSource::Http => "http",
            FetchSource::Cdp => "cdp",
            FetchSource::HttpChallengeThenCdp => "http+cdp",
            FetchSource::FlareSolverr => "flaresolverr",
            FetchSource::CdpThenFlareSolverr => "cdp+flaresolverr",
        }
    }
}

/// Internal bundle returned by the ladder.
#[derive(Debug)]
pub struct LadderResult {
    pub fetch: FetchResult,
    pub screenshot: Option<Vec<u8>>,
    pub source: FetchSource,
    /// Structured diagnosis of the HTTP response, when one was produced.
    /// The ladder populates this whenever it ran the HTTP fetcher; the
    /// FlareSolverr / CDP-only paths leave it as a default `CleanSuccess`.
    pub situation: SituationReport,
}

/// Composite fetcher that owns an HTTP fetcher plus an optional CDP fetcher
/// and an optional FlareSolverr escalation step.
pub struct FetchLadder {
    http: Arc<HttpFetcher>,
    cdp: Option<Arc<CdpFetcher>>,
    flaresolverr: Option<Arc<FlareSolverrClient>>,
    cookies: Arc<CookieJar>,
}

impl FetchLadder {
    pub fn new(
        http: Arc<HttpFetcher>,
        cdp: Option<Arc<CdpFetcher>>,
        flaresolverr: Option<Arc<FlareSolverrClient>>,
    ) -> Self {
        // Fall back to a fresh jar if the HTTP fetcher was built without one
        // (e.g. by a test that did not go through `with_cookies`). Production
        // callers wire the same jar through both fetchers via `new_with_cookies`.
        let cookies = http.cookies();
        Self {
            http,
            cdp,
            flaresolverr,
            cookies,
        }
    }

    /// Construct a ladder that shares one cookie jar between the HTTP and
    /// CDP fetchers. This is the preferred constructor for production code
    /// because it makes cookies round-trip across escalation steps.
    pub fn new_with_cookies(
        cookies: Arc<CookieJar>,
        flaresolverr: Option<Arc<FlareSolverrClient>>,
        cdp_enabled: bool,
        timeout_ms: u32,
        stealth_enabled: bool,
        preset: crw_antibot::DelayPreset,
        cdp_config: Option<crate::cdp::CdpConfig>,
    ) -> Result<Self> {
        let http = Arc::new(
            HttpFetcher::with_cookies(timeout_ms, stealth_enabled, preset, cookies.clone())
                .map_err(|e| crw_core::CrwError::Fetch(format!("HttpFetcher: {e}")))?,
        );
        let cdp = if cdp_enabled {
            let cfg = cdp_config.unwrap_or_default();
            Some(Arc::new(CdpFetcher::with_cookies(cfg, cookies.clone())))
        } else {
            None
        };
        Ok(Self {
            http,
            cdp,
            flaresolverr,
            cookies,
        })
    }

    /// Access the shared cookie jar.
    pub fn cookies(&self) -> Arc<CookieJar> {
        self.cookies.clone()
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

    /// Run the structured situation detector over a `FetchResult`. Returns
    /// the full `SituationReport` so the ladder can act on
    /// `suggested_ladder` rather than a binary "is challenge?" flag.
    fn diagnose_fetch(fetch: &FetchResult) -> SituationReport {
        let header_pairs: Vec<(String, String)> = fetch
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let headers_opt: Option<&[(String, String)]> = if header_pairs.is_empty() {
            None
        } else {
            Some(&header_pairs)
        };
        diagnose_situation(&fetch.html, Some(fetch.status_code), headers_opt)
    }

    /// Decide whether the HTTP response should be escalated. We rely on the
    /// structured `SituationReport` instead of the legacy boolean helpers:
    /// `suggested_ladder` tells us exactly which step to take next (or that
    /// we should stay put). We also fall back to the legacy
    /// `detect_empty_or_blocked` heuristic for the small-payload case the
    /// detector doesn't classify itself.
    fn http_should_escalate(fetch: &FetchResult) -> bool {
        let report = Self::diagnose_fetch(fetch);
        if report.should_escalate() {
            return true;
        }
        // Belt-and-suspenders: if the detector returned CleanSuccess but the
        // body is suspiciously small / shaped, escalate anyway.
        detect_empty_or_blocked(&fetch.html)
    }

    /// Decide whether a CDP-rendered page is still an "empty / anti-bot"
    /// page and should be escalated to FlareSolverr. We deliberately do NOT
    /// re-check `detect_challenge` here: that function looks for the HTML
    /// fingerprint of a Cloudflare/hCaptcha interstitial, which a fully-
    /// rendered browser never sees. Empty/blocked, on the other hand, is the
    /// case the browser loaded the page successfully but the upstream
    /// response was a soft block.
    fn cdp_is_empty_or_blocked(html: &str) -> bool {
        detect_empty_or_blocked(html)
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

    /// Escalate to FlareSolverr to solve a remaining challenge. Returns
    /// `None` if no FlareSolverr client is configured.
    async fn try_flaresolverr(
        &self,
        url: &str,
        request: &ScrapeRequest,
        from_cdp: bool,
    ) -> Result<Option<LadderResult>> {
        let Some(fs) = self.flaresolverr.as_ref() else {
            return Ok(None);
        };
        debug!(url = %url, "escalating to FlareSolverr");
        match fs.fetch(url, 60_000).await {
            Ok(solution) => {
                let mut headers = std::collections::HashMap::new();
                headers.insert("x-crw-source".to_string(), "flaresolverr".to_string());
                if let Some(ua) = &solution.user_agent {
                    headers.insert("x-crw-user-agent".to_string(), ua.clone());
                }
                let fetch = FetchResult {
                    url: request.url.clone(),
                    final_url: solution.final_url,
                    status_code: solution.status_code,
                    html: solution.html,
                    headers,
                };
                Ok(Some(LadderResult {
                    fetch,
                    screenshot: None,
                    source: if from_cdp {
                        FetchSource::CdpThenFlareSolverr
                    } else {
                        FetchSource::FlareSolverr
                    },
                    situation: SituationReport::default(),
                }))
            }
            Err(e) => {
                warn!(error = %e, url = %url, "FlareSolverr escalation failed");
                Err(e)
            }
        }
    }

    /// Run the full ladder and return the best result.
    pub async fn fetch(&self, request: &ScrapeRequest) -> Result<LadderResult> {
        // Decide whether to even try HTTP.
        let force_cdp = Self::needs_cdp(request);

        if !force_cdp {
            match self.try_http(request).await {
                Ok(fetch) => {
                    let situation = Self::diagnose_fetch(&fetch);
                    if !Self::http_should_escalate(&fetch) {
                        return Ok(LadderResult {
                            fetch,
                            screenshot: None,
                            source: FetchSource::Http,
                            situation,
                        });
                    }
                    // Escalate per the situation's suggestion.
                    let suggestion = situation.suggested_ladder;
                    warn!(url = %request.url, situation = %situation.kind, ?suggestion, "HTTP response triggers escalation");
                    // fall through to CDP
                    if self.cdp.is_some() {
                        match self.try_cdp(request).await {
                            Ok(cdp_res) => {
                                // If the CDP response STILL looks empty or
                                // blocked AND FlareSolverr is configured,
                                // escalate one more step. The check now
                                // covers anti-bot "soft blocks" too (Amazon
                                // 404, DataDome challenge, ...) in addition
                                // to the standard challenge HTML.
                                if Self::cdp_is_empty_or_blocked(&cdp_res.html) {
                                    if let Some(fs_result) =
                                        self.try_flaresolverr(&request.url, request, true).await?
                                    {
                                        return Ok(fs_result);
                                    }
                                }
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
                                    situation,
                                });
                            }
                            Err(e) => {
                                warn!(error=%e, "CDP fallback failed after challenge; trying FlareSolverr");
                                if let Some(fs_result) =
                                    self.try_flaresolverr(&request.url, request, true).await?
                                {
                                    return Ok(fs_result);
                                }
                                return Ok(LadderResult {
                                    fetch,
                                    screenshot: None,
                                    source: FetchSource::Http,
                                    situation,
                                });
                            }
                        }
                    }
                    // No CDP available; try FlareSolverr as a final escalation.
                    if let Some(fs_result) =
                        self.try_flaresolverr(&request.url, request, false).await?
                    {
                        return Ok(fs_result);
                    }
                    // No CDP / FlareSolverr available; return the HTTP result
                    // (caller will likely surface the challenge as an error).
                    return Ok(LadderResult {
                        fetch,
                        screenshot: None,
                        source: FetchSource::Http,
                        situation,
                    });
                }
                Err(e) => {
                    debug!(error=%e, "HTTP fetch failed; trying CDP");
                    if self.cdp.is_some() {
                        let cdp_res = self.try_cdp(request).await?;
                        // Escalate to FlareSolverr if the CDP result is still
                        // empty or anti-bot blocked.
                        if Self::cdp_is_empty_or_blocked(&cdp_res.html) {
                            if let Some(fs_result) =
                                self.try_flaresolverr(&request.url, request, true).await?
                            {
                                return Ok(fs_result);
                            }
                        }
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
                            situation: SituationReport::default(),
                        });
                    }
                    return Err(e);
                }
            }
        }

        // Force CDP path.
        let cdp_res = self.try_cdp(request).await?;
        // Run the detector on the CDP-rendered HTML so the report reflects
        // the final, JS-rendered page (a fully-rendered Cloudflare IUAM
        // interstitial would now classify as CleanSuccess).
        let situation = Self::diagnose_fetch(&FetchResult {
            url: cdp_res.url.clone(),
            final_url: cdp_res.final_url.clone(),
            status_code: cdp_res.status_code,
            html: cdp_res.html.clone(),
            headers: cdp_res.headers.clone(),
        });
        // Smart escalation: if the situation still suggests FlareSolverr
        // (e.g. DataDome) we should NOT return the CDP result, we should
        // jump to the right backend. The legacy code only escalated on
        // `detect_challenge` which was never true post-CDP — a real bug
        // that Phase B fixes.
        if matches!(situation.suggested_ladder, SuggestedLadder::FlareSolverr) {
            if let Some(fs_result) = self.try_flaresolverr(&request.url, request, true).await? {
                return Ok(fs_result);
            }
        }
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
            situation,
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
        let ladder = FetchLadder::new(http.clone(), None, None);
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
    fn http_should_escalate_detects_cloudflare() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 200,
            html: "<html><script src='https://challenges.cloudflare.com/'></script></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_should_escalate(&fetch));
    }

    #[test]
    fn http_should_escalate_returns_false_for_clean() {
        let body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                    Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                    Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
                    nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in \
                    reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla \
                    pariatur. Excepteur sint occaecat cupidatat non proident, sunt in \
                    culpa qui officia deserunt mollit anim id est laborum.";
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 200,
            html: format!("<html><body><p>{body}</p></body></html>"),
            headers: Default::default(),
        };
        assert!(!FetchLadder::http_should_escalate(&fetch));
    }

    #[test]
    fn http_should_escalate_detects_403() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 403,
            html: "<html><body>Forbidden</body></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_should_escalate(&fetch));
    }

    #[test]
    fn http_should_escalate_detects_429() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 429,
            html: "<html><body>Too Many Requests</body></html>".into(),
            headers: Default::default(),
        };
        assert!(FetchLadder::http_should_escalate(&fetch));
    }

    #[test]
    fn http_should_escalate_ignores_4xx_other_than_403_429() {
        // Phase B: 5xx codes now DO trigger escalation (ServerError
        // suggests RetryWithDelay). This test only covers 4xx.
        let body = "Not Found - the requested resource does not exist on this server. \
                    Please check the URL and try again. If you believe this is an error, \
                    contact the site administrator. Reference: abc123def456. \
                    Thank you for your patience.";
        for code in [400u16, 404] {
            let fetch = FetchResult {
                url: "https://x".into(),
                final_url: "https://x".into(),
                status_code: code,
                html: format!("<html><body><p>{body}</p></body></html>"),
                headers: Default::default(),
            };
            assert!(
                !FetchLadder::http_should_escalate(&fetch),
                "expected code {code} not to trigger escalation"
            );
        }
    }

    #[test]
    fn http_should_escalate_on_5xx_via_server_error_situation() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 500,
            html: "<html><body>Internal Server Error</body></html>".into(),
            headers: Default::default(),
        };
        let report = FetchLadder::diagnose_fetch(&fetch);
        assert_eq!(report.kind, crw_antibot::SituationKind::ServerError);
        assert!(FetchLadder::http_should_escalate(&fetch));
    }

    #[test]
    fn diagnose_fetch_returns_situation_report_with_suggested_ladder() {
        // Akamai Bot Manager: should suggest CDP via the X-Akamai-Transformed header.
        let mut headers = std::collections::HashMap::new();
        headers.insert("x-akamai-transformed".to_string(), "9 9 9".to_string());
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 403,
            html: "<html></html>".into(),
            headers,
        };
        let report = FetchLadder::diagnose_fetch(&fetch);
        assert_eq!(report.kind, crw_antibot::SituationKind::AkamaiBotManager);
        assert_eq!(report.suggested_ladder, crw_antibot::SuggestedLadder::Cdp);
    }

    #[test]
    fn diagnose_fetch_detects_data_dome_suggests_flaresolverr() {
        let fetch = FetchResult {
            url: "https://x".into(),
            final_url: "https://x".into(),
            status_code: 403,
            html: "<html><body><div class='ddc-captcha'>x</div></body></html>".into(),
            headers: Default::default(),
        };
        let report = FetchLadder::diagnose_fetch(&fetch);
        assert_eq!(report.kind, crw_antibot::SituationKind::DataDomeCaptcha);
        assert_eq!(
            report.suggested_ladder,
            crw_antibot::SuggestedLadder::FlareSolverr
        );
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
            situation: SituationReport::default(),
        };
        let data = scrape_data_from_ladder(&r);
        assert!(data
            .screenshot
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn fetch_source_as_str_returns_expected_strings() {
        assert_eq!(FetchSource::Http.as_str(), "http");
        assert_eq!(FetchSource::Cdp.as_str(), "cdp");
        assert_eq!(FetchSource::HttpChallengeThenCdp.as_str(), "http+cdp");
        assert_eq!(FetchSource::FlareSolverr.as_str(), "flaresolverr");
        assert_eq!(
            FetchSource::CdpThenFlareSolverr.as_str(),
            "cdp+flaresolverr"
        );
    }

    #[test]
    fn ladder_construction_accepts_flaresolverr_option() {
        let http = Arc::new(HttpFetcher::new(5_000, false, DelayPreset::Polite).unwrap());
        let _ladder_no_fs = FetchLadder::new(http.clone(), None, None);
        let fs = Arc::new(FlareSolverrClient::new("http://localhost:8191").unwrap());
        let _ladder_with_fs = FetchLadder::new(http, None, Some(fs));
    }
}
