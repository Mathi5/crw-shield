//! TLS fingerprinting via `wreq` + `wreq-util`.
//!
//! This module maps a [`crw_antibot::BrowserProfile`] to a concrete
//! [`wreq_util::Emulation`] (Chrome 131, Firefox 128, Safari 18, …) and
//! builds a [`wreq::Client`] whose TLS ClientHello + HTTP/2 SETTINGS +
//! header order match the chosen browser byte-for-byte (BoringSSL-backed).
//!
//! It is only compiled when the workspace is built with
//! `--features tls-fingerprint` (which is part of the default feature set).
//!
//! Why `wreq` over alternatives is documented in `TLS_FINGERPRINT_RESEARCH.md`.

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
    Chrome131,
    Chrome124,
    Chrome130,
    Chrome128,
    Firefox128,
    Firefox133,
    Safari18,
}

impl BrowserEmulation {
    /// Map a [`BrowserProfile`] to the closest available fingerprint.
    ///
    /// Matching is based on the profile's `name` (e.g. "Chrome-Windows",
    /// "Firefox-Windows", "Safari-macOS", "Chrome-Android") so we keep the
    /// mapping stable even when the UA string is rotated.
    pub fn from_profile(profile: &BrowserProfile) -> Self {
        // Match on the static profile name; that's the most stable signal
        // we have since UA strings can vary across the rotation pool.
        match profile.name {
            "Firefox-Windows" => BrowserEmulation::Firefox128,
            "Firefox-macOS" => BrowserEmulation::Firefox128,
            "Firefox-Linux" => BrowserEmulation::Firefox128,
            "Safari-macOS" => BrowserEmulation::Safari18,
            "Safari-iOS" => BrowserEmulation::Safari18,
            "Chrome-Android" => BrowserEmulation::Chrome131,
            "Chrome-Linux" => BrowserEmulation::Chrome131,
            // Default: the most common desktop Chrome variant.
            _ => BrowserEmulation::Chrome131,
        }
    }

    /// Convert to the upstream `wreq_util::Emulation` enum.
    fn to_wreq(self) -> Emulation {
        match self {
            BrowserEmulation::Chrome131 => Emulation::Chrome131,
            BrowserEmulation::Chrome130 => Emulation::Chrome130,
            BrowserEmulation::Chrome128 => Emulation::Chrome128,
            BrowserEmulation::Chrome124 => Emulation::Chrome124,
            BrowserEmulation::Firefox128 => Emulation::Firefox128,
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

/// Build a `wreq::Client` pre-configured with the given fingerprint emulation
/// and request timeout.
///
/// The client is plain HTTP/2 (with HTTP/1.1 fallback) + BoringSSL TLS — no
/// extra features enabled beyond the workspace default.
pub fn build_wreq_client(emulation: Emulation, timeout_ms: u32) -> Result<wreq::Client> {
    wreq::Client::builder()
        .emulation(emulation)
        .timeout(Duration::from_millis(u64::from(timeout_ms)))
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
    fn pick_emulation_returns_chrome131_for_windows_chrome() {
        // The Windows Chrome profile (UA "Chrome/131.0.0.0") should map
        // to the Chrome 131 fingerprint regardless of OS string.
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Chrome-Windows")
            .expect("Chrome-Windows profile missing from static table");
        assert_eq!(
            pick_emulation_for_profile(profile),
            Emulation::Chrome131,
            "Chrome-Windows profile should pick Chrome 131 emulation"
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
        // produce a valid emulation — Chrome 131 is the safe default since
        // most sites see far more Chrome than anything else.
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
            pick_emulation_for_profile(&profile),
            Emulation::Chrome131,
            "unknown profile should fall back to Chrome 131"
        );
    }

    #[test]
    fn browser_emulation_from_profile_maps_chrome_android_to_chrome131() {
        // wreq-util 2 does not ship a dedicated mobile Chrome variant
        // (its "FirefoxAndroid135" is Firefox-only), so we send Android
        // traffic as desktop Chrome 131 and rely on the rest of the
        // request (UA, viewport) to carry the mobile signal.
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Chrome-Android")
            .expect("Chrome-Android profile missing from static table");
        assert_eq!(
            BrowserEmulation::from_profile(profile),
            BrowserEmulation::Chrome131
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
}
