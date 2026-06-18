//! HTTP fetch abstraction.

use async_trait::async_trait;
use crw_antibot::{
    http_stealth::USER_AGENTS, BrowserProfile, DelayPreset, RequestDelay, StealthHeaders,
    UserAgentRotator, BROWSER_PROFILES,
};
use crw_core::{CrwError, Result, ScrapeRequest};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use url::Url;

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

/// Real HTTP fetcher built on `reqwest` with anti-bot stealth headers.
pub struct HttpFetcher {
    client: reqwest::Client,
    ua_rotator: Mutex<UserAgentRotator>,
    delay: RequestDelay,
    stealth_enabled: bool,
}

impl HttpFetcher {
    pub fn new(timeout_ms: u32, stealth_enabled: bool, preset: DelayPreset) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms as u64))
            .user_agent(USER_AGENTS[0])
            .build()
            .map_err(|e| CrwError::Fetch(e.to_string()))?;
        Ok(Self {
            client,
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
        })
    }

    pub fn with_client(
        client: reqwest::Client,
        stealth_enabled: bool,
        preset: DelayPreset,
    ) -> Self {
        Self {
            client,
            ua_rotator: Mutex::new(UserAgentRotator::new()),
            delay: RequestDelay::new(preset),
            stealth_enabled,
        }
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

        let mut req = self.client.get(url.clone());
        for (k, v) in stealth.as_pairs() {
            req = req.header(k, v);
        }
        if request.skip_tls_verification {
            tracing::warn!(
                "skip_tls_verification requested but reqwest client was built with verification on"
            );
        }
        if request.mobile {
            req = req.header(
                "Sec-CH-UA-Mobile",
                if profile.sec_ch_ua_mobile.is_empty() {
                    "?0"
                } else {
                    profile.sec_ch_ua_mobile
                },
            );
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
        let html = response
            .text()
            .await
            .map_err(|e| CrwError::Fetch(e.to_string()))?;

        if !status.is_success() {
            return Err(CrwError::Http {
                status: status.as_u16(),
                message: format!("HTTP {status}"),
            });
        }
        Ok(FetchResult {
            url: request.url.clone(),
            final_url,
            status_code: status.as_u16(),
            html,
            headers: response_headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_core::ScrapeRequest;

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
}
