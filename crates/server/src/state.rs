use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crw_antibot::{DelayPreset, HostCounters};
use crw_core::Config;
use crw_fetch::{
    CdpConfig, CdpFetcher, FetchLadder, FlareSolverrAllowlist, FlareSolverrClient, HttpFetcher,
    TlsProxy,
};

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
    /// Per-server rate limiter (min interval + random jitter). Defaults
    /// to 2000 ms + 500 ms (configurable via `RATE_LIMIT_MIN_MS` and
    /// `RATE_LIMIT_JITTER_MS`). Set both to 0 to disable. See
    /// `crate::rate_limit::RateLimiter`.
    pub rate_limiter: Arc<crate::rate_limit::RateLimiter>,
    /// Resolved cookie persistence path: the user-configured path if it
    /// is writable, otherwise the user-local fallback
    /// (`$XDG_DATA_HOME/crw-shield/cookies.json`), otherwise `None` for
    /// in-memory only. Handlers should read this rather than
    /// `config.cookie_persistence_path` so they pick up the fallback.
    pub cookie_persistence_path: Option<std::path::PathBuf>,
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
        // Light.4: opt-in allowlist (env: FLARESOLVERR_HOSTS). Empty when
        // unset, which preserves the "FS disabled by default" behaviour
        // called out in Pitfall 17 (global FS regresses cloudflare.com
        // 8385→502).
        let fs_allowlist = FlareSolverrAllowlist::from_env();
        if !fs_allowlist.is_empty() && flaresolverr.is_some() {
            tracing::info!(
                hosts = fs_allowlist.len(),
                "FlareSolverr opt-in allowlist active"
            );
        }
        let ladder = Arc::new(
            FetchLadder::new(http, cdp, flaresolverr)
                .with_tls_proxy_opt(tls_proxy.clone())
                .with_flaresolverr_allowlist(fs_allowlist),
        );
        // Load any persisted cookies from disk (e.g. cf_clearance resolved by
        // a previous HITL). The HTTP + CDP fetchers all share `ladder.cookies()`
        // — we just re-seed it from the saved file. A load failure is logged
        // and ignored; the jar starts empty and the next scrape will collect
        // whatever the upstream site sends back.
        let cookie_path =
            Self::resolve_cookie_persistence_path(config.cookie_persistence_path.as_deref());
        Self::seed_cookie_jar(&ladder, cookie_path.as_deref());
        // Spawn a background task that snapshots the cookie jar to disk
        // every 60s. Lets cf_clearance / session cookies survive a
        // container restart. Skipped when no persistence path is set
        // (in-memory only mode).
        Self::spawn_cookie_persistence_loop(ladder.cookies(), cookie_path.clone());
        Self {
            config: Arc::new(config),
            ladder,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            host_counters: Arc::new(Mutex::new(HashMap::new())),
            tls_proxy,
            rate_limiter: Arc::new(crate::rate_limit::RateLimiter::from_env()),
            cookie_persistence_path: cookie_path,
        }
    }

    /// Decide where the cookie jar should actually live on disk.
    ///
    /// Order of preference:
    /// 1. `configured_path` (from `COOKIE_PERSISTENCE_PATH` env var or default
    ///    `/var/lib/crw-shield/cookies.json`). If we can create the parent
    ///    dir + write a tiny probe file, use it.
    /// 2. A user-local fallback: `$XDG_DATA_HOME/crw-shield/cookies.json` or
    ///    `~/.local/share/crw-shield/cookies.json`. Covers local dev / smoke
    ///    tests where the process is unprivileged and can't write to
    ///    `/var/lib/...`.
    /// 3. `None` — in-memory only, no persistence. Last resort if even the
    ///    user-local dir is unwritable (very unusual).
    ///
    /// `configured_path = None` short-circuits to `None` immediately, so
    /// `COOKIE_PERSISTENCE_PATH=""` still disables persistence as before.
    fn resolve_cookie_persistence_path(
        configured_path: Option<&str>,
    ) -> Option<std::path::PathBuf> {
        let Some(p) = configured_path else {
            tracing::info!("cookie persistence disabled (no COOKIE_PERSISTENCE_PATH)");
            return None;
        };
        let path = std::path::PathBuf::from(p);
        if Self::probe_writable(&path) {
            // Bug-fix v0.4.3: log INFO (not WARN) when the configured path
            // IS writable so operators can confirm at startup that the
            // server is using the path they asked for. Previously this was
            // silent, which made the WARN-on-fallback look like a
            // surprise. See fix discussion in PITFALLS.md / cookie
            // persistence.
            tracing::info!(
                path = %path.display(),
                "cookie persistence enabled at configured path"
            );
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            "configured cookie persistence path is not writable; falling back to user-local dir"
        );
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .or_else(Self::dirs_home_fallback);
        let Some(home) = home else {
            tracing::warn!("no HOME directory available; cookie persistence disabled");
            return None;
        };
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"));
        let fallback = xdg.join("crw-shield").join("cookies.json");
        if Self::probe_writable(&fallback) {
            tracing::warn!(
                path = %fallback.display(),
                "cookie persistence moved to user-local fallback"
            );
            return Some(fallback);
        }
        tracing::warn!(
            path = %fallback.display(),
            "user-local fallback also not writable; cookie persistence disabled (in-memory only)"
        );
        None
    }

    /// Test whether we can create the parent dir of `path` and write a
    /// small probe file inside it. Returns false on any error so the
    /// caller can fall back without surfacing confusing I/O errors later.
    fn probe_writable(path: &std::path::Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!(path = %parent.display(), error = %e,
                    "probe_writable: create_dir_all failed");
                return false;
            }
        }
        // Write a tiny probe file with a unique name. We don't try to clean it
        // up — it's in the target dir, ephemeral, and harmless.
        let probe = parent.join(format!(".write-probe-{}", std::process::id()));
        match std::fs::write(&probe, b"probe") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(e) => {
                tracing::debug!(path = %probe.display(), error = %e,
                    "probe_writable: write failed");
                false
            }
        }
    }

    /// Last-ditch HOME lookup for environments where $HOME isn't set (e.g. some
    /// container init systems, systemd services without User=). Uses getpwuid_r
    /// via `users` crate if available — but to avoid adding a dep we just
    /// return None and let the caller fall back to in-memory mode.
    fn dirs_home_fallback() -> Option<std::path::PathBuf> {
        None
    }

    /// Re-seed the shared cookie jar from a JSON file (if any). Missing
    /// file = empty jar (first boot), corrupt file = warning + empty jar.
    fn seed_cookie_jar(ladder: &Arc<FetchLadder>, path: Option<&std::path::Path>) {
        let Some(path) = path else {
            tracing::info!("cookie persistence disabled (no COOKIE_PERSISTENCE_PATH)");
            return;
        };
        match crw_antibot::CookieJar::load_from_path(path) {
            Ok(loaded) => {
                let count = loaded.iter().len();
                if count == 0 {
                    tracing::info!(path = %path.display(), "cookie jar loaded (empty)");
                    return;
                }
                let shared = ladder.cookies();
                for (host, name, value, expires_at_unix) in loaded.iter() {
                    // Convert absolute Unix expiry into a relative "seconds
                    // from now" so `set_cookie` can store it. A 0-second
                    // remaining lifetime is treated as "expired immediately"
                    // by `set_cookie` (see cookie_jar.rs), which is what we
                    // want for cookies whose deadline is already in the past.
                    let max_age_secs = expires_at_unix.map(|unix| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        unix.saturating_sub(now)
                    });
                    shared.set_cookie(&host, &name, &value, max_age_secs);
                }
                tracing::info!(
                    path = %path.display(),
                    cookies = count,
                    "cookie jar re-seeded from disk"
                );
            }
            Err(e) if e.to_string().contains("missing") || e.to_string().contains("not found") => {
                tracing::info!(path = %path.display(), "no cookie jar on disk yet (first boot)");
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load cookie jar; starting empty"
                );
            }
        }
    }

    /// Spawn a tokio task that periodically saves the cookie jar to disk
    /// so the next restart can pick it up. Cancelled on server shutdown
    /// (the task holds only `Arc`s and an owned `PathBuf`).
    fn spawn_cookie_persistence_loop(
        jar: Arc<crw_antibot::CookieJar>,
        path: Option<std::path::PathBuf>,
    ) {
        let Some(path) = path else {
            return;
        };
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            // First tick fires immediately; skip it (we just loaded, nothing
            // to save yet).
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(e) = jar.save_to_path(&path) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "failed to persist cookie jar"
                    );
                } else {
                    tracing::debug!(path = %path.display(), "cookie jar persisted");
                }
            }
        });
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
        let fs_allowlist = FlareSolverrAllowlist::from_env();
        let ladder = Arc::new(
            FetchLadder::new(http, cdp, flaresolverr)
                .with_tls_proxy_opt(None)
                .with_flaresolverr_allowlist(fs_allowlist),
        );
        // Sync path: we cannot `await` the tokio background save loop here,
        // but seeding the jar from disk is synchronous and works in tests.
        let cookie_path =
            Self::resolve_cookie_persistence_path(config.cookie_persistence_path.as_deref());
        Self::seed_cookie_jar(&ladder, cookie_path.as_deref());
        Self {
            config: Arc::new(config),
            ladder,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            host_counters: Arc::new(Mutex::new(HashMap::new())),
            tls_proxy: None,
            rate_limiter: Arc::new(crate::rate_limit::RateLimiter::from_env()),
            cookie_persistence_path: cookie_path,
        }
    }
}
