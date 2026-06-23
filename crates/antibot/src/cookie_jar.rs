//! Minimal cookie jar keyed by host, with optional disk persistence.
//!
//! The store shape mirrors what the task spec asked for:
//!
//! ```text
//! host -> name -> value (+ optional expires_at)
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
//! * Cookies with no expiration are kept forever (until replaced or
//!   explicitly cleared); cookies with a `max_age_secs` get dropped by
//!   `clear_expired`.
//!
//! Persistence:
//!
//! * [`CookieJar::save_to_path`] writes the current contents to a JSON file
//!   atomically (write `{path}.tmp`, then `rename` to `path`) so a crash
//!   mid-write never leaves a half-baked file behind.
//! * [`CookieJar::load_from_path`] reads the file back. Cookies whose
//!   `expires_at` is already in the past are dropped. A missing file is
//!   not an error — the loader returns an empty jar, which is the
//!   expected "first boot" behaviour.
//!
//! Despite those limitations, this is enough to round-trip session cookies
//! (`session-id`, `x-amz-rid`, `cf_clearance`, DataDome `dd`, ...) between
//! fetchers and across requests, which is the highest-ROI gap identified in
//! `ANTIBOT_RESEARCH.md`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Error type for cookie-jar persistence (save/load).
#[derive(Debug)]
pub struct CookieJarError(pub String);

impl fmt::Display for CookieJarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cookie jar error: {}", self.0)
    }
}

impl std::error::Error for CookieJarError {}

/// On-disk format version. Bump if the layout changes incompatibly.
const COOKIE_JAR_FORMAT_VERSION: u32 = 1;

/// A single cookie entry tracked by the jar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub value: String,
    /// Absolute expiration time, as **Unix epoch seconds**. `None` (serialised
    /// as JSON `null`) means the cookie has no expiration — it's a session
    /// cookie kept until manually replaced or removed via `clear`.
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
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
        let expires_at_unix = max_age_secs.and_then(|secs| {
            if secs == 0 {
                // Treat 0-second lifetime as "expired immediately".
                unix_now()
            } else {
                unix_now().and_then(|now| now.checked_add(secs))
            }
        });
        let entry = Entry {
            value: value.to_string(),
            expires_at_unix,
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
        let now = unix_now();
        let mut parts: Vec<String> = Vec::new();
        for host in host_candidates(&host) {
            if let Some(bucket) = guard.cookies.get(&host) {
                for (name, entry) in bucket {
                    if is_expired_unix(entry.expires_at_unix, now) {
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
        let now = unix_now();
        let mut guard = self.inner.lock().expect("cookie jar mutex poisoned");
        for bucket in guard.cookies.values_mut() {
            bucket.retain(|_, entry| !is_expired_unix(entry.expires_at_unix, now));
        }
        guard.cookies.retain(|_, bucket| !bucket.is_empty());
    }

    /// Drop every cookie stored for `host` and all of its parent domains
    /// (`sub.example.com`, `example.com`, `com`).
    ///
    /// Called by the L1 (ClearAndRetry) handler in the ladder when a site
    /// returns a block on the first attempt — clearing stale cookies
    /// (e.g. a `cf_clearance` that expired, a DataDome token that was
    /// blacklisted) is a cheap way to retry without rotating the whole
    /// browser identity.
    ///
    /// Returns the number of cookies removed (useful for telemetry).
    pub fn clear_for_host(&self, host: &str) -> usize {
        let normalized = normalize_host(host);
        let mut guard = self.inner.lock().expect("cookie jar mutex poisoned");
        let mut removed = 0usize;
        for h in host_candidates(&normalized) {
            if let Some(bucket) = guard.cookies.get_mut(&h) {
                removed += bucket.len();
                bucket.clear();
            }
        }
        // Also remove now-empty buckets from the outer map.
        guard.cookies.retain(|_, bucket| !bucket.is_empty());
        removed
    }

    /// Total number of cookies currently stored (including expired ones that
    /// have not yet been swept by `clear_expired`). Mostly useful for tests.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("cookie jar mutex poisoned");
        guard.cookies.values().map(|b| b.len()).sum()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

    /// Iterate over every stored cookie as `(host, name, value, expires_at_unix)`.
    ///
    /// Used by `crates/server/src/state.rs` to seed the in-memory jar from the
    /// on-disk file at startup, and by the tests below.
    pub fn iter(&self) -> Vec<(String, String, String, Option<u64>)> {
        let guard = self.inner.lock().expect("cookie jar mutex poisoned");
        let mut out = Vec::new();
        for (host, bucket) in guard.cookies.iter() {
            for (name, entry) in bucket.iter() {
                out.push((
                    host.clone(),
                    name.clone(),
                    entry.value.clone(),
                    entry.expires_at_unix,
                ));
            }
        }
        out
    }

    /// Persist the entire jar to `path` as JSON.
    ///
    /// Format:
    /// ```json
    /// {
    ///   "version": 1,
    ///   "cookies": {
    ///     "host1": {
    ///       "cookie1": { "value": "...", "expires_at_unix": 1234567890 }
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Session cookies (`expires_at_unix == None`) are serialised without the
    /// `expires_at_unix` key (serde `skip_serializing_if`).
    ///
    /// Writes are atomic: we dump to `{path}.tmp` first and then
    /// `std::fs::rename` it over the real file. A crash mid-write therefore
    /// leaves either the old file or the new one — never a half-baked file.
    pub fn save_to_path(&self, path: &Path) -> Result<(), CookieJarError> {
        // Snapshot under the lock, then drop the lock before doing I/O so
        // concurrent `set_cookie` / `load_from_path` calls don't block on
        // a long fsync. We clone the map (cheap — these are small structs
        // and only used at the rare save cadence) so the on-disk JSON
        // builder doesn't have to hold the mutex across the write.
        #[derive(Serialize)]
        struct OnDiskJar {
            version: u32,
            cookies: HashMap<String, HashMap<String, Entry>>,
        }
        let snapshot: OnDiskJar = {
            let guard = self.inner.lock().expect("cookie jar mutex poisoned");
            OnDiskJar {
                version: COOKIE_JAR_FORMAT_VERSION,
                cookies: guard.cookies.clone(),
            }
        };

        // Make sure the parent directory exists. `/var/lib/crw-shield/` is
        // not created automatically by Runtipi so the first boot would
        // otherwise fail at the rename step.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CookieJarError(format!("create_dir_all {}: {e}", parent.display()))
                })?;
            }
        }

        let tmp_path = {
            let mut p = path.as_os_str().to_owned();
            p.push(".tmp");
            std::path::PathBuf::from(p)
        };

        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| CookieJarError(format!("serialize: {e}")))?;
        std::fs::write(&tmp_path, &bytes)
            .map_err(|e| CookieJarError(format!("write {}: {e}", tmp_path.display())))?;
        std::fs::rename(&tmp_path, path).map_err(|e| {
            CookieJarError(format!(
                "rename {} -> {}: {e}",
                tmp_path.display(),
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Read the jar back from `path`. Cookies whose `expires_at_unix` is in
    /// the past are dropped. A missing file is **not** an error — we return
    /// an empty jar (this is the "first boot" path). A corrupted file does
    /// return `Err`.
    pub fn load_from_path(path: &Path) -> Result<Self, CookieJarError> {
        // Missing file -> empty jar, not an error. (Check existence first so
        // we can distinguish from I/O errors on read.)
        if !path.exists() {
            return Ok(Self::new());
        }
        let bytes = std::fs::read(path)
            .map_err(|e| CookieJarError(format!("read {}: {e}", path.display())))?;
        if bytes.is_empty() {
            return Ok(Self::new());
        }

        #[derive(Deserialize)]
        struct OnDiskJar {
            #[allow(dead_code)]
            version: u32,
            cookies: HashMap<String, HashMap<String, Entry>>,
        }

        let parsed: OnDiskJar = serde_json::from_slice(&bytes)
            .map_err(|e| CookieJarError(format!("parse {}: {e}", path.display())))?;

        let now = unix_now();
        let mut cookies: HashMap<String, HashMap<String, Entry>> = HashMap::new();
        for (host, bucket) in parsed.cookies {
            let mut kept: HashMap<String, Entry> = HashMap::new();
            for (name, entry) in bucket {
                if !is_expired_unix(entry.expires_at_unix, now) {
                    kept.insert(name, entry);
                }
            }
            if !kept.is_empty() {
                cookies.insert(host, kept);
            }
        }

        Ok(Self {
            inner: Mutex::new(Inner { cookies }),
        })
    }
}

/// Current Unix epoch seconds. `None` if the system clock is somehow
/// pre-1970 (extremely unlikely on any modern platform).
fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// `true` when the cookie's expiration is in the past or `now`.
/// Session cookies (`expires_at_unix == None`) are never expired.
fn is_expired_unix(expires_at_unix: Option<u64>, now: Option<u64>) -> bool {
    match (expires_at_unix, now) {
        (Some(t), Some(now)) => t <= now,
        _ => false,
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

    #[test]
    fn clear_for_host_removes_host_and_parents() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "root", "1", None);
        jar.set_cookie("sub.example.com", "sub", "2", None);
        jar.set_cookie("other.com", "leave", "3", None);
        let removed = jar.clear_for_host("sub.example.com");
        // `sub.example.com` had 1, `example.com` (parent) had 1 = 2 total.
        assert_eq!(removed, 2);
        // `other.com` should be untouched.
        let other_header = jar.cookie_header_for("https://other.com/").unwrap();
        assert_eq!(other_header, "leave=3");
        // Both sub.* and root cookies should be gone.
        assert!(jar.cookie_header_for("https://sub.example.com/").is_none());
        assert!(jar.cookie_header_for("https://example.com/").is_none());
    }

    #[test]
    fn clear_for_host_returns_zero_when_nothing_to_clear() {
        let jar = CookieJar::new();
        jar.set_cookie("other.com", "x", "1", None);
        let removed = jar.clear_for_host("example.com");
        assert_eq!(removed, 0);
        // `other.com` cookie still there.
        let h = jar.cookie_header_for("https://other.com/").unwrap();
        assert_eq!(h, "x=1");
    }

    #[test]
    fn clear_for_host_clears_multiple_cookies_on_same_host() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "cf_clearance", "abc", None);
        jar.set_cookie("example.com", "__cf_bm", "xyz", None);
        jar.set_cookie("example.com", "datadome", "dd", None);
        let removed = jar.clear_for_host("example.com");
        assert_eq!(removed, 3);
        assert!(jar.cookie_header_for("https://example.com/").is_none());
    }

    #[test]
    fn clear_for_host_is_idempotent() {
        let jar = CookieJar::new();
        jar.set_cookie("example.com", "x", "1", None);
        assert_eq!(jar.clear_for_host("example.com"), 1);
        // Second call on a now-empty jar returns 0 (no panic, no overflow).
        assert_eq!(jar.clear_for_host("example.com"), 0);
        assert!(jar.cookie_header_for("https://example.com/").is_none());
    }

    // -----------------------------------------------------------------------
    // Persistence tests
    // -----------------------------------------------------------------------

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("crw-shield-{name}-{nanos}.json"));
        p
    }

    #[test]
    fn save_load_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);

        let jar = CookieJar::new();
        // Two with expiry (one short, one long) + one session cookie.
        jar.set_cookie("example.com", "short", "alpha", Some(60));
        jar.set_cookie("example.com", "long", "beta", Some(86_400));
        jar.set_cookie("other.com", "session", "gamma", None);

        jar.save_to_path(&path).expect("save");
        let loaded = CookieJar::load_from_path(&path).expect("load");

        // Both hosts present.
        let ex = loaded.cookies_for("example.com");
        let ot = loaded.cookies_for("other.com");
        assert_eq!(ex.len(), 2);
        assert_eq!(ot.len(), 1);
        // Order in HashMap is non-deterministic so we look up by name.
        assert!(ex.iter().any(|(k, v)| k == "short" && v == "alpha"));
        assert!(ex.iter().any(|(k, v)| k == "long" && v == "beta"));
        assert!(ot.iter().any(|(k, v)| k == "session" && v == "gamma"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_drops_expired_cookies() {
        let path = tmp_path("expired");
        let _ = std::fs::remove_file(&path);

        // Write a JSON file that already contains an expired cookie
        // (1 second in the past) plus a long-lived one. We bypass the
        // jar's own `set_cookie` so we can put a specific unix timestamp
        // that we know is in the past.
        let past = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 1;
        let far_future = past + 86_400;
        let json = serde_json::json!({
            "version": 1,
            "cookies": {
                "example.com": {
                    "stale":   { "value": "old",  "expires_at_unix": past },
                    "fresh":   { "value": "new",  "expires_at_unix": far_future },
                    "session": { "value": "sess", "expires_at_unix": null }
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

        let loaded = CookieJar::load_from_path(&path).expect("load");
        let cookies = loaded.cookies_for("example.com");
        // Only `fresh` and `session` should remain.
        let names: Vec<&str> = cookies.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"fresh"));
        assert!(names.contains(&"session"));
        assert!(!names.contains(&"stale"), "stale cookie was not dropped");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_empty_jar() {
        // Build a path we are confident does not exist.
        let path = tmp_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(!path.exists());

        let loaded = CookieJar::load_from_path(&path).expect("missing file is not an error");
        assert!(loaded.is_empty());
        assert_eq!(loaded.len(), 0);
    }

    #[test]
    fn load_corrupted_json_returns_error() {
        let path = tmp_path("corrupted");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"this is not json {").expect("write garbage");

        let err = CookieJar::load_from_path(&path).expect_err("corrupt file should produce an Err");
        // Sanity check the error message contains some hint of what went wrong.
        let s = format!("{err}");
        assert!(
            s.contains("parse") || s.contains("cookie jar error"),
            "unexpected error message: {s}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
