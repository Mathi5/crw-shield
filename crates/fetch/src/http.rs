//! HTTP fetch abstraction.
//!
//! The default `HttpFetcher` is built on top of `reqwest` (rustls). When the
//! `tls-fingerprint` feature is enabled (which is the default feature set for
//! this crate), the fetcher transparently uses a `wreq`-backed client instead
//! so that the TLS ClientHello, HTTP/2 SETTINGS and header order on the wire
//! match a real browser — see `tls_profile.rs` and `TLS_FINGERPRINT_RESEARCH.md`
//! for the rationale and the matching logic.

use async_trait::async_trait;
#[cfg(not(feature = "tls-fingerprint"))]
use crw_antibot::http_stealth::USER_AGENTS;
use crw_antibot::{
    BrowserProfile, CookieJar, DelayPreset, RequestDelay, StealthHeaders, UserAgentRotator,
    BROWSER_PROFILES,
};
use crw_core::{CrwError, Result, ScrapeRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use url::Url;

#[cfg(feature = "tls-fingerprint")]
use crate::tls_profile::build_wreq_client;

/// Result of a single HTTP fetch.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub url: String,
    pub final_url: String,
    pub status_code: u16,
    pub html: String,
    pub headers: HashMap<String, String>,
}

/// Abstraction so callers (including tests) can supply mock fetchers.
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, request: &ScrapeRequest) -> Result<FetchResult>;
}

/// Underlying HTTP client. When the `tls-fingerprint` feature is on, requests
/// flow through `wreq` with a Chrome/Firefox/Safari emulation. Otherwise the
/// path is the same as before — pure `reqwest` + rustls.
enum HttpClient {
    Reqwest(reqwest::Client),
    #[cfg(feature = "tls-fingerprint")]
    Wreq(wreq::Client),
}

impl HttpClient {
    fn reqwest(c: reqwest::Client) -> Self {
        HttpClient::Reqwest(c)
    }

    #[cfg(feature = "tls-fingerprint")]
    fn wreq(c: wreq::Client) -> Self {
        HttpClient::Wreq(c)
    }
}

/// Real HTTP fetcher built on `reqwest` (or `wreq` when the `tls-fingerprint`
/// feature is enabled) with anti-bot stealth headers.
pub struct HttpFetcher {
    client: HttpClient,
    ua_rotator: Mutex<UserAgentRotator>,
    delay: RequestDelay,
    stealth_enabled: bool,
    cookies: Arc<CookieJar>,
}

impl HttpFetcher {
    pub fn new(timeout_ms: u32, stealth_enabled: bool, preset: DelayPreset) -> Result<Self> {
        Self::with_cookies(
            timeout_ms,
            stealth_enabled,
            preset,
            Arc::new(CookieJar::new()),
        )
    }

    /// Construct an `HttpFetcher` that shares a cookie jar with other
    /// fetchers (typically `CdpFetcher`). Cookies returned by the upstream
    /// site are written into the jar on every response; cookies in the jar
    /// are attached as a `Cookie:` header on every outgoing request.
    pub fn with_cookies(
        timeout_ms: u32,
        stealth_enabled: bool,
        preset: DelayPreset,
        cookies: Arc<CookieJar>,
    ) -> Result<Self> {
        #[cfg(feature = "tls-fingerprint")]
        let client = {
            // Bug-fix v0.4.3: Chrome 137 emulation (was Chrome 131). The
            // Chrome MCP bridge used for HITL solves runs Chrome 149, and
            // Cloudflare's `cf_clearance` is bound to the TLS
            // ClientHello + H2 SETTINGS of the browser that resolved the
            // challenge. Chrome 131 was too far behind — the wreq
            // fingerprint was rejected and HITL solve round-trips failed
            // 100% of the time. Chrome 137 is the newest emulation in
            // wreq-util 2.2.6 and the closest match available without
            // bumping the major version. Operators can override at
            // runtime via `CRW_TLS_EMULATION` (parsed in
            // `tls_profile::pick_emulation_for_profile`).
            let emulation = crate::tls_profile::pick_emulation_for_profile_or_env();
            let client = build_wreq_client(emulation, timeout_ms)?;
            HttpClient::wreq(client)
        };
        #[cfg(not(feature = "tls-fingerprint"))]
        let client = {
            use std::time::Duration;
            let c = reqwest::Client::builder()
                .timeout(Duration::from_millis(u64::from(timeout_ms)))
                .user_agent(USER_AGENTS[0])
                .build()
                .map_err(|e| CrwError::Fetch(e.to_string()))?;
            HttpClient::reqwest(c)
        };
        Ok(Self {
            client,
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
            cookies,
        })
    }

    /// Inject a caller-supplied `reqwest::Client`. Used by tests that wire
    /// in a `mockito` server. When the `tls-fingerprint` feature is on, the
    /// `wreq` code path is exercised by `with_wreq_client` instead.
    pub fn with_client(
        client: reqwest::Client,
        stealth_enabled: bool,
        preset: DelayPreset,
    ) -> Self {
        Self {
            client: HttpClient::reqwest(client),
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
            cookies: Arc::new(CookieJar::new()),
        }
    }

    /// Like `with_client` but uses a caller-supplied cookie jar.
    pub fn with_client_and_cookies(
        client: reqwest::Client,
        stealth_enabled: bool,
        preset: DelayPreset,
        cookies: Arc<CookieJar>,
    ) -> Self {
        Self {
            client: HttpClient::reqwest(client),
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
            cookies,
        }
    }

    /// Construct an `HttpFetcher` that uses a `wreq` client with a specific
    /// browser emulation. Only available with the `tls-fingerprint` feature.
    #[cfg(feature = "tls-fingerprint")]
    pub fn with_wreq_client(
        emulation: wreq_util::Emulation,
        stealth_enabled: bool,
        preset: DelayPreset,
        cookies: Arc<CookieJar>,
    ) -> Result<Self> {
        let client = build_wreq_client(emulation, 30_000)?;
        Ok(Self {
            client: HttpClient::wreq(client),
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
            cookies,
        })
    }

    /// Access the cookie jar (used by tests).
    pub fn cookies(&self) -> Arc<CookieJar> {
        self.cookies.clone()
    }

    fn pick_profile(&self) -> BrowserProfile {
        use crw_antibot::USER_AGENTS;
        // Pick a random profile and a random UA from the global pool. We always
        // borrow from the static tables so we keep the `&'static str` typing.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let profile_idx = (now % BROWSER_PROFILES.len() as u128) as usize;
        let mut p = BROWSER_PROFILES[profile_idx].clone();
        if self.stealth_enabled {
            if let Ok(mut rotator) = self.ua_rotator.lock() {
                let picked = rotator.next();
                // Find the same string in the static USER_AGENTS slice so we keep
                // the lifetime valid.
                if let Some(static_ua) = USER_AGENTS.iter().find(|u| **u == picked).copied() {
                    p.user_agent = static_ua;
                }
            }
        }
        p
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, request: &ScrapeRequest) -> Result<FetchResult> {
        let url = Url::parse(&request.url).map_err(|e| CrwError::InvalidUrl(e.to_string()))?;
        let _ = self.delay.next_delay();
        let profile = self.pick_profile();

        let headers_map: HashMap<String, String> = request
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let stealth = if self.stealth_enabled {
            StealthHeaders::build(&profile, &url, &headers_map)
        } else {
            StealthHeaders::minimal(&profile, &headers_map)
        };

        let cookie_header = self
            .cookies
            .cookie_header_for(request.url.as_str())
            .unwrap_or_default();

        if request.skip_tls_verification {
            tracing::warn!(
                "skip_tls_verification requested but client was built with verification on"
            );
        }

        let mobile_header = if request.mobile {
            Some(if profile.sec_ch_ua_mobile.is_empty() {
                "?0"
            } else {
                profile.sec_ch_ua_mobile
            })
        } else {
            None
        };

        // Dispatch to the underlying client. The two branches share a lot of
        // shape (status / url / headers / body) but the concrete `Response`
        // types are not name-compatible, hence the explicit duplication.
        match &self.client {
            HttpClient::Reqwest(client) => {
                let mut req = client.get(url.clone());
                for (k, v) in stealth.as_pairs() {
                    req = req.header(k, v);
                }
                if !cookie_header.is_empty() {
                    req = req.header("Cookie", cookie_header.as_str());
                }
                if let Some(m) = mobile_header {
                    req = req.header("Sec-CH-UA-Mobile", m);
                }

                let response = req
                    .send()
                    .await
                    .map_err(|e| CrwError::Fetch(e.to_string()))?;
                let status = response.status();
                let final_url = response.url().to_string();
                let mut response_headers: HashMap<String, String> = HashMap::new();
                for (k, v) in response.headers().iter() {
                    response_headers.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
                }
                let host_for_cookies = Url::parse(&final_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| request.url.clone());
                for value in response.headers().get_all("set-cookie").iter() {
                    if let Ok(s) = value.to_str() {
                        self.cookies.set_from_set_cookie(&host_for_cookies, s);
                    }
                }
                let html = response
                    .text()
                    .await
                    .map_err(|e| CrwError::Fetch(e.to_string()))?;

                let status_u16 = status.as_u16();
                if !status.is_success() && !matches!(status_u16, 403 | 429) {
                    return Err(CrwError::Http {
                        status: status_u16,
                        message: format!("HTTP {status}"),
                    });
                }
                Ok(FetchResult {
                    url: request.url.clone(),
                    final_url,
                    status_code: status_u16,
                    html,
                    headers: response_headers,
                })
            }
            #[cfg(feature = "tls-fingerprint")]
            HttpClient::Wreq(client) => {
                let mut req = client.get(url.as_str());
                for (k, v) in stealth.as_pairs() {
                    req = req.header(k, v);
                }
                if !cookie_header.is_empty() {
                    req = req.header("Cookie", cookie_header.as_str());
                }
                if let Some(m) = mobile_header {
                    req = req.header("Sec-CH-UA-Mobile", m);
                }

                let response = req
                    .send()
                    .await
                    .map_err(|e| CrwError::Fetch(format!("wreq send: {e}")))?;
                let status = response.status();
                let final_url = response.url().to_string();
                let mut response_headers: HashMap<String, String> = HashMap::new();
                for (k, v) in response.headers().iter() {
                    response_headers.insert(k.to_string(), v.to_str().unwrap_or("").to_string());
                }
                let host_for_cookies = Url::parse(&final_url)
                    .ok()
                    .and_then(|u| u.host_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| request.url.clone());
                for value in response.headers().get_all("set-cookie").iter() {
                    if let Ok(s) = value.to_str() {
                        self.cookies.set_from_set_cookie(&host_for_cookies, s);
                    }
                }
                let html = response
                    .text()
                    .await
                    .map_err(|e| CrwError::Fetch(format!("wreq body: {e}")))?;

                let status_u16 = status.as_u16();
                if !status.is_success() && !matches!(status_u16, 403 | 429) {
                    return Err(CrwError::Http {
                        status: status_u16,
                        message: format!("HTTP {status}"),
                    });
                }
                Ok(FetchResult {
                    url: request.url.clone(),
                    final_url,
                    status_code: status_u16,
                    html,
                    headers: response_headers,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::ScrapeRequest;
    use std::time::Duration;

    #[test]
    fn http_fetcher_constructs() {
        let _ = HttpFetcher::new(5_000, true, DelayPreset::Polite).unwrap();
        let _ = HttpFetcher::new(5_000, false, DelayPreset::Aggressive).unwrap();
    }

    #[tokio::test]
    async fn fetcher_with_mock_server() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/page")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<html><body><h1>Hi</h1></body></html>")
            .create_async()
            .await;

        let url = format!("{}/page", server.url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fetcher = HttpFetcher::with_client(client, false, DelayPreset::Polite);

        let req = ScrapeRequest::default_for_url(url.clone());
        let result = fetcher.fetch(&req).await.unwrap();
        assert_eq!(result.status_code, 200);
        assert!(result.html.contains("Hi"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn invalid_url_returns_error() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fetcher = HttpFetcher::with_client(client, false, DelayPreset::Polite);
        let req = ScrapeRequest::default_for_url("not-a-url");
        let err = fetcher.fetch(&req).await.unwrap_err();
        match err {
            CrwError::InvalidUrl(_) => {}
            _ => panic!("expected InvalidUrl, got {err:?}"),
        }
    }

    #[tokio::test]
    async fn fetcher_returns_ok_for_403_with_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/blocked")
            .with_status(403)
            .with_header("content-type", "text/html")
            .with_body("<html><body>nope</body></html>")
            .create_async()
            .await;

        let url = format!("{}/blocked", server.url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fetcher = HttpFetcher::with_client(client, false, DelayPreset::Polite);
        let req = ScrapeRequest::default_for_url(url);
        let res = fetcher.fetch(&req).await.unwrap();
        assert_eq!(res.status_code, 403);
        assert!(res.html.contains("nope"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetcher_returns_ok_for_429_with_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/slow")
            .with_status(429)
            .with_header("content-type", "text/html")
            .with_body("<html><body>slow down</body></html>")
            .create_async()
            .await;

        let url = format!("{}/slow", server.url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fetcher = HttpFetcher::with_client(client, false, DelayPreset::Polite);
        let req = ScrapeRequest::default_for_url(url);
        let res = fetcher.fetch(&req).await.unwrap();
        assert_eq!(res.status_code, 429);
        assert!(res.html.contains("slow down"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetcher_errors_on_other_non_success_statuses() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/server-error")
            .with_status(500)
            .with_body("kaboom")
            .create_async()
            .await;

        let url = format!("{}/server-error", server.url());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let fetcher = HttpFetcher::with_client(client, false, DelayPreset::Polite);
        let req = ScrapeRequest::default_for_url(url);
        let err = fetcher.fetch(&req).await.unwrap_err();
        match err {
            CrwError::Http { status, .. } => assert_eq!(status, 500),
            other => panic!("expected Http error, got {other:?}"),
        }
        mock.assert_async().await;
    }
}
