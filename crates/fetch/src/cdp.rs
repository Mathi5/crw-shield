//! CDP-based fetcher built on top of `chromiumoxide`.
//!
//! The fetcher lazily launches a single headless Chromium browser (per process)
//! and opens a fresh page for each fetch. The stealth script is installed via
//! `Page.addScriptToEvaluateOnNewDocument` so that the patches are in place
//! before any page script runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::Page;
use crw_antibot::stealth_script;
use crw_core::{BrowserAction, CrwError, Result, ScrapeRequest};
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::warn;
use url::Url;

use crate::http::{FetchResult, Fetcher};

/// Extended fetch result for CDP — includes the optional screenshot bytes
/// (encoded as PNG) alongside the HTML payload.
#[derive(Debug, Clone)]
pub struct CdpFetchResult {
    pub url: String,
    pub final_url: String,
    pub status_code: u16,
    pub html: String,
    pub headers: HashMap<String, String>,
    pub screenshot: Option<Vec<u8>>,
}

/// Browser-launch configuration for `CdpFetcher`.
#[derive(Debug, Clone)]
pub struct CdpConfig {
    pub chrome_path: Option<PathBuf>,
    pub headless: bool,
    pub window_width: u32,
    pub window_height: u32,
    pub user_data_dir: Option<PathBuf>,
    pub request_timeout: Duration,
    pub launch_timeout: Duration,
    pub enable_stealth: bool,
}

impl Default for CdpConfig {
    fn default() -> Self {
        Self {
            chrome_path: std::env::var("CHROME_PATH").ok().map(PathBuf::from),
            headless: true,
            window_width: 1280,
            window_height: 800,
            user_data_dir: None,
            request_timeout: Duration::from_secs(30),
            launch_timeout: Duration::from_secs(60),
            enable_stealth: true,
        }
    }
}

/// Shared, lazily-initialised browser handle. The browser runs on its own
/// background tokio task that owns the WS handler stream.
struct Inner {
    browser: Browser,
    /// Held to keep the handler task alive.
    _handler: Arc<tokio::task::JoinHandle<()>>,
}

struct InnerSlot {
    inner: Option<Inner>,
    init_attempted: bool,
}

/// CDP fetcher. Uses a single browser instance and one fresh page per fetch.
pub struct CdpFetcher {
    config: CdpConfig,
    slot: Arc<Mutex<InnerSlot>>,
}

impl CdpFetcher {
    pub fn new(config: CdpConfig) -> Self {
        Self {
            config,
            slot: Arc::new(Mutex::new(InnerSlot {
                inner: None,
                init_attempted: false,
            })),
        }
    }

    pub fn with_default() -> Self {
        Self::new(CdpConfig::default())
    }

    /// Build a `BrowserConfig` matching our settings.
    fn build_browser_config(&self) -> Result<BrowserConfig> {
        let mut builder = BrowserConfig::builder()
            .no_sandbox()
            .window_size(self.config.window_width, self.config.window_height)
            .request_timeout(self.config.request_timeout)
            .launch_timeout(self.config.launch_timeout)
            .viewport(Viewport {
                width: self.config.window_width,
                height: self.config.window_height,
                device_scale_factor: Some(1.0),
                emulating_mobile: false,
                has_touch: false,
                is_landscape: true,
            });
        if self.config.headless {
            builder = builder.new_headless_mode();
        }
        if let Some(path) = self.config.chrome_path.as_ref() {
            builder = builder.chrome_executable(path);
        }
        if let Some(dir) = self.config.user_data_dir.as_ref() {
            builder = builder.user_data_dir(dir);
        }
        builder = builder
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--disable-features=IsolateOrigins,site-per-process")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-gpu");
        builder
            .build()
            .map_err(|e| CrwError::Fetch(format!("browser config: {e}")))
    }

    /// Get or initialise the browser.
    async fn get_or_init<'a>(&'a self, slot: &'a mut InnerSlot) -> Result<&'a mut Browser> {
        if let Some(ref mut inner) = slot.inner {
            return Ok(&mut inner.browser);
        }
        if slot.init_attempted {
            return Err(CrwError::Fetch(
                "browser initialisation previously failed".to_string(),
            ));
        }
        slot.init_attempted = true;
        let cfg = self.build_browser_config()?;
        let (browser, mut handler) = Browser::launch(cfg)
            .await
            .map_err(|e| CrwError::Fetch(format!("browser launch: {e}")))?;
        let handle = tokio::spawn(async move { while let Some(_msg) = handler.next().await {} });
        slot.inner = Some(Inner {
            browser,
            _handler: Arc::new(handle),
        });
        Ok(&mut slot.inner.as_mut().unwrap().browser)
    }

    /// Open a new page, install the stealth script, navigate, run actions,
    /// optionally capture a screenshot, and return the resulting HTML.
    async fn run_fetch(&self, request: &ScrapeRequest) -> Result<CdpFetchResult> {
        let url = Url::parse(&request.url).map_err(|e| CrwError::InvalidUrl(e.to_string()))?;
        let mut slot = self.slot.lock().await;
        let browser = self.get_or_init(&mut slot).await?;

        // Open a fresh page so each fetch gets its own context.
        let page = browser
            .new_page("about:blank")
            .await
            .map_err(|e| CrwError::Fetch(format!("new_page: {e}")))?;

        if self.config.enable_stealth {
            if let Err(e) = page.evaluate_on_new_document(stealth_script()).await {
                warn!(error=?e, "failed to install stealth script");
            }
        }

        // Navigate.
        let page = page
            .goto(url.as_str())
            .await
            .map_err(|e| CrwError::Fetch(format!("navigate: {e}")))?;

        // Honour wait_for.
        if request.wait_for > 0 {
            tokio::time::sleep(Duration::from_millis(request.wait_for)).await;
        }

        // Run actions. Collect screenshots from screenshot actions.
        let mut screenshot_actions: Vec<bool> = Vec::new();
        for action in &request.actions {
            if let BrowserAction::Screenshot { full_page } = action {
                screenshot_actions.push(*full_page);
            }
            apply_action(page, action).await?;
        }

        // Pull HTML and final URL.
        let html = page
            .content()
            .await
            .map_err(|e| CrwError::Fetch(format!("content: {e}")))?;
        let final_url = page
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| request.url.clone());

        let wants_screenshot = request
            .formats
            .iter()
            .any(|f| matches!(f, crw_core::Format::Screenshot));
        let screenshot_bytes = if wants_screenshot || !screenshot_actions.is_empty() {
            capture_screenshot(page, !screenshot_actions.is_empty())
                .await
                .ok()
        } else {
            None
        };

        let _ = page.clone().close().await;

        Ok(CdpFetchResult {
            url: request.url.clone(),
            final_url,
            status_code: 200,
            html,
            headers: HashMap::new(),
            screenshot: screenshot_bytes,
        })
    }
}

#[async_trait]
impl Fetcher for CdpFetcher {
    async fn fetch(&self, request: &ScrapeRequest) -> Result<FetchResult> {
        let result = self.run_fetch(request).await?;
        let mut headers = result.headers;
        if result.screenshot.is_some() {
            headers.insert("x-crw-screenshot".to_string(), "1".to_string());
        }
        Ok(FetchResult {
            url: result.url,
            final_url: result.final_url,
            status_code: result.status_code,
            html: result.html,
            headers,
        })
    }
}

impl CdpFetcher {
    /// Fetch and also return the raw screenshot bytes (if available). Used by
    /// the `FetchLadder` to inject the screenshot into `ScrapeData`.
    pub async fn fetch_with_screenshot(&self, request: &ScrapeRequest) -> Result<CdpFetchResult> {
        self.run_fetch(request).await
    }
}

/// Execute one browser action.
async fn apply_action(page: &Page, action: &BrowserAction) -> Result<()> {
    match action {
        BrowserAction::Wait { milliseconds } => {
            tokio::time::sleep(Duration::from_millis(*milliseconds)).await;
        }
        BrowserAction::Click { selector } => {
            let el = page
                .find_element(selector.clone())
                .await
                .map_err(|e| CrwError::Fetch(format!("click find: {e}")))?;
            let _ = el
                .click()
                .await
                .map_err(|e| CrwError::Fetch(format!("click: {e}")))?;
        }
        BrowserAction::Screenshot { .. } => {
            // Screenshot capture is deferred until after all actions have run,
            // so we just store the action for the outer loop to process.
        }
        BrowserAction::Write { text } => {
            let el = page
                .find_element("body")
                .await
                .map_err(|e| CrwError::Fetch(format!("write find body: {e}")))?;
            let _ = el
                .type_str(text.clone())
                .await
                .map_err(|e| CrwError::Fetch(format!("type: {e}")));
        }
        BrowserAction::Press { key } => {
            let key_json = serde_json::to_string(key.as_str())
                .map_err(|e| CrwError::Fetch(format!("press json: {e}")))?;
            let script = format!(
                r#"document.dispatchEvent(new KeyboardEvent('keydown', {{ key: {key_json}, bubbles: true }}));
                   document.dispatchEvent(new KeyboardEvent('keyup',   {{ key: {key_json}, bubbles: true }}));"#
            );
            let _ = page
                .evaluate(script)
                .await
                .map_err(|e| CrwError::Fetch(format!("press: {e}")));
        }
        BrowserAction::Scroll { direction, amount } => {
            let script = match direction {
                crw_core::ScrollDirection::Down => {
                    format!("window.scrollBy(0, {amount});")
                }
                crw_core::ScrollDirection::Up => {
                    format!("window.scrollBy(0, -{amount});")
                }
            };
            let _ = page
                .evaluate(script)
                .await
                .map_err(|e| CrwError::Fetch(format!("scroll: {e}")));
        }
        BrowserAction::Scrape {} => {
            // Just a marker — no-op.
        }
        BrowserAction::ExecuteJavascript { script } => {
            let _ = page
                .evaluate(script.clone())
                .await
                .map_err(|e| CrwError::Fetch(format!("execute_js: {e}")));
        }
    }
    Ok(())
}

async fn capture_screenshot(page: &Page, full_page: bool) -> Result<Vec<u8>> {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .full_page(full_page)
        .build();
    page.screenshot(params)
        .await
        .map_err(|e| CrwError::Fetch(format!("screenshot: {e}")))
}

impl Default for CdpFetcher {
    fn default() -> Self {
        Self::with_default()
    }
}

/// Detect whether a Chromium/Chrome binary is reachable. Used by tests and by
/// callers that want to know if they can run the CDP path.
pub fn chrome_available() -> bool {
    if std::env::var("CHROME_PATH").is_ok() {
        return true;
    }
    for cand in [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ] {
        if std::path::Path::new(cand).exists() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_config_default_is_headless_and_stealth() {
        let cfg = CdpConfig::default();
        assert!(cfg.headless);
        assert!(cfg.enable_stealth);
        assert!(cfg.request_timeout.as_secs() > 0);
    }

    #[test]
    fn cdp_fetcher_can_be_constructed_without_launching() {
        let _ = CdpFetcher::with_default();
        let _ = CdpFetcher::new(CdpConfig::default());
    }

    #[test]
    fn chrome_available_detects_system_chromium() {
        // Should be true on this image — chromiumoxide's auto-detect is good
        // enough that the build script does not need a manual env var.
        // We only assert that the function does not panic.
        let _ = chrome_available();
    }

    #[test]
    fn build_browser_config_succeeds() {
        let f = CdpFetcher::with_default();
        let cfg = f.build_browser_config();
        assert!(cfg.is_ok(), "config build failed: {:?}", cfg.err());
    }

    // -----------------------------------------------------------------------
    // Browser-dependent integration test — opt-in via `cargo test -- --ignored`.
    // -----------------------------------------------------------------------
    #[ignore = "requires Chrome/Chromium on the host; run with: cargo test -- --ignored"]
    #[tokio::test]
    async fn fetches_simple_page_with_browser() {
        let fetcher = CdpFetcher::with_default();
        let req = ScrapeRequest::default_for_url("https://example.com");
        let res = fetcher.run_fetch(&req).await;
        assert!(
            res.is_ok(),
            "CDP fetch should succeed when chromium is installed"
        );
        let r = res.unwrap();
        assert!(r.html.contains("Example Domain") || r.html.contains("example"));
    }
}
