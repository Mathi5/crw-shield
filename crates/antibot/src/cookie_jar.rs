//! Minimal in-memory cookie jar keyed by host.
//!
//! The store shape mirrors what the task spec asked for:
//!
//! ```text
//! host -> name -> value
//! ```
//!
//! wrapped in a single `Mutex` so the jar is `Send + Sync` and can be shared
//! across the HTTP fetcher, the CDP fetcher and any escalation steps via
//! `Arc<CookieJar>`.
//!
//! This jar is intentionally small:
//!
//! * It does **not** honour `Path`, `Secure`, `HttpOnly`, `SameSite` or
//!   `Domain=` attributes other than the host-level suffix match.
//! * It does not persist to disk.
//! * Cookies with no expiration are kept forever (until replaced or
//!   explicitly cleared); cookies with a `max_age_secs` get dropped by
//!   `clear_expired`.
//!
//! Despite those limitations, this is enough to round-trip session cookies
//! (`session-id`, `x-amz-rid`, `cf_clearance`, DataDome `dd`, ...) between
//! fetchers and across requests, which is the highest-ROI gap identified in
//! `ANTIBOT_RESEARCH.md`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// A single cookie entry tracked by the jar.
#[derive(Debug, Clone)]
struct Entry {
    value: String,
    /// Absolute expiration time. `None` means the cookie has no expiration
    /// (treated as a session cookie and kept until manually replaced or
    /// removed via `clear`).
    expires_at: Option<SystemTime>,
}

#[derive(Debug, Default)]
struct Inner {
    /// `host -> (cookie name -> entry)`
    cookies: HashMap<String, HashMap<String, Entry>>,
}

/// Thread-safe cookie jar keyed by host.
#[derive(Debug, Default)]
pub struct CookieJar {
    inner: Mutex<Inner>,
}

impl CookieJar {
    /// Build a fresh, empty cookie jar.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Store a cookie for the given host. If `max_age_secs` is `Some(d)`, the
    /// cookie is considered expired `d` seconds from now. If `None`, the
    /// cookie is kept until replaced.
    pub fn set_cookie(&self, host: &str, name: &str, value: &str, max_age_secs: Option<u64>) {
        let expires_at = max_age_secs.and_then(|secs| {
            if secs == 0 {
                // Treat 0-second lifetime as "expired immediately".
                Some(SystemTime::now())
            } else {
                SystemTime::now().checked_add(Duration::from_secs(secs))
            }
        });
        let entry = Entry {
            value: value.to_string(),
            expires_at,
        };
        let mut guard = self.inner.lock().expect("cookie jar mutex poisoned");
        guard
            .cookies
            .entry(normalize_host(host))
            .or_default()
            .insert(name.to_string(), entry);
    }

    /// Build the value for a `Cookie:` request header for `url`. Cookies are
    /// matched against the URL host and all of its parent domains
    /// (`sub.example.com` first, then `example.com`, then `com`), so a cookie
    /// stored for `example.com` is reused for `sub.example.com`.
    ///
    /// Returns `None` when no cookies are registered for this host.
    pub fn cookie_header_for(&self, url: &str) -> Option<String> {
        let host = url_host(url)?;
        let guard = self.inner.lock().expect("cookie jar mutex poisoned");
        let mut parts: Vec<String> = Vec::new();
        for host in host_candidates(&host) {
            if let Some(bucket) = guard.cookies.get(&host) {
                for (name, entry) in bucket {
                    if is_expired(entry.expires_at) {
                        continue;
                    }
                    parts.push(format!("{name}={}", entry.value));
                }
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }

    /// Drop every cookie whose `expires_at` is in the past. Cookies with no
    /// expiration are left alone.
    pub fn clear_expired(&self) {
        let mut guard = self.inner.lock().expect("cookie jar mutex poisoned");
        for bucket in guard.cookies.values_mut() {
            bucket.retain(|_, entry| !is_expired(entry.expires_at));
        }
        guard.cookies.retain(|_, bucket| !bucket.is_empty());
    }

    /// Total number of cookies currently stored (including expired ones that
    /// have not yet been swept by `clear_expired`). Mostly useful for tests.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("cookie jar mutex poisoned");
        guard.cookies.values().map(|b| b.len()).sum()
    }

    /// Parse a `Set-Cookie` header value (just the first line of it — the
    /// spec deals with the value as it appears in HTTP wire format) and store
    /// it in the jar.
    ///
    /// Supports the small subset of attributes we care about:
    /// * `name=value`
    /// * `Max-Age=<seconds>`
    /// * `Domain=<domain>` (leading dot stripped)
    /// * `expires` / `path` / `secure` / `httponly` / `samesite` are ignored
    ///   beyond what affects the host keying.
    pub fn set_from_set_cookie(&self, url_host: &str, set_cookie: &str) {
        let mut parts = set_cookie.split(';');
        let Some(first) = parts.next() else { return };
        let Some((name, value)) = first.split_once('=') else {
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let value = value.trim();

        let mut max_age_secs: Option<u64> = None;
        let mut domain: Option<String> = None;
        for raw in parts {
            let attr = raw.trim();
            if attr.is_empty() {
                continue;
            }
            if let Some((k, v)) = attr.split_once('=') {
                let key = k.trim().to_ascii_lowercase();
                match key.as_str() {
                    "max-age" => {
                        if let Ok(n) = v.trim().parse::<i64>() {
                            if n >= 0 {
                                max_age_secs = Some(n as u64);
                            }
                        }
                    }
                    "domain" => {
                        let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                        if !d.is_empty() {
                            domain = Some(d);
                        }
                    }
                    _ => {}
                }
            } else {
                // `Secure`, `HttpOnly`, `SameSite=...`, `Path=...` etc — we
                // ignore these attributes beyond the host keying decision.
                let _ = attr.to_ascii_lowercase();
            }
        }

        let host = domain.unwrap_or_else(|| normalize_host(url_host));
        self.set_cookie(&host, name, value, max_age_secs);
    }

    /// Snapshot of all cookies for `host`, used by tests.
    #[cfg(test)]
    pub fn cookies_for(&self, host: &str) -> Vec<(String, String)> {
        let guard = self.inner.lock().expect("cookie jar mutex poisoned");
        guard
            .cookies
            .get(&normalize_host(host))
            .map(|bucket| {
                bucket
                    .iter()
                    .map(|(k, v)| (k.clone(), v.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn is_expired(expires_at: Option<SystemTime>) -> bool {
    match expires_at {
        Some(t) => t <= SystemTime::now(),
        None => false,
    }
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Extract the host (without port) from a URL string. Falls back to `None` if
/// the URL is unparseable — in that case no cookies are returned, which is
/// safe.
fn url_host(url: &str) -> Option<String> {
    // Try URL parsing first; if that fails, fall back to a manual split.
    if let Ok(parsed) = url::Url::parse(url) {
        return parsed.host_str().map(normalize_host);
    }
    // Manual fallback: `scheme://host/...` or `host/...`.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host_part = after_scheme
        .split_once('/')
        .map(|(h, _)| h)
        .unwrap_or(after_scheme);
    let host = host_part.split(':').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(normalize_host(host))
    }
}

/// Walk up the domain tree, returning the host itself first, then each parent
/// domain. Always includes at least one entry.
fn host_candidates(host: &str) -> Vec<String> {
    let normalized = normalize_host(host);
    let mut out: Vec<String> = Vec::new();
    if normalized.is_empty() {
        return out;
    }
    out.push(normalized.clone());
    let mut current = normalized.as_str();
    while let Some(idx) = current.find('.') {
        current = &current[idx + 1..];
        if current.is_empty() {
            break;
        }
        out.push(current.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_cookie_for_same_host() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "session", "abc", None);
        let header = jar.cookie_header_for("https://example.com/").unwrap();
        assert_eq!(header, "session=abc");
    }

    #[test]
    fn cookie_header_matches_parent_domains() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "sid", "root", None);
        let header = jar
            .cookie_header_for("https://sub.example.com/path")
            .unwrap();
        assert_eq!(header, "sid=root");
    }

    #[test]
    fn cookie_header_does_not_leak_to_other_domains() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "sid", "root", None);
        assert!(jar.cookie_header_for("https://other.com/").is_none());
        assert!(jar.cookie_header_for("https://notexample.com/").is_none());
    }

    #[test]
    fn clear_expired_removes_only_expired_entries() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "session", "abc", None);
        jar.set_cookie("example.com", "flash", "xyz", Some(0));
        jar.clear_expired();
        // Flash should be gone; session remains.
        let header = jar.cookie_header_for("https://example.com/").unwrap();
        assert!(header.contains("session=abc"));
        assert!(!header.contains("flash"));
    }

    #[test]
    fn multiple_cookies_are_joined_in_header() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "a", "1", None);
        jar.set_cookie("example.com", "b", "2", None);
        let header = jar.cookie_header_for("https://example.com/").unwrap();
        assert!(header.contains("a=1"));
        assert!(header.contains("b=2"));
        assert!(header.contains("; "));
    }

    #[test]
    fn set_from_set_cookie_parses_max_age() {
        let jar = CookieJar::new();
        jar.set_from_set_cookie(
            "example.com",
            "dd=xyz123; Max-Age=3600; Domain=.example.com",
        );
        let header = jar.cookie_header_for("https://sub.example.com/").unwrap();
        assert!(header.contains("dd=xyz123"));
    }

    #[test]
    fn set_from_set_cookie_handles_no_attributes() {
        let jar = CookieJar::new();
        jar.set_from_set_cookie("example.com", "cf_clearance=abc");
        let header = jar.cookie_header_for("https://example.com/").unwrap();
        assert_eq!(header, "cf_clearance=abc");
    }

    #[test]
    fn host_candidates_walks_parent_domains() {
        let c = host_candidates("a.b.example.com");
        assert_eq!(
            c,
            vec!["a.b.example.com", "b.example.com", "example.com", "com"]
        );
    }

    #[test]
    fn invalid_url_returns_none() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "x", "1", None);
        // Missing scheme but valid host — should still pick up the cookie via
        // the manual fallback.
        let header = jar.cookie_header_for("example.com/foo");
        assert_eq!(header.as_deref(), Some("x=1"));
    }
}
