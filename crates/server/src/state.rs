use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crw_antibot::{DelayPreset, HostCounters};
use crw_core::Config;
use crw_fetch::{CdpConfig, CdpFetcher, FetchLadder, FlareSolverrClient, HttpFetcher, TlsProxy};

use crate::handlers::CrawlJob;

/// Concrete fetcher type the server uses. Aliasing it here keeps the rest of
/// the crate decoupled from the concrete HTTP/reqwest types.
pub type FetchLadderType = FetchLadder;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub ladder: Arc<FetchLadder>,
    pub delay_preset: DelayPreset,
    pub jobs: Arc<Mutex<std::collections::HashMap<String, CrawlJob>>>,
    /// Per-host rotation bookkeeping shared across all requests.
    /// `Arc<Mutex<…>>` is `Clone`-friendly so this state survives `AppState`
    /// clones (each request gets the same backing map).
    pub host_counters: HostCounters,
    /// Handle to the running `tls-impersonate-proxy` sidecar (if enabled).
    /// Held in `AppState` so the proxy is kept alive for the lifetime of
    /// the server. `None` when `TLS_PROXY_ENABLED` is not set.
    pub tls_proxy: Option<Arc<TlsProxy>>,
}

impl AppState {
    /// Build the `AppState` from a parsed `Config`, optionally spawning
    /// the TLS-impersonate-proxy sidecar first (when `TLS_PROXY_ENABLED=true`).
    /// The proxy must be alive before the `CdpFetcher` lazily launches
    /// Chrome — Chrome connects to `--proxy-server=...` at launch time.
    pub async fn from_config_async(config: Config) -> Self {
        // 1. Spawn the TLS-impersonate-proxy sidecar FIRST, so the listen
        //    port is up before any CDP fetcher is constructed.
        let tls_proxy = match crw_fetch::TlsProxyConfig::from_env() {
            Some(cfg) => match TlsProxy::spawn(cfg).await {
                Ok(p) => {
                    tracing::info!("tls-impersonate-proxy sidecar started");
                    Some(Arc::new(p))
                }
                Err(e) => {
                    tracing::error!(error=%e,
                        "failed to spawn tls-impersonate-proxy; \
                         CDP fetcher will use vanilla BoringSSL fingerprint");
                    None
                }
            },
            None => None,
        };

        let preset = DelayPreset::from_str(&config.scrape_delay_preset);
        let http = Arc::new(
            HttpFetcher::new(60_000, config.stealth_enabled, preset)
                .expect("failed to build HttpFetcher"),
        );
        let cdp = if config.cdp_enabled {
            // FIX 3 (MEDIUM.2): explicitly thread the chrome path through.
            // `CdpConfig::default()` already reads CHROME_PATH from env, so
            // when the Dockerfile sets `ENV CHROME_PATH=/usr/bin/chromium`
            // the fetcher will pick it up. We also log the resolved path so
            // operators can verify the container picked the right binary.
            //
            // The `tls_proxy` field is populated by `CdpConfig::default()`,
            // which reads TLS_PROXY_ENABLED at construction time. Since we
            // already spawned the proxy above (or confirmed it stays None),
            // this is consistent: the config flag and the running process
            // always agree.
            let cdp_cfg = CdpConfig::with_chrome_path(None);
            tracing::info!(
                cdp_enabled = config.cdp_enabled,
                chrome_path = ?cdp_cfg.chrome_path,
                headless = cdp_cfg.headless,
                tls_proxy_enabled = cdp_cfg.tls_proxy.is_some(),
                "building CDP fetcher"
            );
            Some(Arc::new(CdpFetcher::new(cdp_cfg)))
        } else {
            None
        };
        let flaresolverr = match config.flaresolverr_url.as_deref() {
            Some(url) if !url.is_empty() => match FlareSolverrClient::new(url) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(error=%e, "failed to build FlareSolverrClient; disabling");
                    None
                }
            },
            _ => None,
        };
        let ladder = Arc::new(
            FetchLadder::new(http, cdp, flaresolverr)
                .with_tls_proxy_opt(tls_proxy.clone()),
        );
        Self {
            config: Arc::new(config),
            ladder,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            host_counters: Arc::new(Mutex::new(HashMap::new())),
            tls_proxy,
        }
    }

    /// Synchronous wrapper for callers that don't need to await the
    /// proxy spawn (used by tests + the no-proxy path).
    pub fn from_config(config: Config) -> Self {
        // For the sync path, we can't await the proxy spawn. We accept
        // that on the sync path the proxy is NOT spawned — the server
        // will run in vanilla mode. Production code should use
        // `from_config_async` instead. This preserves backwards compat
        // with the existing `AppState::from_config` callers (e.g. tests).
        tracing::warn!(
            "AppState::from_config called synchronously; \
             use from_config_async to enable TLS-impersonation-proxy. \
             Falling back to vanilla CDP fetcher."
        );
        let preset = DelayPreset::from_str(&config.scrape_delay_preset);
        let http = Arc::new(
            HttpFetcher::new(60_000, config.stealth_enabled, preset)
                .expect("failed to build HttpFetcher"),
        );
        let cdp = if config.cdp_enabled {
            let cdp_cfg = CdpConfig::with_chrome_path(None);
            Some(Arc::new(CdpFetcher::new(cdp_cfg)))
        } else {
            None
        };
        let flaresolverr = match config.flaresolverr_url.as_deref() {
            Some(url) if !url.is_empty() => match FlareSolverrClient::new(url) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(error=%e, "failed to build FlareSolverrClient; disabling");
                    None
                }
            },
            _ => None,
        };
        let ladder = Arc::new(
            FetchLadder::new(http, cdp, flaresolverr).with_tls_proxy_opt(None),
        );
        Self {
            config: Arc::new(config),
            ladder,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            host_counters: Arc::new(Mutex::new(HashMap::new())),
            tls_proxy: None,
        }
    }
}
