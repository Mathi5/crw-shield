//! FlareSolverr client.
//!
//! FlareSolverr is a proxy server that solves Cloudflare and other anti-bot
//! challenges in a real browser and returns the resulting HTML. When our
//! CDP-based fetcher also gets stuck on a challenge, we delegate to
//! FlareSolverr as a final escalation step.
//!
//! API reference: <https://github.com/FlareSolverr/FlareSolverr>

use std::time::Duration;

use crw_core::{CrwError, Result};
use serde::{Deserialize, Serialize};

/// A cookie returned by FlareSolverr alongside the solved HTML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub expires: Option<i64>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default, rename = "httpOnly")]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default)]
    pub session: Option<bool>,
    #[serde(default, rename = "sameSite")]
    pub same_site: Option<String>,
}

/// The `solution` block returned by FlareSolverr on success.
#[derive(Debug, Clone, Deserialize)]
struct FlareSolverrSolution {
    url: String,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    cookies: Vec<CookieInfo>,
    #[serde(default, rename = "userAgent")]
    user_agent: Option<String>,
}

/// The full response body returned by FlareSolverr.
#[derive(Debug, Clone, Deserialize)]
struct FlareSolverrResponse {
    status: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    solution: Option<FlareSolverrSolution>,
}

/// Request body for `request.get`.
#[derive(Debug, Serialize)]
struct FlareSolverrRequest<'a> {
    cmd: &'a str,
    url: &'a str,
    #[serde(rename = "maxTimeout")]
    max_timeout: u64,
}

/// The successful outcome of a FlareSolverr fetch.
#[derive(Debug, Clone)]
pub struct FlareSolverrResult {
    pub html: String,
    pub status_code: u16,
    pub cookies: Vec<CookieInfo>,
    pub final_url: String,
    pub user_agent: Option<String>,
}

/// FlareSolverr HTTP client. Cheap to construct; share it via `Arc`.
pub struct FlareSolverrClient {
    base_url: String,
    client: reqwest::Client,
}

impl FlareSolverrClient {
    /// Build a client that talks to FlareSolverr at the given base URL
    /// (e.g. `http://flaresolverr:8191`). A 65-second timeout is applied to
    /// the underlying HTTP client — FlareSolverr itself may take up to
    /// 60 seconds to solve a challenge.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(65))
            .build()
            .map_err(|e| CrwError::Fetch(format!("flaresolverr client: {e}")))?;
        Ok(Self {
            base_url: base_url.into(),
            client,
        })
    }

    /// Build a client using an externally-provided `reqwest::Client`. Useful
    /// for tests that want to point the client at a `mockito` server.
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            base_url: base_url.into(),
            client,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// POST a `request.get` command to FlareSolverr and parse the response.
    /// `max_timeout_ms` is the maximum time FlareSolverr is allowed to spend
    /// solving the challenge before giving up.
    pub async fn fetch(&self, url: &str, max_timeout_ms: u64) -> Result<FlareSolverrResult> {
        let endpoint = format!("{}/v1", self.base_url.trim_end_matches('/'));
        let body = FlareSolverrRequest {
            cmd: "request.get",
            url,
            max_timeout: max_timeout_ms,
        };

        let response = self
            .client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| CrwError::Fetch(format!("flaresolverr request: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| CrwError::Fetch(format!("flaresolverr body: {e}")))?;

        if !status.is_success() {
            return Err(CrwError::Fetch(format!(
                "flaresolverr returned HTTP {status}: {text}"
            )));
        }

        let parsed: FlareSolverrResponse = serde_json::from_str(&text).map_err(|e| {
            CrwError::Fetch(format!(
                "flaresolverr JSON decode: {e} (body: {} bytes)",
                text.len()
            ))
        })?;

        if parsed.status != "ok" {
            let msg = parsed.message.unwrap_or_else(|| "<no message>".into());
            return Err(CrwError::Fetch(format!("flaresolverr error: {msg}")));
        }

        let solution = parsed
            .solution
            .ok_or_else(|| CrwError::Fetch("flaresolverr response missing solution".to_string()))?;

        let html = solution.response.unwrap_or_default();
        let final_url = solution.url;
        let status_code = solution.status.unwrap_or(200);

        Ok(FlareSolverrResult {
            html,
            status_code,
            cookies: solution.cookies,
            final_url,
            user_agent: solution.user_agent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use serde_json::json;

    #[test]
    fn flaresolverr_client_can_be_constructed() {
        let client = FlareSolverrClient::new("http://localhost:8191").unwrap();
        assert_eq!(client.base_url(), "http://localhost:8191");
    }

    #[test]
    fn flaresolverr_client_trims_trailing_slash() {
        let client = FlareSolverrClient::new("http://localhost:8191/").unwrap();
        assert_eq!(client.base_url(), "http://localhost:8191/");
    }

    #[tokio::test]
    async fn fetch_returns_error_when_server_unreachable() {
        // 127.0.0.1:1 is not going to be reachable in a test environment.
        let client = FlareSolverrClient::new("http://127.0.0.1:1").unwrap();
        let res = client.fetch("https://example.com", 5_000).await;
        assert!(res.is_err(), "expected error when server unreachable");
    }

    #[tokio::test]
    async fn fetch_returns_error_when_server_unreachable_via_invalid_host() {
        // A bad host (nonexistent.test.) will fail to resolve.
        let client = FlareSolverrClient::new("http://nonexistent.invalid").unwrap();
        let res = client.fetch("https://example.com", 5_000).await;
        assert!(res.is_err(), "expected error for invalid host");
    }

    #[tokio::test]
    async fn fetch_returns_error_when_status_is_error() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/v1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "cmd": "request.get",
                    "status": "error",
                    "message": "Could not solve challenge",
                    "version": "3"
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = FlareSolverrClient::new(server.url()).unwrap();
        let res = client.fetch("https://example.com", 5_000).await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("Could not solve challenge"),
            "expected error message, got: {err}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetch_parses_successful_response() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/v1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "cmd": "request.get",
                    "status": "ok",
                    "message": "Challenge solved!",
                    "solution": {
                        "url": "https://example.com/final",
                        "status": 200,
                        "response": "<html><body>solved</body></html>",
                        "cookies": [
                            {
                                "name": "cf_clearance",
                                "value": "abc123",
                                "domain": ".example.com"
                            }
                        ],
                        "userAgent": "Mozilla/5.0"
                    },
                    "startTimestamp": 0,
                    "endTimestamp": 1000,
                    "version": "3"
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = FlareSolverrClient::new(server.url()).unwrap();
        let res = client.fetch("https://example.com", 10_000).await.unwrap();
        assert_eq!(res.status_code, 200);
        assert_eq!(res.final_url, "https://example.com/final");
        assert!(res.html.contains("solved"));
        assert_eq!(res.cookies.len(), 1);
        assert_eq!(res.cookies[0].name, "cf_clearance");
        assert_eq!(res.cookies[0].value, "abc123");
        assert_eq!(res.user_agent.as_deref(), Some("Mozilla/5.0"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetch_returns_error_on_non_2xx_http_status() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/v1")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let client = FlareSolverrClient::new(server.url()).unwrap();
        let res = client.fetch("https://example.com", 5_000).await;
        assert!(res.is_err());
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("500") || err.contains("internal"),
            "expected http error info, got: {err}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn fetch_returns_error_on_malformed_json() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/v1")
            .with_status(200)
            .with_body("not json at all")
            .create_async()
            .await;

        let client = FlareSolverrClient::new(server.url()).unwrap();
        let res = client.fetch("https://example.com", 5_000).await;
        assert!(res.is_err());
        mock.assert_async().await;
    }
}
