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

/// Configuration for spawning the tls-impersonate-proxy sidecar.
#[derive(Debug, Clone)]
pub struct TlsProxyConfig {
    /// Absolute path to the `tls-impersonate-proxy` binary.
    pub binary: PathBuf,
    /// Listen address (`host:port`). Default `127.0.0.1:7890`.
    pub listen: String,
    /// TLS profile name. Default `chrome_120`.
    pub profile: String,
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
    /// - `TLS_PROXY_PROFILE`       — TLS profile (default `chrome_120`)
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
        self.listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
    }
}

/// Handle to a running `tls-impersonate-proxy` child process. Dropping
/// the handle does NOT kill the child — call `kill().await` explicitly
/// (typically in graceful-shutdown paths). The handle uses
/// `kill_on_drop(true)` on the underlying `Command` so a forgotten
/// `kill()` on a panic will still reap the zombie.
pub struct TlsProxy {
    child: Arc<Mutex<Option<Child>>>,
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

        info!(
            binary = %config.binary.display(),
            listen = %config.listen,
            profile = %config.profile,
            ca_dir = %config.ca_dir.display(),
            "spawning tls-impersonate-proxy"
        );

        let mut child = Command::new(&config.binary)
            .arg("-listen")
            .arg(&config.listen)
            .arg("-profile")
            .arg(&config.profile)
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
            })?;

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
            config,
        })
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

    #[test]
    fn config_disabled_by_default() {
        // SAFETY: only the parent process reads this in tests, and tests
        // run sequentially within a single test binary.
        unsafe { std::env::remove_var("TLS_PROXY_ENABLED") };
        assert!(TlsProxyConfig::from_env().is_none());
    }

    #[test]
    fn config_enabled_via_env() {
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
        unsafe { std::env::set_var("TLS_PROXY_ENABLED", "1") };
        assert!(TlsProxyConfig::from_env().is_some());
        unsafe { std::env::remove_var("TLS_PROXY_ENABLED") };
    }

    #[test]
    fn config_enabled_via_yes() {
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
            ca_dir: PathBuf::from("/tmp/ca"),
            bypass: DEFAULT_BYPASS.into(),
            timeout: DEFAULT_TIMEOUT,
        };
        assert_eq!(cfg.port(), Some(7890));
    }
}
