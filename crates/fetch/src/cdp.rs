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
use crw_antibot::{stealth_script, CookieJar};
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

impl CdpConfig {
    /// Build a `CdpConfig` whose chrome path is taken from `CHROME_PATH`
    /// (if set) or `chrome_path_override` (if `Some`), in that order.
    /// Convenience for callers that want to inject a path explicitly
    /// without re-implementing the env-var lookup.
    pub fn with_chrome_path(chrome_path_override: Option<PathBuf>) -> Self {
        let mut cfg = Self::default();
        if cfg.chrome_path.is_none() {
            cfg.chrome_path = chrome_path_override;
        }
        cfg
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
    /// Timestamp of the most recent failed init attempt, if any. We keep this
    /// so we don't spin on a broken chromium binary (a hot retry loop would
    /// cost ~60 s each), but we DO allow re-attempts: a previous failure does
    /// not permanently poison the slot. See `get_or_init` for the retry policy.
    last_init_failure: Option<std::time::Instant>,
}

/// CDP fetcher. Uses a single browser instance and one fresh page per fetch.
pub struct CdpFetcher {
    config: CdpConfig,
    slot: Arc<Mutex<InnerSlot>>,
    /// Shared cookie jar — see `HttpFetcher::cookies` for the rationale.
    cookies: Arc<CookieJar>,
}

impl CdpFetcher {
    pub fn new(config: CdpConfig) -> Self {
        Self {
            config,
            slot: Arc::new(Mutex::new(InnerSlot {
                inner: None,
                last_init_failure: None,
            })),
            cookies: Arc::new(CookieJar::new()),
        }
    }

    pub fn with_default() -> Self {
        Self::new(CdpConfig::default())
    }

    /// Construct a CDP fetcher that shares a cookie jar with the HTTP fetcher
    /// (or any other fetcher). Cookies persisted by CDP navigations will be
    /// re-sent on subsequent HTTP requests, and vice-versa.
    pub fn with_cookies(config: CdpConfig, cookies: Arc<CookieJar>) -> Self {
        Self {
            config,
            slot: Arc::new(Mutex::new(InnerSlot {
                inner: None,
                last_init_failure: None,
            })),
            cookies,
        }
    }

    /// Access the shared cookie jar.
    pub fn cookies(&self) -> Arc<CookieJar> {
        self.cookies.clone()
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
    ///
    /// **Retry policy (LIGHT.1 fix)**: previously this method used a sticky
    /// `init_attempted: bool` flag — the first `Browser::launch()` failure
    /// permanently poisoned the slot, so every subsequent fetch returned the
    /// generic "browser initialisation previously failed" error even after the
    /// underlying issue (e.g. missing CHROME_PATH, transient container start
    /// race) was fixed. That made the server un-recoverable until a restart.
    ///
    /// New policy:
    ///   1. If the slot already holds a live browser, return it.
    ///   2. Otherwise, attempt `Browser::launch(cfg)` up to **2 times** with
    ///      a **2 s backoff** between attempts. Both failures emit the
    ///      original chromiumoxide error (prefixed with "browser config: ")
    ///      so operators can diagnose the real cause.
    ///   3. If both attempts fail, record the failure timestamp on the slot
    ///      and return the error. We do NOT cache the failure permanently:
    ///      the next call after a short cooldown (`RETRY_COOLDOWN`) will be
    ///      allowed to try again. That way a transient failure (container
    ///      coming up, browser binary missing then installed, ...) self-heals
    ///      without a server restart, but a genuinely broken setup doesn't
    ///      burn a 60 s launch timeout on every request.
    async fn get_or_init<'a>(&'a self, slot: &'a mut InnerSlot) -> Result<&'a mut Browser> {
        if let Some(ref mut inner) = slot.inner {
            return Ok(&mut inner.browser);
        }
        // If the previous attempt failed recently, refuse to retry until the
        // cooldown has elapsed — this avoids a hot loop on a broken binary.
        const RETRY_COOLDOWN: Duration = Duration::from_secs(30);
        if let Some(last) = slot.last_init_failure {
            if last.elapsed() < RETRY_COOLDOWN {
                return Err(CrwError::Fetch(
                    "browser initialisation previously failed (retry cooldown)".to_string(),
                ));
            }
            // Cooldown elapsed — allow another attempt. Clear the timestamp
            // so a *fresh* failure re-arms the cooldown.
            slot.last_init_failure = None;
        }
        let cfg = self.build_browser_config()?;
        const MAX_ATTEMPTS: u32 = 2;
        let mut last_err: Option<CrwError> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match Browser::launch(cfg.clone()).await {
                Ok((browser, mut handler)) => {
                    let handle =
                        tokio::spawn(async move { while let Some(_msg) = handler.next().await {} });
                    slot.inner = Some(Inner {
                        browser,
                        _handler: Arc::new(handle),
                    });
                    return Ok(&mut slot.inner.as_mut().unwrap().browser);
                }
                Err(e) => {
                    // Preserve the original chromiumoxide error message —
                    // operators need to see e.g. "Could not find chrome" or
                    // "Connection refused", not a generic wrapper.
                    let crw_err = CrwError::Fetch(format!("browser config: {e}"));
                    warn!(
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        error = %e,
                        "Browser::launch failed"
                    );
                    last_err = Some(crw_err);
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        }
        // Both attempts failed — record the timestamp so the next caller
        // hits the cooldown rather than retrying immediately.
        slot.last_init_failure = Some(std::time::Instant::now());
        Err(last_err.unwrap_or_else(|| {
            CrwError::Fetch("browser initialisation failed (unknown reason)".to_string())
        }))
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

        // Re-inject cookies that the HTTP fetcher (or a previous CDP fetch)
        // already learned for this host. Setting via `document.cookie` only
        // works for non-HttpOnly cookies, but it is the simplest path that
        // does not require a separate CDP round-trip per cookie.
        if let Some(cookie_header) = self.cookies.cookie_header_for(request.url.as_str()) {
            // Escape any single quotes in the cookie value so the JS string
            // literal is safe to evaluate.
            let escaped = cookie_header.replace('\'', "\\'");
            let script = format!(
                r#"(() => {{
                    const raw = '{escaped}';
                    const pairs = raw.split(';');
                    for (const p of pairs) {{
                        const eq = p.indexOf('=');
                        if (eq <= 0) continue;
                        const name = p.slice(0, eq).trim();
                        const value = p.slice(eq + 1).trim();
                        if (!name) continue;
                        try {{
                            document.cookie = name + '=' + value + '; path=/';
                        }} catch (e) {{}}
                    }}
                }})()"#
            );
            let _ = page
                .evaluate(script)
                .await
                .map_err(|e| warn!(error=?e, "failed to inject cookies via document.cookie"));
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

        // Apply realistic timing + mouse micro-movements on e-commerce sites
        // (and any other site that opts-in via wait_for). This makes the CDP
        // session look more like a real human browsing the page.
        if request.wait_for > 0 || is_ecommerce_host(&request.url) {
            humanise_pre_extract(page).await;
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

        // Persist any cookies the page set during navigation. We read
        // `document.cookie` and feed each name=value pair into the jar. This
        // catches first-party cookies only (HttpOnly cookies remain hidden
        // to JS, which is fine for our use case).
        if let Ok(final_url_parsed) = Url::parse(&final_url) {
            if let Some(host) = final_url_parsed.host_str() {
                if let Ok(value) = page.evaluate("() => document.cookie").await {
                    if let Ok(cookie_str) = value.into_value::<String>() {
                        for pair in cookie_str.split(';') {
                            let pair = pair.trim();
                            if let Some((name, value)) = pair.split_once('=') {
                                self.cookies
                                    .set_cookie(host, name.trim(), value.trim(), None);
                            }
                        }
                    }
                }
            }
        }

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

/// Heuristic: should the CDP fetcher apply the "humanise" pre-extract dance
/// on this URL? The list of hosts is the short list of e-commerce sites we
/// tested against. Other sites get a fast, non-mouse path.
fn is_ecommerce_host(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    const HOSTS: &[&str] = &[
        "amazon.",
        "leboncoin.",
        "fnac.",
        "cdiscount.",
        "darty.",
        "cdiscount.com",
        "shopify",
        "aliexpress.",
        "ebay.",
    ];
    HOSTS.iter().any(|needle| lower.contains(needle))
}

/// Apply a short sequence of small mouse moves and waits to make a CDP
/// session look slightly more like a human on a slow e-commerce site.
///
/// This is *not* a behavioural anti-detect engine on its own — the heavy
/// lifting is done by the JS stealth script installed before the navigation.
/// The mouse moves here just add some non-zero event activity, which is
/// enough to satisfy DataDome and Akamai's "is the page actually being
/// interacted with?" heuristics on simple static endpoints.
async fn humanise_pre_extract(page: &Page) {
    // Cheap, no-budget path: if the operator disabled the dance, skip it
    // entirely (used by tests and benchmarks).
    if !humanise_enabled() {
        return;
    }
    humanise_full_session(page).await;
}

/// Tunable knobs for the pre-extract "humanise" dance. All are read from
/// env vars on every call so test harnesses can change them at runtime.
struct HumaniseConfig {
    delay_min_ms: u64,
    delay_max_ms: u64,
    total_budget_ms: u64,
}

impl HumaniseConfig {
    fn from_env() -> Self {
        Self {
            delay_min_ms: parse_env_u64("HUMANISE_DELAY_MIN_MS", 50),
            delay_max_ms: parse_env_u64("HUMANISE_DELAY_MAX_MS", 200),
            total_budget_ms: parse_env_u64("HUMANISE_TOTAL_BUDGET_MS", 5_000),
        }
    }
}

fn parse_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(default)
}

fn humanise_enabled() -> bool {
    parse_env_u64("HUMANISE_ENABLED", 1) != 0
}

/// 2D cubic Bezier. Used to generate human-looking mouse trajectories
/// between two screen positions. The control points are jittered by the
/// caller so each run produces a slightly different curve.
struct Bezier;

impl Bezier {
    /// Evaluate a 2D cubic Bezier curve at parameter `t` (in [0, 1]).
    ///
    /// `B(t) = (1-t)^3 P0 + 3(1-t)^2 t P1 + 3(1-t) t^2 P2 + t^3 P3`
    pub fn cubic(
        p0: (f32, f32),
        p1: (f32, f32),
        p2: (f32, f32),
        p3: (f32, f32),
        t: f32,
    ) -> (f32, f32) {
        let u = 1.0 - t;
        let b0 = u * u * u;
        let b1 = 3.0 * u * u * t;
        let b2 = 3.0 * u * t * t;
        let b3 = t * t * t;
        (
            b0 * p0.0 + b1 * p1.0 + b2 * p2.0 + b3 * p3.0,
            b0 * p0.1 + b1 * p1.1 + b2 * p2.1 + b3 * p3.1,
        )
    }
}

/// Apply a longer, more realistic sequence of human interactions before
/// extracting content. Used on e-commerce hosts (or any page that opted
/// in via `request.wait_for > 0`).
///
/// The sequence is:
///   1. Wait for `document.readyState === "complete"`.
///   2. 5–10 mouse moves along 2D cubic Bezier curves from the previous
///      cursor position to a series of pseudo-random targets on the page
///      (header, content, sidebar, link, etc.).
///   3. Progressive scroll in 200 px increments every 300–500 ms (jittered)
///      until 3 viewport-heights of scroll OR the end of the page,
///      then scroll back to the top in 1–2 chunks.
///   4. Reading pause: 1–3 s sleep.
///   5. 0–2 link hovers (hover without click).
///   6. `Page.bringToFront`.
///
/// The whole dance is bounded by `HUMANISE_TOTAL_BUDGET_MS` (default
/// 5 s) and aborts early if it would exceed the budget. That keeps
/// the scrape latency predictable even on slow sites.
pub async fn humanise_full_session(page: &Page) {
    let cfg = HumaniseConfig::from_env();
    // Track the start of the dance so we can stop early if we overrun.
    let start = std::time::Instant::now();

    // 1. Wait for `document.readyState === "complete"`. We use a small
    //    fixed number of polls rather than an event listener because
    //    chromiumoxide does not expose Page.lifecycleEvent directly.
    for _ in 0..5 {
        let ready = page
            .evaluate("() => document.readyState")
            .await
            .ok()
            .and_then(|v| v.into_value::<String>().ok())
            .unwrap_or_default();
        if ready == "complete" {
            break;
        }
        if !budget_allows(start, 80, cfg.total_budget_ms) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }

    // Pull the viewport once; we use it both for mouse-target generation
    // and for the "3 viewport-heights of scroll" cap.
    let viewport = page
        .evaluate("() => ({ w: window.innerWidth, h: window.innerHeight })")
        .await
        .ok()
        .and_then(|v| v.into_value::<serde_json::Value>().ok())
        .map(|raw| {
            let w = raw.get("w").and_then(|x| x.as_f64()).unwrap_or(1280.0) as f32;
            let h = raw.get("h").and_then(|x| x.as_f64()).unwrap_or(800.0) as f32;
            (w, h)
        })
        .unwrap_or((1280.0, 800.0));
    let (vw, vh) = viewport;

    // 2. Mouse moves: 5–10 targets, each traversed along a cubic Bezier
    //    with jittered control points. We dispatch a `mousemove` event
    //    at ~5–10 points along each curve.
    let mut current = (vw * 0.1, vh * 0.5);
    let target_count = 5 + (fastrand::u64(0..6)) as usize; // 5..=10
    for i in 0..target_count {
        // Pick a target within the viewport, biased away from the current
        // position so the cursor actually moves.
        let tx = {
            let lo = current.0.max(40.0);
            let hi = vw - 40.0;
            if hi > lo {
                lo + fastrand::f32() * (hi - lo)
            } else {
                current.0
            }
        };
        let ty = {
            let lo = 40.0_f32;
            let hi = vh - 40.0;
            if hi > lo {
                lo + fastrand::f32() * (hi - lo)
            } else {
                current.1
            }
        };
        let target = (tx, ty);

        // Jittered control points: pull them perpendicular to the
        // start→end line, by up to 30% of the line length.
        let dx = target.0 - current.0;
        let dy = target.1 - current.1;
        let dist = (dx * dx + dy * dy).sqrt().max(1.0);
        // Perpendicular unit vector (-dy, dx) / dist
        let px = -dy / dist;
        let py = dx / dist;
        let jitter_a = (fastrand::f32() - 0.5) * 0.6 * dist;
        let jitter_b = (fastrand::f32() - 0.5) * 0.6 * dist;
        let c1 = (
            current.0 + dx * 0.33 + px * jitter_a,
            current.1 + dy * 0.33 + py * jitter_a,
        );
        let c2 = (
            current.0 + dx * 0.66 + px * jitter_b,
            current.1 + dy * 0.66 + py * jitter_b,
        );

        let samples = 5 + (fastrand::u64(0..6)) as usize; // 5..=10
        for s in 0..samples {
            let t = (s as f32) / ((samples - 1) as f32);
            let (mx, my) = Bezier::cubic(current, c1, c2, target, t);
            let script = format!(
                r#"(() => {{
                    try {{
                        const e = new MouseEvent('mousemove', {{
                            bubbles: true,
                            cancelable: true,
                            clientX: {mx},
                            clientY: {my},
                            view: window
                        }});
                        document.dispatchEvent(e);
                        window.dispatchEvent(e);
                    }} catch (err) {{}}
                }})()"#
            );
            let _ = page.evaluate(script).await;
            let delay =
                cfg.delay_min_ms + (fastrand::u64(0..(cfg.delay_max_ms - cfg.delay_min_ms + 1)));
            if !budget_allows(start, delay, cfg.total_budget_ms) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        current = target;

        // Mid-way through, occasionally hover an <a> element to add a
        // touch of "I'm reading the page" behaviour.
        if i == 2 || i == 5 {
            if let Err(e) = page.evaluate(HOVER_ANCHOR_JS).await {
                tracing::debug!(error=?e, "link hover evaluate failed");
            }
            if !budget_allows(start, 100, cfg.total_budget_ms) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // 3. Progressive scroll. We scroll in 200 px increments every
    //    300–500 ms (jittered) until we hit the cap or the page end.
    let max_scroll_px = (vh * 3.0) as i64;
    let mut scrolled: i64 = 0;
    while scrolled < max_scroll_px {
        // Step length jittered around 200 px.
        let step = 180 + (fastrand::u64(0..41)) as i64; // 180..=220
        let script = format!(
            r#"(() => {{
                const before = window.scrollY;
                window.scrollBy({{ top: {step}, behavior: 'auto' }});
                return window.scrollY - before;
            }})()"#
        );
        let actually_scrolled = page
            .evaluate(script)
            .await
            .ok()
            .and_then(|v| v.into_value::<i64>().ok())
            .unwrap_or(0);
        if actually_scrolled <= 0 {
            // End of page.
            break;
        }
        scrolled += actually_scrolled;
        let delay = 300 + (fastrand::u64(0..201)); // 300..=500
        if !budget_allows(start, delay, cfg.total_budget_ms) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    // Scroll back to top in 1–2 chunks.
    let back_step = (scrolled / 2).max(200);
    for _ in 0..2 {
        let _ = page
            .evaluate(format!(
                r#"(() => {{
                    window.scrollBy({{ top: -{back_step}, behavior: 'auto' }});
                }})()"#
            ))
            .await;
        if !budget_allows(start, 150, cfg.total_budget_ms) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // Snap to top to undo the last step overshoot.
    let _ = page
        .evaluate("() => window.scrollTo({top: 0, behavior: 'auto'})")
        .await;

    // 4. Reading pause: 1–3 s.
    let read_ms = 1_000 + (fastrand::u64(0..2_001)); // 1000..=3000
    if !budget_allows(start, read_ms, cfg.total_budget_ms) {
        return;
    }
    tokio::time::sleep(Duration::from_millis(read_ms)).await;

    // 5. Occasional link hover (1–2 times) — we already did 2 inline above;
    //    add 0–1 more here for a little more entropy.
    let extra_hovers = (fastrand::u64(0..2)) as usize; // 0..=1
    for _ in 0..extra_hovers {
        let _ = page.evaluate(HOVER_ANCHOR_JS).await;
        if !budget_allows(start, 100, cfg.total_budget_ms) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 6. Bring the page to the front of the browser tab stack. This is a
    //    no-op for headless mode but a real-browser signal that some
    //    fingerprint scorers still look at.
    let _ = page.bring_to_front().await;
}

/// Returns true if there is still time left in the humanise budget for an
/// action that would take `extra_ms` milliseconds.
fn budget_allows(start: std::time::Instant, extra_ms: u64, total_budget_ms: u64) -> bool {
    start.elapsed().as_millis() as u64 + extra_ms <= total_budget_ms
}

/// JS snippet that finds the first visible `<a>` element on the page and
/// dispatches a `mouseover` + `mousemove` on it (without clicking). The
/// helper bails out silently if no anchor is found, so it's safe to call
/// on any page.
const HOVER_ANCHOR_JS: &str = r#"(() => {
    try {
        const links = Array.from(document.querySelectorAll('a'));
        const visible = links.find(a => {
            const r = a.getBoundingClientRect();
            return r.width > 0 && r.height > 0 && r.top < window.innerHeight && r.bottom > 0;
        });
        if (!visible) return false;
        const r = visible.getBoundingClientRect();
        const x = r.left + r.width / 2;
        const y = r.top + r.height / 2;
        const over = new MouseEvent('mouseover', { bubbles: true, cancelable: true, clientX: x, clientY: y, view: window });
        const move = new MouseEvent('mousemove', { bubbles: true, cancelable: true, clientX: x, clientY: y, view: window });
        visible.dispatchEvent(over);
        visible.dispatchEvent(move);
        return true;
    } catch (err) { return false; }
})()"#;

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
    #[ignore = "requires a Chromium binary on PATH; run with `cargo test -- --ignored`"]
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

    // -----------------------------------------------------------------------
    // Bezier math. The midpoint (t=0.5) of a 2D cubic Bezier with control
    // points P0..P3 evaluates to:
    //     B(0.5) = (1/8) P0 + (3/8) P1 + (3/8) P2 + (1/8) P3
    // (the "De Casteljau midpoint" identity). The test pins that down so
    // that the JS script we generate from these coordinates is correct.
    // -----------------------------------------------------------------------
    const BEZIER_TOLERANCE: f32 = 1e-4;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() <= BEZIER_TOLERANCE
    }

    #[test]
    fn bezier_cubic_at_zero_returns_start_point() {
        let p0 = (0.0, 0.0);
        let p1 = (10.0, 20.0);
        let p2 = (40.0, 80.0);
        let p3 = (100.0, 200.0);
        let (x, y) = Bezier::cubic(p0, p1, p2, p3, 0.0);
        assert!(approx_eq(x, p0.0), "x at t=0 was {x}");
        assert!(approx_eq(y, p0.1), "y at t=0 was {y}");
    }

    #[test]
    fn bezier_cubic_at_one_returns_end_point() {
        let p0 = (0.0, 0.0);
        let p1 = (10.0, 20.0);
        let p2 = (40.0, 80.0);
        let p3 = (100.0, 200.0);
        let (x, y) = Bezier::cubic(p0, p1, p2, p3, 1.0);
        assert!(approx_eq(x, p3.0), "x at t=1 was {x}");
        assert!(approx_eq(y, p3.1), "y at t=1 was {y}");
    }

    #[test]
    fn bezier_cubic_midpoint_matches_de_casteljau_weighted_average() {
        // The test value the brief calls out: t=0.5 must equal the weighted
        // average of the four control points with weights 1/8, 3/8, 3/8, 1/8.
        let p0 = (0.0, 0.0);
        let p1 = (200.0, 400.0);
        let p2 = (500.0, 300.0);
        let p3 = (800.0, 600.0);
        let (mx, my) = Bezier::cubic(p0, p1, p2, p3, 0.5);
        let expected_x = 0.125 * p0.0 + 0.375 * p1.0 + 0.375 * p2.0 + 0.125 * p3.0;
        let expected_y = 0.125 * p0.1 + 0.375 * p1.1 + 0.375 * p2.1 + 0.125 * p3.1;
        assert!(
            approx_eq(mx, expected_x),
            "midpoint x {mx} != expected {expected_x}"
        );
        assert!(
            approx_eq(my, expected_y),
            "midpoint y {my} != expected {expected_y}"
        );
    }

    #[test]
    fn bezier_cubic_is_strictly_between_endpoints() {
        // For any non-degenerate cubic where control points lie within the
        // bounding box, B(t) for t in (0, 1) should stay inside that box.
        // We test with random but reasonable coordinates.
        let p0: (f32, f32) = (50.0, 100.0);
        let p1: (f32, f32) = (120.0, 30.0);
        let p2: (f32, f32) = (300.0, 220.0);
        let p3: (f32, f32) = (400.0, 80.0);
        let lo_x: f32 = p0.0.min(p1.0).min(p2.0).min(p3.0);
        let hi_x: f32 = p0.0.max(p1.0).max(p2.0).max(p3.0);
        let lo_y: f32 = p0.1.min(p1.1).min(p2.1).min(p3.1);
        let hi_y: f32 = p0.1.max(p1.1).max(p2.1).max(p3.1);
        for step in 1..20 {
            let t = step as f32 / 20.0;
            let (x, y) = Bezier::cubic(p0, p1, p2, p3, t);
            assert!(
                (lo_x - BEZIER_TOLERANCE..=hi_x + BEZIER_TOLERANCE).contains(&x),
                "x {x} out of [{lo_x}, {hi_x}] at t={t}"
            );
            assert!(
                (lo_y - BEZIER_TOLERANCE..=hi_y + BEZIER_TOLERANCE).contains(&y),
                "y {y} out of [{lo_y}, {hi_y}] at t={t}"
            );
        }
    }

    #[test]
    fn bezier_cubic_with_zero_control_points_is_a_straight_line() {
        // If both control points coincide with the start and end points
        // respectively, the curve is just the straight segment P0→P3.
        let p0 = (0.0, 0.0);
        let p3 = (100.0, 200.0);
        // Control points at 1/3 and 2/3 along the segment.
        let p1 = (100.0 / 3.0, 200.0 / 3.0);
        let p2 = (2.0 * 100.0 / 3.0, 2.0 * 200.0 / 3.0);
        for step in 0..=10 {
            let t = step as f32 / 10.0;
            let (x, y) = Bezier::cubic(p0, p1, p2, p3, t);
            let expected_x = 100.0 * t;
            let expected_y = 200.0 * t;
            assert!(approx_eq(x, expected_x), "t={t}: x {x} != {expected_x}");
            assert!(approx_eq(y, expected_y), "t={t}: y {y} != {expected_y}");
        }
    }

    #[test]
    fn humanise_config_defaults_are_sane() {
        // Make sure the env-var reader doesn't panic and that the fallback
        // values fall inside the ranges the dance assumes.
        let cfg = HumaniseConfig::from_env();
        assert!(cfg.delay_min_ms <= cfg.delay_max_ms);
        assert!(cfg.total_budget_ms > 0);
        assert!(cfg.delay_min_ms >= 1);
    }

    #[test]
    fn budget_allows_returns_false_when_exhausted() {
        // Pick a budget that any single millisecond will exceed.
        let start = std::time::Instant::now();
        let budget_ms = 0_u64;
        // A 1ms action should be rejected when the budget is 0.
        assert!(!budget_allows(start, 1, budget_ms));
    }
}
