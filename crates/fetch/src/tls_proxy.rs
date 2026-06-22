//! Lifecycle management for the `tls-impersonate-proxy` sidecar binary.
//!
//! The proxy is a tiny MITM HTTPS proxy that re-issues Chrome's outbound
//! traffic through `bogdanfinn/tls-client` with a byte-perfect browser TLS
//! ClientHello. It is what unlocks Cloudflare IUAM and similar
//! fingerprint-sensitive challenges that neither `wreq`'s BoringSSL
//! (HTTP-only path) nor vanilla `chromiumoxide` BoringSSL (CDP path)
//! can pass alone.
//!
//! See `tls-proxy/README.md` and `references/tls-impersonate-proxy.md`
//! (in the `rust-anti-scraping-bypass` skill) for the full architecture.
//!
//! ## Activation
//!
//! Opt-in via env var `TLS_PROXY_ENABLED=true`. When disabled, the
//! fetcher runs as before — no proxy, no behaviour change, no regression
//! risk on the existing 16/20 (80%) baseline.
//!
//! ## Lifecycle
//!
//! 1. `TlsProxyConfig::from_env()` reads the config (or returns None if
//!    the feature is disabled).
//! 2. `TlsProxy::spawn()` spawns the binary, polls the listen port for
//!    readiness, and returns a handle.
//! 3. The `CdpConfig` reads the proxy's listen address and injects
//!    `--proxy-server`, `--ignore-certificate-errors`, and
//!    `--proxy-bypass-list` into chromiumoxide's `BrowserConfig`.
//! 4. On graceful shutdown (or L2 rotation), `TlsProxy::kill()` SIGKILLs
//!    the child process.
//!
//! ## Why a child process and not a library
//!
//! `bogdanfinn/tls-client` is a Go library. There is no canonical Rust
//! port, and re-implementing the ClientHello forging in Rust is fragile
//! (the byte-level fingerprint margins are tiny). Spawning a 10 MB
//! static binary as a child is the lowest-risk path; the parent's
//! process management is ~150 lines and the binary is built in the
//! Dockerfile in a separate stage so the lean image stays lean.

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crw_core::{CrwError, Result};

/// Default listen address for the proxy sidecar.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7890";

/// Default TLS profile name passed to `bogdanfinn/tls-client`.
/// Other valid values: `chrome_124`, `chrome_131`, `firefox_117`,
/// `firefox_123`, `safari_16_0`, `chrome_120_psk`, etc. See the
/// `bogdanfinn/tls-client/profiles` package.
pub const DEFAULT_PROFILE: &str = "chrome_120";

/// Default persistent CA dir (CA cert + key live here across restarts).
pub const DEFAULT_CA_DIR: &str = "/var/lib/crw-shield/tls-ca";

/// Default bypass list — hosts that should NOT be MITMed. Always include
/// localhost variants so the proxy doesn't try to proxy itself or the
/// crw-shield server.
pub const DEFAULT_BYPASS: &str = "localhost,127.0.0.1,::1";

/// Default per-request timeout passed to the proxy.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// How long we wait for the proxy to open its listen port before
/// bailing out (cold start of bogdanfinn + Go runtime ~ 1-2s).
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How often we poll the listen port while waiting for readiness.
pub const READY_POLL: Duration = Duration::from_millis(100);

/// Default rotation ladder — order matters. We start with the profile
/// most likely to match the user agent Chrome advertises, then walk
/// older versions of Chrome (less data on Cloudflare's side), then jump
/// to Firefox (completely different TLS shape) and Safari (yet another
/// shape). 5 profiles × 1 retry each = up to 10 attempts before HITL.
///
/// The skill (`rust-anti-scraping-bypass/references/reactive-profile-rotation.md`)
/// recommends 3-5 profiles max — beyond that, the rotated profile is
/// so old that its TLS shape is itself a fingerprint signal.
pub const DEFAULT_PROFILES: &[&str] = &[
    "chrome_120",  // baseline — matches the chromium binary in the image
    "chrome_117",  // 3 minor versions older — different TLS extensions
    "chrome_107",  // ~2 years older — Cloudflare has less data here
    "firefox_117", // completely different TLS shape (no X25519Kyber768)
    "safari_16_0", // yet another shape (different ALPN, cipher order)
];

/// Configuration for spawning the tls-impersonate-proxy sidecar.
#[derive(Debug, Clone)]
pub struct TlsProxyConfig {
    /// Absolute path to the `tls-impersonate-proxy` binary.
    pub binary: PathBuf,
    /// Listen address (`host:port`). Default `127.0.0.1:7890`.
    pub listen: String,
    /// Initial TLS profile (the one used at startup). The runtime
    /// rotation ladder may swap this out — see `profiles`.
    pub profile: String,
    /// Full rotation ladder — tried in order on each anti-bot block.
    /// First element should equal `profile` (or the runtime will skip
    /// the initial profile silently). Empty = no rotation (vanilla).
    pub profiles: Vec<String>,
    /// Backoff between rotation attempts. The skill recommends 15s;
    /// shorter (2-5s) is fine for self-hosted but slightly increases
    /// the chance of being rate-limited by the target.
    pub rotation_delay: Duration,
    /// Persistent CA dir. CA is loaded if it exists, generated if not.
    pub ca_dir: PathBuf,
    /// Comma-separated hosts to forward as a raw tunnel (no MITM).
    pub bypass: String,
    /// Per-request timeout. Default 60s.
    pub timeout: Duration,
}

impl TlsProxyConfig {
    /// Build a `TlsProxyConfig` from the environment. Returns `None` if
    /// `TLS_PROXY_ENABLED` is not set to `"true"` or `"1"`.
    ///
    /// Env vars recognised (all optional with sensible defaults):
    /// - `TLS_PROXY_ENABLED`       — `"true"`/`"1"` to enable (required)
    /// - `TLS_PROXY_BINARY`        — path to the binary (default `/usr/local/bin/tls-impersonate-proxy`)
    /// - `TLS_PROXY_LISTEN`        — listen address (default `127.0.0.1:7890`)
    /// - `TLS_PROXY_PROFILE`       — initial TLS profile (default `chrome_120`)
    /// - `TLS_PROXY_PROFILES`      — rotation ladder, comma-separated (default: full `DEFAULT_PROFILES` ladder)
    /// - `TLS_PROXY_ROTATION_DELAY_SECS` — backoff between rotations (default `15`, `0` to disable)
    /// - `TLS_PROXY_CA_DIR`        — CA dir (default `/var/lib/crw-shield/tls-ca`)
    /// - `TLS_PROXY_BYPASS`        — bypass list (default `localhost,127.0.0.1,::1`)
    /// - `TLS_PROXY_TIMEOUT_SECS`  — per-request timeout secs (default `60`)
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("TLS_PROXY_ENABLED")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);
        if !enabled {
            return None;
        }

        let binary = std::env::var("TLS_PROXY_BINARY")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin/tls-impersonate-proxy"));

        let listen = std::env::var("TLS_PROXY_LISTEN")
            .ok()
            .unwrap_or_else(|| DEFAULT_LISTEN.to_string());

        let profile = std::env::var("TLS_PROXY_PROFILE")
            .ok()
            .unwrap_or_else(|| DEFAULT_PROFILE.to_string());

        // If the operator gave us a CSV ladder, use it as-is. Otherwise
        // fall back to the default ladder with the initial profile as
        // the first entry. An empty TLS_PROXY_PROFILES means "no
        // rotation" — only the initial profile is ever tried.
        let profiles = std::env::var("TLS_PROXY_PROFILES")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_PROFILES.iter().map(|s| s.to_string()).collect());

        // Make sure the initial profile is at index 0 of the ladder.
        // If the operator set both TLS_PROXY_PROFILE=firefox_120 and
        // TLS_PROXY_PROFILES=chrome_120,chrome_117, the operator-set
        // profile wins and we prepend it.
        let profiles = if profiles.first().map(String::as_str) != Some(profile.as_str()) {
            let mut p = vec![profile.clone()];
            p.extend(profiles);
            p
        } else {
            profiles
        };

        let rotation_delay = std::env::var("TLS_PROXY_ROTATION_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(15));

        let ca_dir = std::env::var("TLS_PROXY_CA_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CA_DIR));

        let bypass = std::env::var("TLS_PROXY_BYPASS")
            .ok()
            .unwrap_or_else(|| DEFAULT_BYPASS.to_string());

        let timeout = std::env::var("TLS_PROXY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT);

        Some(Self {
            binary,
            listen,
            profile,
            profiles,
            rotation_delay,
            ca_dir,
            bypass,
            timeout,
        })
    }

    /// Extract the host:port string for the `--proxy-server` chromium
    /// arg. Always returns `http://{listen}` because chromiumoxide
    /// expects a proxy URL.
    pub fn proxy_server_url(&self) -> String {
        format!("http://{}", self.listen)
    }

    /// Parse the port number out of the listen address for the readiness
    /// probe. Returns `None` if the address is malformed.
    pub fn port(&self) -> Option<u16> {
        self.listen.rsplit(':').next().and_then(|p| p.parse().ok())
    }
}

/// Handle to a running `tls-impersonate-proxy` child process. Dropping
/// the handle does NOT kill the child — call `kill().await` explicitly
/// (typically in graceful-shutdown paths). The handle uses
/// `kill_on_drop(true)` on the underlying `Command` so a forgotten
/// `kill()` on a panic will still reap the zombie.
pub struct TlsProxy {
    child: Arc<Mutex<Option<Child>>>,
    /// The profile the proxy is CURRENTLY running. Starts at index 0
    /// of `config.profiles`. Updated by `rotate()`.
    current_profile: Arc<Mutex<String>>,
    config: TlsProxyConfig,
}

impl TlsProxy {
    /// Spawn the proxy binary, wait for it to open its listen port,
    /// and return a handle. Returns an error if the binary is missing,
    /// fails to start, or does not become ready within `READY_TIMEOUT`.
    pub async fn spawn(config: TlsProxyConfig) -> Result<Self> {
        // Make sure the CA dir exists. The proxy will load the CA from
        // this dir if present, or generate a new one if not.
        if let Err(e) = std::fs::create_dir_all(&config.ca_dir) {
            warn!(dir = %config.ca_dir.display(), error = %e,
                  "could not create tls-proxy CA dir; proxy will likely fail");
        }

        let port = config.port().ok_or_else(|| {
            CrwError::Fetch(format!(
                "invalid tls-proxy listen address: {}",
                config.listen
            ))
        })?;

        let initial = config
            .profiles
            .first()
            .cloned()
            .unwrap_or_else(|| config.profile.clone());

        info!(
            binary = %config.binary.display(),
            listen = %config.listen,
            profile = %initial,
            ladder = ?config.profiles,
            ca_dir = %config.ca_dir.display(),
            "spawning tls-impersonate-proxy"
        );

        let mut child = Self::spawn_with_profile(&config, &initial).await?;

        // Poll the listen port for readiness. The Go binary binds almost
        // immediately (< 1s), but bogdanfinn/tls-client init can take
        // 2-3s the first time. 10s is a safe ceiling.
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(CrwError::Fetch(format!(
                    "tls-impersonate-proxy did not open {} within {:?}",
                    config.listen, READY_TIMEOUT
                )));
            }
            if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                drop(stream);
                break;
            }
            // Detect premature exit (bad arg, missing CA, etc.).
            if let Ok(Some(status)) = child.try_wait() {
                return Err(CrwError::Fetch(format!(
                    "tls-impersonate-proxy exited prematurely: {status}"
                )));
            }
            tokio::time::sleep(READY_POLL).await;
        }

        info!(
            listen = %config.listen,
            "tls-impersonate-proxy ready"
        );

        Ok(Self {
            child: Arc::new(Mutex::new(Some(child))),
            current_profile: Arc::new(Mutex::new(initial)),
            config,
        })
    }

    /// Internal: spawn the binary with a specific profile name. Used by
    /// both the initial `spawn` and by `rotate()` when we swap profiles.
    async fn spawn_with_profile(config: &TlsProxyConfig, profile: &str) -> Result<Child> {
        Command::new(&config.binary)
            .arg("-listen")
            .arg(&config.listen)
            .arg("-profile")
            .arg(profile)
            .arg("-ca-dir")
            .arg(&config.ca_dir)
            .arg("-bypass")
            .arg(&config.bypass)
            .arg("-timeout")
            .arg(format!("{}s", config.timeout.as_secs()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                CrwError::Fetch(format!(
                    "failed to spawn tls-impersonate-proxy at {}: {e}",
                    config.binary.display()
                ))
            })
    }

    /// The TLS profile the proxy is CURRENTLY serving. Read-only — to
    /// rotate, call `rotate()` which will SIGKILL the child and start a
    /// new one with the next profile in the ladder.
    pub async fn current_profile(&self) -> String {
        self.current_profile.lock().await.clone()
    }

    /// Returns the next profile in the rotation ladder, or `None` if
    /// the ladder is exhausted (caller should fall back to HITL).
    pub async fn next_profile(&self) -> Option<String> {
        let current = self.current_profile.lock().await.clone();
        let next = self
            .config
            .profiles
            .iter()
            .skip_while(|p| p.as_str() != current.as_str())
            .nth(1)
            .cloned();
        next
    }

    /// True if there is at least one untried profile in the ladder.
    pub async fn has_next_profile(&self) -> bool {
        self.next_profile().await.is_some()
    }

    /// Rotate to the next profile in the ladder: kill the current
    /// child, spawn a new one with the next profile, wait for
    /// readiness. Returns `Ok(Some(new_profile))` on success,
    /// `Ok(None)` if the ladder is exhausted (caller falls back to
    /// HITL), or `Err` if the new child fails to start.
    ///
    /// Honors `config.rotation_delay` before the new child spawns
    /// (the skill recommends 15s; the env can override). Set to 0
    /// to skip the delay (useful in tests).
    pub async fn rotate(&self) -> Result<Option<String>> {
        let next = match self.next_profile().await {
            Some(n) => n,
            None => {
                warn!("tls-impersonate-proxy rotation ladder exhausted");
                return Ok(None);
            }
        };

        if !self.config.rotation_delay.is_zero() {
            info!(
                backoff = ?self.config.rotation_delay,
                next = %next,
                "rotating tls-impersonate-proxy profile"
            );
            tokio::time::sleep(self.config.rotation_delay).await;
        }

        // Kill the current child. Take it out of the Option so a second
        // concurrent rotate doesn't double-spawn.
        {
            let mut guard = self.child.lock().await;
            if let Some(mut old) = guard.take() {
                let _ = old.kill().await;
                let _ = old.wait().await;
            }
        }

        // Spawn the new child.
        let mut new_child = Self::spawn_with_profile(&self.config, &next).await?;

        // Wait for readiness (shorter poll than initial — Go binary
        // is hot in cache by now).
        let port = self.config.port().ok_or_else(|| {
            CrwError::Fetch(format!(
                "invalid tls-proxy listen address: {}",
                self.config.listen
            ))
        })?;
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                let _ = new_child.kill().await;
                let _ = new_child.wait().await;
                return Err(CrwError::Fetch(format!(
                    "tls-impersonate-proxy (profile={next}) did not open {} within {:?}",
                    self.config.listen, READY_TIMEOUT
                )));
            }
            if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                drop(stream);
                break;
            }
            if let Ok(Some(status)) = new_child.try_wait() {
                return Err(CrwError::Fetch(format!(
                    "tls-impersonate-proxy (profile={next}) exited prematurely: {status}"
                )));
            }
            tokio::time::sleep(READY_POLL).await;
        }

        // Store the new child + update current profile.
        {
            let mut guard = self.child.lock().await;
            *guard = Some(new_child);
        }
        {
            let mut cp = self.current_profile.lock().await;
            *cp = next.clone();
        }

        info!(
            profile = %next,
            "tls-impersonate-proxy rotated to new profile"
        );

        Ok(Some(next))
    }

    /// SIGKILL the child process. Idempotent: a second call on an
    /// already-killed proxy is a no-op.
    pub async fn kill(&self) {
        let mut guard = self.child.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            info!("tls-impersonate-proxy killed");
        }
    }

    /// Accessor for the config (used by `CdpConfig` to build chromium
    /// args).
    pub fn config(&self) -> &TlsProxyConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests must not run concurrently because they mutate process-wide
    // environment variables. Same pattern as `crw_core::config::tests::ENV_LOCK`.
    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn config_disabled_by_default() {
        let _guard = lock_env();
        // Remove every TLS_PROXY_* var so a previous test can't leak.
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_PROFILE");
            std::env::remove_var("TLS_PROXY_PROFILES");
            std::env::remove_var("TLS_PROXY_ROTATION_DELAY_SECS");
        }
        assert!(TlsProxyConfig::from_env().is_none());
    }

    #[test]
    fn config_enabled_via_env() {
        let _guard = lock_env();
        unsafe { std::env::set_var("TLS_PROXY_ENABLED", "true") };
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        assert_eq!(cfg.listen, DEFAULT_LISTEN);
        assert_eq!(cfg.profile, DEFAULT_PROFILE);
        assert!(cfg.proxy_server_url().starts_with("http://"));
        assert!(cfg.port().is_some());
        unsafe { std::env::remove_var("TLS_PROXY_ENABLED") };
    }

    #[test]
    fn config_enabled_via_one() {
        let _guard = lock_env();
        unsafe { std::env::set_var("TLS_PROXY_ENABLED", "1") };
        assert!(TlsProxyConfig::from_env().is_some());
        unsafe { std::env::remove_var("TLS_PROXY_ENABLED") };
    }

    #[test]
    fn config_enabled_via_yes() {
        let _guard = lock_env();
        unsafe { std::env::set_var("TLS_PROXY_ENABLED", "yes") };
        assert!(TlsProxyConfig::from_env().is_some());
        unsafe { std::env::remove_var("TLS_PROXY_ENABLED") };
    }

    #[test]
    fn proxy_server_url_format() {
        let cfg = TlsProxyConfig {
            binary: PathBuf::from("/bin/x"),
            listen: "127.0.0.1:7890".into(),
            profile: "chrome_120".into(),
            profiles: vec!["chrome_120".into()],
            rotation_delay: Duration::from_secs(15),
            ca_dir: PathBuf::from("/tmp/ca"),
            bypass: DEFAULT_BYPASS.into(),
            timeout: DEFAULT_TIMEOUT,
        };
        assert_eq!(cfg.proxy_server_url(), "http://127.0.0.1:7890");
        assert_eq!(cfg.port(), Some(7890));
    }

    #[test]
    fn port_parsing_handles_ipv6() {
        let cfg = TlsProxyConfig {
            binary: PathBuf::from("/bin/x"),
            listen: "[::1]:7890".into(),
            profile: "chrome_120".into(),
            profiles: vec!["chrome_120".into()],
            rotation_delay: Duration::from_secs(15),
            ca_dir: PathBuf::from("/tmp/ca"),
            bypass: DEFAULT_BYPASS.into(),
            timeout: DEFAULT_TIMEOUT,
        };
        assert_eq!(cfg.port(), Some(7890));
    }

    #[test]
    fn default_ladder_has_five_profiles() {
        assert_eq!(DEFAULT_PROFILES.len(), 5);
        assert_eq!(DEFAULT_PROFILES[0], "chrome_120");
        assert_eq!(DEFAULT_PROFILES[1], "chrome_117");
        assert_eq!(DEFAULT_PROFILES[2], "chrome_107");
        assert_eq!(DEFAULT_PROFILES[3], "firefox_117");
        assert_eq!(DEFAULT_PROFILES[4], "safari_16_0");
    }

    #[test]
    fn custom_ladder_via_env() {
        let _guard = lock_env();
        unsafe {
            std::env::remove_var("TLS_PROXY_PROFILE");
            std::env::set_var("TLS_PROXY_ENABLED", "true");
            std::env::set_var("TLS_PROXY_PROFILES", "firefox_117,firefox_120");
        }
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        // The initial profile (default chrome_120) is prepended because
        // the CSV ladder didn't start with it.
        assert_eq!(
            cfg.profiles,
            vec![
                "chrome_120".to_string(),
                "firefox_117".to_string(),
                "firefox_120".to_string(),
            ]
        );
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_PROFILES");
        }
    }

    #[test]
    fn custom_ladder_prepends_initial_profile() {
        let _guard = lock_env();
        unsafe {
            std::env::set_var("TLS_PROXY_ENABLED", "true");
            std::env::set_var("TLS_PROXY_PROFILE", "firefox_123");
            std::env::set_var("TLS_PROXY_PROFILES", "chrome_120,chrome_117");
        }
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        // Initial profile is firefox_123 — prepended to the CSV ladder.
        assert_eq!(
            cfg.profiles,
            vec![
                "firefox_123".to_string(),
                "chrome_120".to_string(),
                "chrome_117".to_string(),
            ]
        );
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_PROFILE");
            std::env::remove_var("TLS_PROXY_PROFILES");
        }
    }

    #[test]
    fn empty_ladder_disables_rotation() {
        let _guard = lock_env();
        unsafe {
            std::env::set_var("TLS_PROXY_ENABLED", "true");
            std::env::set_var("TLS_PROXY_PROFILES", "");
        }
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        // Empty CSV → fall back to DEFAULT_PROFILES (full ladder).
        assert_eq!(cfg.profiles.len(), DEFAULT_PROFILES.len());
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_PROFILES");
        }
    }

    #[test]
    fn rotation_delay_default_15s() {
        let _guard = lock_env();
        unsafe {
            std::env::set_var("TLS_PROXY_ENABLED", "true");
            std::env::remove_var("TLS_PROXY_ROTATION_DELAY_SECS");
        }
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        assert_eq!(cfg.rotation_delay, Duration::from_secs(15));
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_ROTATION_DELAY_SECS");
        };
    }

    #[test]
    fn rotation_delay_override() {
        let _guard = lock_env();
        unsafe {
            std::env::set_var("TLS_PROXY_ENABLED", "true");
            std::env::set_var("TLS_PROXY_ROTATION_DELAY_SECS", "0");
        }
        let cfg = TlsProxyConfig::from_env().expect("should be enabled");
        assert_eq!(cfg.rotation_delay, Duration::from_secs(0));
        unsafe {
            std::env::remove_var("TLS_PROXY_ENABLED");
            std::env::remove_var("TLS_PROXY_ROTATION_DELAY_SECS");
        }
    }
}
