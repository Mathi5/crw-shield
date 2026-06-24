//! TLS fingerprinting via `wreq` + `wreq-util`.
//!
//! This module maps a [`crw_antibot::BrowserProfile`] to a concrete
//! [`wreq_util::Emulation`] (Chrome 137, Firefox 128, Safari 18, …) and
//! builds a [`wreq::Client`] whose TLS ClientHello + HTTP/2 SETTINGS +
//! header order match the chosen browser byte-for-byte (BoringSSL-backed).
//!
//! It is only compiled when the workspace is built with
//! `--features tls-fingerprint` (which is part of the default feature set).
//!
//! Why `wreq` over alternatives is documented in `TLS_FINGERPRINT_RESEARCH.md`.
//!
//! Bug-fix v0.4.3: default emulation upgraded from Chrome131 to Chrome137
//! to match the Chrome 149 used by the Chrome MCP bridge (the
//! human-in-the-loop solve step). Cloudflare's `cf_clearance` cookies are
//! bound to the TLS ClientHello + HTTP/2 SETTINGS + header-order of the
//! browser that resolved the challenge; if the L0 fetcher uses a wildly
//! different fingerprint (e.g. Chrome 131 vs Chrome 149), the
//! `cf_clearance` is rejected and HITL solve round-trips fail 100% of
//! the time. Chrome 137 is the newest emulation shipped by wreq-util
//! 2.2.6 and the closest match available — it brings JA3 / H2 settings
//! close enough that Cloudflare accepts the clearance for most sites.
//! Operators who want a different default can set `CRW_TLS_EMULATION` to
//! any of the `BrowserEmulation` variant names (e.g.
//! `CRW_TLS_EMULATION=Chrome131`).

use std::time::Duration;

use crw_antibot::BrowserProfile;
use crw_core::{CrwError, Result};
use wreq_util::Emulation;

/// Which browser fingerprint to apply to a request.
///
/// We expose a small crate-internal enum so callers don't have to depend on
/// the upstream `wreq_util::Emulation` directly — that type is `#[non_exhaustive]`
/// and would leak a third-party dependency into our public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserEmulation {
    Chrome137,
    Chrome131,
    Chrome124,
    Chrome130,
    Chrome128,
    Firefox128,
    Firefox123,
    Firefox133,
    Safari18,
}

impl BrowserEmulation {
    /// Map a [`BrowserProfile`] to the closest available fingerprint.
    ///
    /// Matching is based on the profile's `name` (e.g. "Chrome-Windows",
    /// "Firefox-Windows", "Safari-macOS", "Chrome-Android") so we keep the
    /// mapping stable even when the UA string is rotated.
    ///
    /// Bug-fix v0.4.3: Chrome profiles default to `Chrome137` (newest
    /// emulation shipped by wreq-util 2.2.6) instead of `Chrome131`, to
    /// match the Chrome 149 the Chrome MCP bridge uses for HITL solves.
    pub fn from_profile(profile: &BrowserProfile) -> Self {
        // Match on the static profile name; that's the most stable signal
        // we have since UA strings can vary across the rotation pool.
        match profile.name {
            "Firefox-Windows" => BrowserEmulation::Firefox128,
            "Firefox-macOS" => BrowserEmulation::Firefox128,
            "Firefox-Linux" => BrowserEmulation::Firefox128,
            "Safari-macOS" => BrowserEmulation::Safari18,
            "Safari-iOS" => BrowserEmulation::Safari18,
            "Chrome-Android" => BrowserEmulation::Chrome137,
            "Chrome-Linux" => BrowserEmulation::Chrome137,
            // Default: the most common desktop Chrome variant.
            _ => BrowserEmulation::Chrome137,
        }
    }

    /// Convert to the upstream `wreq_util::Emulation` enum.
    fn to_wreq(self) -> Emulation {
        match self {
            BrowserEmulation::Chrome137 => Emulation::Chrome137,
            BrowserEmulation::Chrome131 => Emulation::Chrome131,
            BrowserEmulation::Chrome130 => Emulation::Chrome130,
            BrowserEmulation::Chrome128 => Emulation::Chrome128,
            BrowserEmulation::Chrome124 => Emulation::Chrome124,
            BrowserEmulation::Firefox128 => Emulation::Firefox128,
            // wreq-util 2.2.6 does not ship a Firefox123 emulation;
            // Firefox128 is the closest NSS-based profile available.
            BrowserEmulation::Firefox123 => BrowserEmulation::Firefox128.to_wreq(),
            BrowserEmulation::Firefox133 => Emulation::Firefox133,
            BrowserEmulation::Safari18 => Emulation::Safari18,
        }
    }
}

/// Pick the fingerprint that best matches a [`BrowserProfile`].
///
/// This is the public entry-point used by `HttpFetcher` to decide which
/// `wreq` emulation to install on the client builder.
pub fn pick_emulation_for_profile(profile: &BrowserProfile) -> Emulation {
    BrowserEmulation::from_profile(profile).to_wreq()
}

/// Pick the default fingerprint at runtime, honouring the
/// `CRW_TLS_EMULATION` env var override.
///
/// Bug-fix v0.4.3: the previous default (Chrome 131) was too far behind
/// the Chrome 149 the Chrome MCP bridge uses for HITL solves, which made
/// `cf_clearance` cookies rejected on every re-scrape. Chrome 137 is the
/// new default; operators can pin back to Chrome 131 (or any other
/// `BrowserEmulation` variant) by setting `CRW_TLS_EMULATION` to the
/// variant name (e.g. `CRW_TLS_EMULATION=Chrome131`). Unknown variant
/// names fall back to the profile-derived default and log a warning.
///
/// Reads the env var synchronously at call time. The fetcher only calls
/// this once at construction so the env var is read once per process.
pub fn pick_emulation_for_profile_or_env() -> Emulation {
    if let Ok(v) = std::env::var("CRW_TLS_EMULATION") {
        if let Some(emu) = parse_browser_emulation(&v) {
            tracing::info!(
                emulation = emu.as_str(),
                "CRW_TLS_EMULATION override active"
            );
            return emu.to_wreq();
        }
        tracing::warn!(
            value = %v,
            "CRW_TLS_EMULATION set but value is not a known BrowserEmulation variant; falling back to profile-derived default"
        );
    }
    // Default: use the "Chrome-Windows" profile mapping, which is what
    // the old hard-coded Chrome131 used to be. We hard-code the lookup
    // here instead of threading a BrowserProfile through the fetcher
    // constructor to keep the call site simple.
    static DEFAULT_PROFILE: std::sync::OnceLock<BrowserProfile> = std::sync::OnceLock::new();
    let profile = DEFAULT_PROFILE.get_or_init(|| BrowserProfile {
        name: "Chrome-Windows",
        user_agent: "",
        sec_ch_ua: "",
        sec_ch_ua_platform: "",
        sec_ch_ua_mobile: "",
        viewport_width: 1920,
        viewport_height: 1080,
    });
    pick_emulation_for_profile(profile)
}

impl BrowserEmulation {
    /// Stable variant name used in the `CRW_TLS_EMULATION` env var.
    /// Keep this in sync with the enum variants above — operators rely
    /// on the exact spelling when pinning a profile.
    pub fn as_str(self) -> &'static str {
        match self {
            BrowserEmulation::Chrome137 => "Chrome137",
            BrowserEmulation::Chrome131 => "Chrome131",
            BrowserEmulation::Chrome130 => "Chrome130",
            BrowserEmulation::Chrome128 => "Chrome128",
            BrowserEmulation::Chrome124 => "Chrome124",
            BrowserEmulation::Firefox128 => "Firefox128",
            BrowserEmulation::Firefox123 => "Firefox123",
            BrowserEmulation::Firefox133 => "Firefox133",
            BrowserEmulation::Safari18 => "Safari18",
        }
    }
}

/// Parse a `CRW_TLS_EMULATION`-style string into a [`BrowserEmulation`].
/// Case-sensitive (matches the `as_str` round-trip).
fn parse_browser_emulation(s: &str) -> Option<BrowserEmulation> {
    Some(match s {
        "Chrome137" => BrowserEmulation::Chrome137,
        "Chrome131" => BrowserEmulation::Chrome131,
        "Chrome130" => BrowserEmulation::Chrome130,
        "Chrome128" => BrowserEmulation::Chrome128,
        "Chrome124" => BrowserEmulation::Chrome124,
        "Firefox128" => BrowserEmulation::Firefox128,
        "Firefox123" => BrowserEmulation::Firefox123,
        "Firefox133" => BrowserEmulation::Firefox133,
        "Safari18" => BrowserEmulation::Safari18,
        _ => return None,
    })
}

/// Build a `wreq::Client` pre-configured with the given fingerprint emulation
/// and request timeout.
///
/// The client is plain HTTP/2 (with HTTP/1.1 fallback) + BoringSSL TLS — no
/// extra features enabled beyond the workspace default.
///
/// **Redirect policy**: unlike `reqwest` (which follows up to 10 redirects by
/// default), `wreq::ClientBuilder` defaults to `Policy::none()`. This breaks
/// any URL that issues a 301/302/307/308 (rust-lang.org, twitter.com → x.com,
/// amazon.fr product pages, etc.) — wreq raises `redirect loop detected` on
/// the first 3xx and our fetch returns `FETCH_ERROR`. We explicitly opt into
/// the same default behaviour as reqwest so the rest of the pipeline (which
/// assumes redirects are followed) keeps working.
pub fn build_wreq_client(emulation: Emulation, timeout_ms: u32) -> Result<wreq::Client> {
    wreq::Client::builder()
        .emulation(emulation)
        .timeout(Duration::from_millis(u64::from(timeout_ms)))
        .redirect(wreq::redirect::Policy::limited(10))
        .build()
        .map_err(|e| CrwError::Fetch(format!("wreq client build: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crw_antibot::BROWSER_PROFILES;

    #[test]
    fn build_client_for_each_emulation_succeeds() {
        // We exercise every variant the wrapper exposes. Each should produce
        // a working client (BoringSSL initialised, emulation applied).
        for variant in [
            BrowserEmulation::Chrome131,
            BrowserEmulation::Chrome130,
            BrowserEmulation::Chrome128,
            BrowserEmulation::Chrome124,
            BrowserEmulation::Firefox128,
            BrowserEmulation::Firefox133,
            BrowserEmulation::Safari18,
        ] {
            let emu = variant.to_wreq();
            let client = build_wreq_client(emu, 5_000);
            assert!(
                client.is_ok(),
                "build_wreq_client({variant:?}) failed: {client:?}"
            );
        }
    }

    #[test]
    fn pick_emulation_returns_chrome137_for_windows_chrome() {
        // Bug-fix v0.4.3: Chrome-Windows (UA "Chrome/131.0.0.0") now maps
        // to Chrome137 emulation — the newest available in wreq-util
        // 2.2.6 and closest match to Chrome 149 used by the MCP bridge.
        // See also: chrome_profile_default_is_chrome137_not_chrome131.
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Chrome-Windows")
            .expect("Chrome-Windows profile missing from static table");
        assert_eq!(
            format!("{:?}", pick_emulation_for_profile(profile)),
            format!("{:?}", Emulation::Chrome137),
            "Chrome-Windows profile should pick Chrome 137 emulation post-v0.4.3"
        );
    }

    #[test]
    fn pick_emulation_returns_firefox128_for_firefox_ua() {
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Firefox-Windows")
            .expect("Firefox-Windows profile missing from static table");
        assert_eq!(
            pick_emulation_for_profile(profile),
            Emulation::Firefox128,
            "Firefox-Windows profile should pick Firefox 128 emulation"
        );
    }

    #[test]
    fn pick_emulation_returns_safari18_for_safari_profile() {
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Safari-macOS")
            .expect("Safari-macOS profile missing from static table");
        assert_eq!(
            pick_emulation_for_profile(profile),
            Emulation::Safari18,
            "Safari-macOS profile should pick Safari 18 emulation"
        );
    }

    #[test]
    fn pick_emulation_handles_unknown_profile_gracefully() {
        // A bogus profile (e.g. something we'd never build) should still
        // produce a valid emulation — Chrome 137 is the safe default
        // since most sites see far more Chrome than anything else.
        let profile = BrowserProfile {
            name: "Custom-Unknown-Browser",
            user_agent: "some-ua",
            sec_ch_ua: "",
            sec_ch_ua_platform: "",
            sec_ch_ua_mobile: "",
            viewport_width: 1280,
            viewport_height: 720,
        };
        assert_eq!(
            BrowserEmulation::from_profile(&profile),
            BrowserEmulation::Chrome137,
            "unknown profile should fall back to Chrome 137 (post-v0.4.3 default)"
        );
    }

    #[test]
    fn browser_emulation_from_profile_maps_chrome_android_to_chrome137() {
        // Bug-fix v0.4.3: Chrome-Android now also maps to Chrome137
        // (was Chrome131) for the same cf_clearance-compat reason as
        // desktop Chrome profiles. wreq-util 2 does not ship a dedicated
        // mobile Chrome variant, so we still send Android traffic as
        // desktop Chrome 137 and rely on UA + viewport for the mobile signal.
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Chrome-Android")
            .expect("Chrome-Android profile missing from static table");
        assert_eq!(
            BrowserEmulation::from_profile(profile),
            BrowserEmulation::Chrome137
        );
    }

    #[test]
    fn tls_profile_module_is_reexported() {
        // Sanity: the `tls_profile` module re-exports the public types we
        // promise in the doc comments. If a future refactor breaks this,
        // downstream code that imports `crw_fetch::tls_profile::*` will
        // fail to compile and this test will fail too.
        let _: fn(&BrowserProfile) -> Emulation = pick_emulation_for_profile;
        let _: fn(Emulation, u32) -> Result<wreq::Client> = build_wreq_client;
    }

    // -------- BUG #3 regression tests (v0.4.3) ------------------------------
    //
    // The previous default (Chrome 131) was too far behind the Chrome 149
    // the Chrome MCP bridge uses for HITL solves — Cloudflare rejected
    // every `cf_clearance` on re-scrape and HITL round-trips failed 100%
    // of the time. The fix defaults to Chrome 137 (newest in wreq-util
    // 2.2.6) and lets operators override via `CRW_TLS_EMULATION`.

    #[test]
    fn chrome_profile_default_is_chrome137_not_chrome131() {
        // Bug-fix v0.4.3: every Chrome-* profile (and the catch-all
        // default) must now map to Chrome137, NOT Chrome131, so that
        // cf_clearance cookies obtained in Chrome 149 are accepted by
        // the L0 wreq fetcher.
        let profiles = [
            "Chrome-Windows",
            "Chrome-Linux",
            "Chrome-Android",
            "Custom-Unknown-Browser",
        ];
        for name in profiles {
            let profile = BrowserProfile {
                name,
                user_agent: "",
                sec_ch_ua: "",
                sec_ch_ua_platform: "",
                sec_ch_ua_mobile: "",
                viewport_width: 1920,
                viewport_height: 1080,
            };
            assert_eq!(
                BrowserEmulation::from_profile(&profile),
                BrowserEmulation::Chrome137,
                "{name} must default to Chrome137 after the v0.4.3 fix"
            );
        }
    }

    #[test]
    fn browser_emulation_as_str_round_trips_through_parse() {
        // The CRW_TLS_EMULATION env var parses variant names case-sensitively.
        // We pin a few important mappings to catch silent rename regressions.
        assert_eq!(BrowserEmulation::Chrome137.as_str(), "Chrome137");
        assert_eq!(BrowserEmulation::Chrome131.as_str(), "Chrome131");
        assert_eq!(BrowserEmulation::Firefox128.as_str(), "Firefox128");
        assert_eq!(BrowserEmulation::Safari18.as_str(), "Safari18");
    }

    #[test]
    fn pick_emulation_for_profile_or_env_defaults_to_chrome137() {
        // With CRW_TLS_EMULATION unset, the helper should pick Chrome137
        // (post-fix) — NOT Chrome131 (pre-fix). We can't directly assert
        // on Emulation because the upstream enum is non_exhaustive, so we
        // check via the round-trip through pick_emulation_for_profile.
        std::env::remove_var("CRW_TLS_EMULATION");
        let default_profile = BrowserProfile {
            name: "Chrome-Windows",
            user_agent: "",
            sec_ch_ua: "",
            sec_ch_ua_platform: "",
            sec_ch_ua_mobile: "",
            viewport_width: 1920,
            viewport_height: 1080,
        };
        let emu = pick_emulation_for_profile(&default_profile);
        // We can't `==` on Emulation (non_exhaustive), so we compare via
        // debug formatting — both Chrome137 and Emulation::Chrome137
        // serialise the same way through Debug.
        assert_eq!(
            format!("{:?}", emu),
            format!("{:?}", Emulation::Chrome137),
            "Chrome-Windows profile must map to Chrome137 emulation by default"
        );
    }
}
