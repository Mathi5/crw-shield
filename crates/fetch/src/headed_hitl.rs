//! Headed HITL orchestrator — v0.4.6 web viewer.
//!
//! When the scrape ladder escalates to L3 (HITL) and the operator wants a
//! remote-controllable browser session, this module launches a Chromium
//! browser with `headless=new` (offscreen render target, no X11 display
//! required) and exposes a CDP-over-WebSocket endpoint that streams
//! screenshots and accepts mouse/keyboard input events. The operator
//! opens the viewer URL in any web browser, sees what the Chromium sees,
//! and clicks the Turnstile / login wall as a human would.
//!
//! # Why offscreen / `headless=new`?
//!
//! Traditional `headless` mode (`--headless`) refuses mouse / keyboard
//! input via CDP. The new `headless=new` mode (Chrome 109+) renders to
//! an offscreen target and accepts full CDP input. This is the exact
//! mode used by Browserless.io, Scrapingbee, Apify, etc. for "remote
//! browser as a service" use cases. It works on servers without X11.
//!
//! # Flow
//!
//! 1. Ladder calls [`solve_with_viewer`] with the target URL + a hitl_id.
//! 2. We launch a Chromium in `headless=new` mode (separate from the
//!    normal L1 fetcher so we don't pollute its lifecycle).
//! 3. We navigate to the target URL.
//! 4. The viewer HTML page (served at `/hitl/viewer/{hitl_id}`) opens
//!    a WebSocket to `/hitl/cdp/{hitl_id}` which proxies CDP traffic
//!    1:1 to the Chromium browser.
//! 5. The operator clicks the Turnstile. The Chromium browser receives
//!    the input via CDP, solves the challenge in its own TLS session,
//!    and a `cf_clearance` cookie is set on the browser's cookie jar.
//! 6. We watch the Chromium cookie jar via `Network.getCookies` polled
//!    every 500ms. As soon as a session-like cookie (heuristic:
//!    `cf_clearance`, `__cf_bm`, `session`, etc.) appears, we consider
//!    the challenge solved.
//! 7. We extract the cookies, inject them into the shared
//!    [`CookieJar`], and re-scrape via the SAME Chromium browser
//!    (same TLS session, same IP — Cloudflare accepts).
//! 8. We return the resolved HTML. The viewer displays the resolved
//!    page; the API caller gets the data via the normal response.
//!
//! # Cookie watch heuristic
//!
//! Ported from `cortex-scout/mcp-server/src/features/session_store.rs`
//! (MIT-licensed, `cortex-works/cortex-scout`):
//!
//! ```text
//! cookie_name_is_session_like(name):
//!   n = name.to_ascii_lowercase()
//!   n == "session" || n.contains("session") || n.contains("sess")
//!   || n.contains("auth") || n.contains("token")
//!   || n == "sid" || n.ends_with("_sid")
//!   || n == "jwt" || n.contains("jwt")
//!   || n == "cf_clearance" || n == "__cf_bm"
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::Page;
use crw_antibot::CookieJar;
use futures::StreamExt;
use tracing::{info, warn};

use crw_core::Result;

/// Maximum time to wait for a session-like cookie to appear in the
/// browser's cookie jar. After this, we give up and return whatever
/// HTML we got (which will likely be the challenge page itself).
const SOLVE_TIMEOUT: Duration = Duration::from_secs(600); // 10 min

/// Interval between `Network.getCookies` polls.
const COOKIE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Heuristic ported from cortex-scout: does this cookie name look like
/// an auth/session cookie? If yes, the HITL challenge is likely solved.
pub fn cookie_name_is_session_like(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n == "session"
        || n.contains("session")
        || n.contains("sess")
        || n.contains("auth")
        || n.contains("token")
        || n == "sid"
        || n.ends_with("_sid")
        || n == "jwt"
        || n.contains("jwt")
        // Cloudflare-specific
        || n == "cf_clearance"
        || n == "__cf_bm"
}

/// Active HITL session. One per `hitl_id`. Lives for the duration of
/// the solve (until cookies appear or timeout).
pub struct HeadedHitlSession {
    pub hitl_id: String,
    pub url: String,
    pub browser: Browser,
    pub page: Page,
    /// Held to keep the chromiumoxide handler task alive.
    _handler: tokio::task::JoinHandle<()>,
    /// When the session was created (for timeout calculation).
    pub created_at: Instant,
}

impl HeadedHitlSession {
    /// Launch a fresh headed Chromium browser for HITL solving.
    ///
    /// Uses `headless=new` mode so the server doesn't need a display,
    /// but the operator can still interact via the web viewer (CDP
    /// input events are accepted in this mode since Chrome 109+).
    pub async fn launch(hitl_id: &str, url: &str) -> Result<Self> {
        let config = BrowserConfig::builder()
            .no_sandbox()
            .window_size(1280, 800)
            .viewport(Viewport {
                width: 1280,
                height: 800,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                has_touch: false,
                is_landscape: true,
            })
            // `--headless=new` is critical: accepts CDP input events,
            // works on servers without X11. Old `--headless` does not.
            .arg("--headless=new")
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--disable-features=IsolateOrigins,site-per-process")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .request_timeout(Duration::from_secs(30))
            .launch_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed browser config: {e}")))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed browser launch: {e}")))?;

        let handler_task =
            tokio::spawn(async move { while let Some(_msg) = handler.next().await {} });

        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed new_page: {e}")))?;

        // Inject our standard stealth script before any page JS runs.
        // (Reuse the antibot stealth_script: hides webdriver flag, fakes
        // chrome.runtime, etc.)
        let stealth = crw_antibot::stealth_script();
        use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
        let _ = page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(stealth))
            .await;

        page.goto(url)
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed goto: {e}")))?;

        info!(
            hitl_id = %hitl_id,
            url = %url,
            "headed HITL browser launched, viewer can connect"
        );

        Ok(Self {
            hitl_id: hitl_id.to_string(),
            url: url.to_string(),
            browser,
            page,
            _handler: handler_task,
            created_at: Instant::now(),
        })
    }

    /// Poll the browser's cookie jar for session-like cookies.
    /// Returns `Ok(cookies)` once a session-like cookie appears, or
    /// `Err` on timeout / page close.
    pub async fn wait_for_solve(&self) -> Result<Vec<serde_json::Value>> {
        let deadline = self.created_at + SOLVE_TIMEOUT;
        loop {
            if self.created_at.elapsed() > SOLVE_TIMEOUT {
                return Err(crw_core::CrwError::Fetch(format!(
                    "HITL solve timeout after {}s",
                    SOLVE_TIMEOUT.as_secs()
                )));
            }

            // CDP: Network.getCookies — returns ALL cookies for ALL URLs.
            // (The cookie_jar module is HTTP-path-only; CDP gives us
            // everything the browser has, including Secure + HttpOnly.)
            use chromiumoxide::cdp::browser_protocol::network::GetCookiesParams;
            match self.page.execute(GetCookiesParams::default()).await {
                Ok(resp) => {
                    let cookies = resp.result.cookies;
                    let has_session_like =
                        cookies.iter().any(|c| cookie_name_is_session_like(&c.name));
                    if has_session_like {
                        info!(
                            hitl_id = %self.hitl_id,
                            cookies = cookies.len(),
                            "session-like cookie appeared — HITL solved"
                        );
                        return Ok(cookies
                            .iter()
                            .map(|c| {
                                serde_json::json!({
                                    "name": c.name,
                                    "value": c.value,
                                    "domain": c.domain,
                                    "expires": c.expires,
                                    "path": c.path,
                                    "secure": c.secure,
                                    "httpOnly": c.http_only,
                                })
                            })
                            .collect());
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Network.getCookies failed (continuing poll)");
                }
            }

            tokio::time::sleep(COOKIE_POLL_INTERVAL).await;
            let _ = deadline; // suppress unused warning
        }
    }

    /// After cookies appear, re-navigate within the same browser so the
    /// fetch happens in the SAME TLS session. This is the whole point of
    /// headed HITL: the cookies are valid for this browser, not for a
    /// different fetch path.
    pub async fn refetch_in_same_session(&self) -> Result<String> {
        // Reload the page; the browser now has the auth cookies set
        // during solve, so the re-fetch should pass the challenge.
        self.page
            .reload()
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed reload: {e}")))?;

        // Wait for the page to settle (network idle heuristic).
        self.page
            .evaluate("new Promise(r => setTimeout(r, 1500))")
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("settle wait: {e}")))?;

        // Extract the rendered HTML.
        let html = self
            .page
            .content()
            .await
            .map_err(|e| crw_core::CrwError::Fetch(format!("headed content: {e}")))?;

        Ok(html)
    }

    /// Close the browser cleanly. Called when the viewer disconnects
    /// or after the solve completes (success or timeout). Takes
    /// `&mut self` because `chromiumoxide::Browser::close` needs
    /// `&mut self`. The caller is expected to hold the only
    /// outstanding reference to the session when calling this.
    pub async fn close(&mut self) {
        let _ = self.browser.close().await;
        self._handler.abort();
        info!(hitl_id = %self.hitl_id, "headed HITL browser closed");
    }

    /// Best-effort close that doesn't require `&mut self`. Tries to
    /// extract the only outstanding Arc reference and close; if other
    /// references exist (e.g. the viewer is still connected), aborts
    /// the handler task which terminates the underlying browser
    /// process. Either way the session is shut down.
    pub async fn close_via_arc(self: Arc<Self>) {
        // Try to get the only reference; if so, take ownership and
        // call the mut version. Otherwise just abort the handler.
        match Arc::try_unwrap(self) {
            Ok(mut unique) => {
                unique.close().await;
            }
            Err(shared) => {
                // Other holders exist (e.g. the viewer). Force-kill
                // by aborting the chromiumoxide handler task — the
                // browser process dies when the WS handler is
                // dropped.
                shared._handler.abort();
                info!(
                    hitl_id = %shared.hitl_id,
                    "headed HITL browser force-killed (other Arc holders still alive)"
                );
            }
        }
    }
}

/// Extract the cookies from a Chromium CDP response into our shared
/// CookieJar. Cookies that fail to parse are silently skipped.
pub fn inject_cdp_cookies_into_jar(jar: &CookieJar, cdp_cookies: &[serde_json::Value]) {
    for c in cdp_cookies {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let value = c.get("value").and_then(|v| v.as_str()).unwrap_or("");
        let domain = c
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim_start_matches('.');
        // expires is a Unix timestamp (seconds, f64). Convert to
        // `max_age_secs` relative to now; None for session cookies
        // (expires == -1).
        let max_age_secs = c.get("expires").and_then(|v| v.as_f64()).and_then(|exp| {
            if exp <= 0.0 {
                None
            } else {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as f64)
                    .unwrap_or(0.0);
                Some((exp - now).max(0.0) as u64)
            }
        });

        if name.is_empty() || domain.is_empty() {
            continue;
        }
        jar.set_cookie(domain, name, value, max_age_secs);
    }
}

/// Shared registry of active headed HITL sessions, keyed by hitl_id.
/// The HTTP handler looks up a session here when the viewer connects.
pub type SessionRegistry =
    Arc<tokio::sync::Mutex<std::collections::HashMap<String, Arc<HeadedHitlSession>>>>;

pub fn new_session_registry() -> SessionRegistry {
    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
}
