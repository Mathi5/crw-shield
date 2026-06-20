//! Firefox / NSS-based browser profiles.
//!
//! Anti-bot stacks that have learned to fingerprint Chrome (Cloudflare
//! Bot Management, DataDome, Akamai Bot Manager) often fail to recognise
//! Firefox traffic because the TLS stack is different (NSS vs BoringSSL).
//! Adding a small pool of coherent Firefox profiles gives the ladder a
//! second identity to try when Chrome gets blocked.
//!
//! Design notes (adapted from cortex-bridge `src/profiles.rs`, MIT):
//!
//! - **Coherence**: every piece of identity in a profile (UA string,
//!   `Sec-Ch-Ua*` Client Hints, `navigator.userAgentData`) must match.
//!   Sending Chrome 131 UA with a Firefox TLS fingerprint is a bot
//!   signature by itself.
//! - **Stable pool**: we keep the profile per "identity" (one lifetime),
//!   not rotating per request. A real user doesn't change browser
//!   versions between page loads.
//! - **No Client Hints on Firefox**: Firefox does not support the
//!   `Sec-Ch-Ua*` family — those headers must be **absent**, not empty.
//! - **Canvas noise seed**: deterministic per profile so the canvas
//!   fingerprint stays consistent within one identity.
//!
//! This module is deliberately self-contained — it doesn't depend on the
//! existing `BrowserProfile` type (whose fields are too Chrome-centric).
//! Callers that want to use one of these profiles can map them back to a
//! `BrowserProfile` via [`FirefoxProfile::as_browser_profile`].

use serde::{Deserialize, Serialize};

/// `Sec-Ch-Ua*` Client Hint values for Firefox. **Must be empty strings
/// on the wire** — Firefox doesn't send these headers at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirefoxSecChUa {
    pub ua: String,
    pub mobile: String,
    pub platform: String,
    pub arch: String,
}

impl Default for FirefoxSecChUa {
    fn default() -> Self {
        Self {
            ua: String::new(),
            mobile: String::new(),
            platform: String::new(),
            arch: String::new(),
        }
    }
}

/// A coherent Firefox browser identity.
///
/// `tls_profile` names the upstream `wreq_util::Emulation` variant we
/// want to apply when using this profile (e.g. `Firefox128`,
/// `Firefox133`). On the HTTP level these correspond to NSS-based TLS
/// ClientHellos that fingerprint distinctly from Chrome/BoringSSL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirefoxProfile {
    /// Display name (for logs)
    pub name: String,
    /// Upstream wreq_util emulation key (e.g. `firefox_128`).
    pub tls_profile: String,
    /// Full User-Agent string (must match `tls_profile` era)
    pub user_agent: String,
    /// Always-empty Client Hints (Firefox does not send them).
    pub sec_ch_ua: FirefoxSecChUa,
    /// `navigator.userAgentData.getHighEntropyValues('uaFullVersion')`
    pub ua_full_version: String,
    /// Suffix for the persistent profile dir. Different suffixes give
    /// different cookies / IndexedDB / localStorage — useful so that a
    /// Firefox identity keeps its state separate from a Chrome one.
    pub profile_dir_suffix: String,
    /// Deterministic seed for canvas noise. Same profile = same noise,
    /// matching how a real user keeps their canvas fingerprint across
    /// sessions of the same browser install.
    pub canvas_noise_seed: u32,
}

impl FirefoxProfile {
    /// Map back to the legacy `BrowserProfile` so the existing
    /// `StealthHeaders::build()` machinery can use this identity.
    ///
    /// Note: this loses information (`canvas_noise_seed`,
    /// `profile_dir_suffix`) — those are only relevant for CDP sessions
    /// and will be plumbed in a later phase.
    pub fn as_browser_profile(&self) -> crate::BrowserProfile {
        crate::BrowserProfile {
            name: "Firefox-Linux", // Reuse existing slot
            user_agent: leaked_str(&self.user_agent),
            sec_ch_ua: "",
            sec_ch_ua_platform: "",
            sec_ch_ua_mobile: "",
            viewport_width: 1920,
            viewport_height: 1080,
        }
    }
}

/// Tiny helper to leak a `String` as `&'static str`. Only used at
/// module-init time for the static profile pool, so the leak is bounded.
fn leaked_str(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// Default Firefox pool — 3 identities across 2 versions.
///
/// `firefox_128` is the modern era (NSS 3.99+); `firefox_123` is older
/// and has a noticeably different TLS ClientHello, giving us diversity.
pub fn default_firefox_profiles() -> Vec<FirefoxProfile> {
    vec![
        // 1. Firefox 128 — current default for Firefox traffic.
        FirefoxProfile {
            name: "firefox-128-linux".to_string(),
            tls_profile: "firefox_128".to_string(),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0"
                .to_string(),
            sec_ch_ua: FirefoxSecChUa::default(),
            ua_full_version: "128.5.0".to_string(),
            profile_dir_suffix: "ff-128-default".to_string(),
            canvas_noise_seed: 0xFF12_8001,
        },
        // 2. Firefox 133 — newer major, distinct TLS fingerprint.
        FirefoxProfile {
            name: "firefox-133-linux".to_string(),
            tls_profile: "firefox_133".to_string(),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0"
                .to_string(),
            sec_ch_ua: FirefoxSecChUa::default(),
            ua_full_version: "133.0".to_string(),
            profile_dir_suffix: "ff-133".to_string(),
            canvas_noise_seed: 0xFF13_3002,
        },
        // 3. Firefox 123 — older TLS stack (NSS 3.91), distinct JA3.
        FirefoxProfile {
            name: "firefox-123-linux".to_string(),
            tls_profile: "firefox_123".to_string(),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0"
                .to_string(),
            sec_ch_ua: FirefoxSecChUa::default(),
            ua_full_version: "123.0".to_string(),
            profile_dir_suffix: "ff-123".to_string(),
            canvas_noise_seed: 0xFF12_3003,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pool_has_three_profiles() {
        let pool = default_firefox_profiles();
        assert_eq!(pool.len(), 3);
        assert_eq!(pool[0].name, "firefox-128-linux");
        assert_eq!(pool[1].name, "firefox-133-linux");
        assert_eq!(pool[2].name, "firefox-123-linux");
    }

    #[test]
    fn all_profiles_have_distinct_tls_profiles() {
        let pool = default_firefox_profiles();
        let mut tls = std::collections::HashSet::new();
        for p in &pool {
            assert!(
                tls.insert(p.tls_profile.clone()),
                "duplicate tls_profile in pool: {}",
                p.tls_profile
            );
        }
    }

    #[test]
    fn all_profiles_have_distinct_canvas_noise_seeds() {
        let pool = default_firefox_profiles();
        let mut seeds = std::collections::HashSet::new();
        for p in &pool {
            assert!(
                seeds.insert(p.canvas_noise_seed),
                "duplicate canvas_noise_seed {}",
                p.canvas_noise_seed
            );
        }
    }

    #[test]
    fn all_profiles_have_distinct_profile_dir_suffix() {
        let pool = default_firefox_profiles();
        let mut dirs = std::collections::HashSet::new();
        for p in &pool {
            assert!(
                dirs.insert(p.profile_dir_suffix.clone()),
                "duplicate profile_dir_suffix {}",
                p.profile_dir_suffix
            );
        }
    }

    #[test]
    fn user_agent_matches_tls_profile_era() {
        let pool = default_firefox_profiles();
        for p in &pool {
            // UA must contain "rv:<version>" matching the tls_profile era.
            if p.tls_profile == "firefox_128" {
                assert!(p.user_agent.contains("rv:128.0"), "{}", p.user_agent);
            } else if p.tls_profile == "firefox_133" {
                assert!(p.user_agent.contains("rv:133.0"), "{}", p.user_agent);
            } else if p.tls_profile == "firefox_123" {
                assert!(p.user_agent.contains("rv:123.0"), "{}", p.user_agent);
            }
        }
    }

    #[test]
    fn sec_ch_ua_is_empty_on_all_firefox_profiles() {
        let pool = default_firefox_profiles();
        for p in &pool {
            assert_eq!(p.sec_ch_ua.ua, "", "{}: ua should be empty", p.name);
            assert_eq!(
                p.sec_ch_ua.platform, "",
                "{}: platform should be empty",
                p.name
            );
        }
    }

    #[test]
    fn as_browser_profile_preserves_user_agent() {
        let p = FirefoxProfile {
            name: "test".to_string(),
            tls_profile: "firefox_128".to_string(),
            user_agent: "Mozilla/5.0 (test) Firefox/128.0".to_string(),
            sec_ch_ua: FirefoxSecChUa::default(),
            ua_full_version: "128.0".to_string(),
            profile_dir_suffix: "test".to_string(),
            canvas_noise_seed: 42,
        };
        let bp = p.as_browser_profile();
        assert_eq!(bp.user_agent, "Mozilla/5.0 (test) Firefox/128.0");
        assert_eq!(bp.sec_ch_ua, "");
    }
}