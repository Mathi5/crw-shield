//! HTTP stealth — user-agent rotation, browser profiles, headers, and rate
//! limiting with jitter. Ported from CortexScout.

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use url::Url;

/// 17 desktop + mobile user agents.
pub const USER_AGENTS: &[&str] = &[
    // Chrome desktop
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    // Firefox desktop
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.5; rv:128.0) Gecko/20100101 Firefox/128.0",
    "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0",
    // Safari desktop
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15",
    // Edge desktop
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
    // Chrome mobile
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/131.0.0.0 Mobile/15E148 Safari/604.1",
    // Firefox mobile
    "Mozilla/5.0 (Android 14; Mobile; rv:128.0) Gecko/128.0 Firefox/128.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/128.0 Mobile/15E148 Safari/605.1.15",
    // Safari mobile
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
    // Brave / Opera / Vivaldi
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 OPR/116.0.0.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Vivaldi/6.9.3447.41",
];

/// A complete browser profile — user-agent, sec-ch-ua headers, viewport.
#[derive(Debug, Clone)]
pub struct BrowserProfile {
    pub name: &'static str,
    pub user_agent: &'static str,
    pub sec_ch_ua: &'static str,
    pub sec_ch_ua_platform: &'static str,
    pub sec_ch_ua_mobile: &'static str,
    pub viewport_width: u32,
    pub viewport_height: u32,
}

pub const BROWSER_PROFILES: &[BrowserProfile] = &[
    BrowserProfile {
        name: "Chrome-Windows",
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        sec_ch_ua: r#""Chromium";v="131", "Not_A Brand";v="24", "Google Chrome";v="131""#,
        sec_ch_ua_platform: r#""Windows""#,
        sec_ch_ua_mobile: "?0",
        viewport_width: 1920,
        viewport_height: 1080,
    },
    BrowserProfile {
        name: "Chrome-macOS",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        sec_ch_ua: r#""Chromium";v="131", "Not_A Brand";v="24", "Google Chrome";v="131""#,
        sec_ch_ua_platform: r#""macOS""#,
        sec_ch_ua_mobile: "?0",
        viewport_width: 1440,
        viewport_height: 900,
    },
    BrowserProfile {
        name: "Firefox-Windows",
        user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0",
        sec_ch_ua: "",
        sec_ch_ua_platform: "",
        sec_ch_ua_mobile: "",
        viewport_width: 1920,
        viewport_height: 1080,
    },
    BrowserProfile {
        name: "Safari-macOS",
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
        sec_ch_ua: "",
        sec_ch_ua_platform: "",
        sec_ch_ua_mobile: "",
        viewport_width: 1440,
        viewport_height: 900,
    },
    BrowserProfile {
        name: "Chrome-Android",
        user_agent: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36",
        sec_ch_ua: r#""Chromium";v="131", "Not_A Brand";v="24", "Google Chrome";v="131""#,
        sec_ch_ua_platform: r#""Android""#,
        sec_ch_ua_mobile: "?1",
        viewport_width: 412,
        viewport_height: 915,
    },
];

/// User-agent rotator that uses a sliding window so we don't repeat within a
/// recent cycle.
#[derive(Debug)]
pub struct UserAgentRotator {
    pool: Vec<&'static str>,
    recent: Vec<usize>,
    rng: rand::rngs::StdRng,
}

impl Default for UserAgentRotator {
    fn default() -> Self {
        Self::new()
    }
}

impl UserAgentRotator {
    pub fn new() -> Self {
        // Seed from system time so the sequence varies between processes.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Self {
            pool: USER_AGENTS.to_vec(),
            recent: Vec::with_capacity(USER_AGENTS.len()),
            rng: rand::rngs::StdRng::seed_from_u64(seed),
        }
    }

    pub fn with_pool<I: IntoIterator<Item = &'static str>>(pool: I) -> Self {
        let pool: Vec<&'static str> = pool.into_iter().collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        Self {
            pool,
            recent: Vec::new(),
            rng: rand::rngs::StdRng::seed_from_u64(seed),
        }
    }

    /// Returns the next user agent, avoiding any UA already drawn within the
    /// last `pool.len()` draws.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> &'static str {
        if self.pool.is_empty() {
            return "";
        }
        let pool_len = self.pool.len();
        // If every UA was recently drawn, reset the history.
        if self.recent.len() >= pool_len {
            self.recent.clear();
        }
        let available: Vec<usize> = (0..pool_len).filter(|i| !self.recent.contains(i)).collect();
        let pick = available.choose(&mut self.rng).copied().unwrap_or(0);
        self.recent.push(pick);
        self.pool[pick]
    }

    pub fn len(&self) -> usize {
        self.pool.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }
}

/// Delay presets for rate limiting. Range is inclusive, jitter is ±20%.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayPreset {
    Polite,
    Aggressive,
    Conservative,
    Custom { min_ms: u64, max_ms: u64 },
}

impl DelayPreset {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "aggressive" => DelayPreset::Aggressive,
            "conservative" => DelayPreset::Conservative,
            "polite" => DelayPreset::Polite,
            other => {
                // parse "min-max" or fallback polite
                if let Some((a, b)) = other.split_once('-') {
                    if let (Ok(min), Ok(max)) = (a.parse::<u64>(), b.parse::<u64>()) {
                        return DelayPreset::Custom {
                            min_ms: min,
                            max_ms: max,
                        };
                    }
                }
                DelayPreset::Polite
            }
        }
    }

    pub fn min_ms(&self) -> u64 {
        match self {
            DelayPreset::Polite => 500,
            DelayPreset::Aggressive => 100,
            DelayPreset::Conservative => 1_000,
            DelayPreset::Custom { min_ms, .. } => *min_ms,
        }
    }

    pub fn max_ms(&self) -> u64 {
        match self {
            DelayPreset::Polite => 1_500,
            DelayPreset::Aggressive => 500,
            DelayPreset::Conservative => 3_000,
            DelayPreset::Custom { max_ms, .. } => *max_ms,
        }
    }
}

/// Thread-safe per-host delay tracker. Tracks the next allowed request time
/// using an atomic nanosecond timestamp.
#[derive(Debug)]
pub struct RequestDelay {
    preset: DelayPreset,
    next_allowed_ns: AtomicU64,
}

impl RequestDelay {
    pub fn new(preset: DelayPreset) -> Self {
        Self {
            preset,
            next_allowed_ns: AtomicU64::new(0),
        }
    }

    pub fn preset(&self) -> DelayPreset {
        self.preset
    }

    /// Compute and store the next allowed timestamp, returning the delay the
    /// caller must wait.
    pub fn next_delay(&self) -> Duration {
        let mut rng = rand::thread_rng();
        let min = self.preset.min_ms();
        let max = self.preset.max_ms();
        let base = if max > min {
            rng.gen_range(min..=max)
        } else {
            min
        };
        let jitter = (base as f64) * 0.2;
        let jitter_signed: i64 = rng.gen_range((-jitter) as i64..=jitter as i64);
        let total_ms = (base as i64 + jitter_signed).max(0) as u64;
        let duration = Duration::from_millis(total_ms);
        let now_ns = current_time_ns();
        let next = now_ns.saturating_add(duration.as_nanos() as u64);
        // We store the *latest* scheduled wake-up; if a later caller already
        // scheduled a later time, keep it.
        let prev = self.next_allowed_ns.load(Ordering::Relaxed);
        if next > prev {
            self.next_allowed_ns.store(next, Ordering::Relaxed);
        }
        duration
    }
}

fn current_time_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A set of HTTP headers that mimic a real browser. Header keys are normalised
/// to the casing we set (lowercase here, but reqwest normalises again).
#[derive(Debug, Clone)]
pub struct StealthHeaders {
    headers: HashMap<String, String>,
}

impl StealthHeaders {
    /// Build a bare-bones header set with only a User-Agent and the supplied
    /// extras. Used when stealth headers are disabled.
    pub fn minimal(profile: &BrowserProfile, extra: &HashMap<String, String>) -> Self {
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), profile.user_agent.to_string());
        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }
        Self { headers }
    }

    /// Build headers for the given URL using the supplied browser profile and
    /// extra headers (e.g. user-supplied `headers` from a scrape request).
    pub fn build(profile: &BrowserProfile, url: &Url, extra: &HashMap<String, String>) -> Self {
        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), profile.user_agent.to_string());
        headers.insert(
            "Accept".to_string(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
                .to_string(),
        );
        let is_english = url
            .host_str()
            .map(|h| {
                h.ends_with(".com")
                    || h.ends_with(".org")
                    || h.ends_with(".net")
                    || h.ends_with(".io")
            })
            .unwrap_or(true);
        let accept_lang = if is_english {
            "en-US,en;q=0.9"
        } else {
            "en-US,en;q=0.9,fr;q=0.8"
        };
        headers.insert("Accept-Language".to_string(), accept_lang.to_string());
        headers.insert(
            "Accept-Encoding".to_string(),
            "gzip, deflate, br".to_string(),
        );
        headers.insert("DNT".to_string(), "1".to_string());
        headers.insert("Connection".to_string(), "keep-alive".to_string());
        headers.insert("Upgrade-Insecure-Requests".to_string(), "1".to_string());
        headers.insert("Sec-Fetch-Dest".to_string(), "document".to_string());
        headers.insert("Sec-Fetch-Mode".to_string(), "navigate".to_string());
        headers.insert("Sec-Fetch-Site".to_string(), "none".to_string());
        headers.insert("Sec-Fetch-User".to_string(), "?1".to_string());
        headers.insert("Cache-Control".to_string(), "max-age=0".to_string());

        if !profile.sec_ch_ua.is_empty() {
            headers.insert("Sec-CH-UA".to_string(), profile.sec_ch_ua.to_string());
            headers.insert(
                "Sec-CH-UA-Platform".to_string(),
                profile.sec_ch_ua_platform.to_string(),
            );
            headers.insert(
                "Sec-CH-UA-Mobile".to_string(),
                profile.sec_ch_ua_mobile.to_string(),
            );
        }

        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }

        Self { headers }
    }

    pub fn as_pairs(&self) -> Vec<(&str, &str)> {
        self.headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(|s| s.as_str())
    }

    pub fn map(&self) -> &HashMap<String, String> {
        &self.headers
    }
}

/// Pick a random browser profile.
pub fn random_profile<R: Rng + ?Sized>(rng: &mut R) -> &'static BrowserProfile {
    let idx = rng.gen_range(0..BROWSER_PROFILES.len());
    &BROWSER_PROFILES[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agents_pool_has_17_entries() {
        assert_eq!(USER_AGENTS.len(), 17);
    }

    #[test]
    fn ua_rotation_no_repeat_on_full_cycle() {
        let mut rotator = UserAgentRotator::new();
        let mut seen = Vec::with_capacity(USER_AGENTS.len());
        for _ in 0..USER_AGENTS.len() {
            let ua = rotator.next();
            assert!(!seen.contains(&ua), "UA {ua} repeated within one cycle");
            seen.push(ua);
        }
        assert_eq!(seen.len(), USER_AGENTS.len());
        assert_eq!(rotator.len(), 17);
    }

    #[test]
    fn ua_rotation_cycles_completely() {
        let mut rotator = UserAgentRotator::new();
        let mut unique = std::collections::HashSet::new();
        for _ in 0..50 {
            unique.insert(rotator.next());
        }
        // Should eventually see all 17
        assert_eq!(unique.len(), USER_AGENTS.len());
    }

    #[test]
    fn detect_clean_html_no_challenge() {
        assert_eq!(
            crate::challenge_detect::detect_challenge("<html></html>"),
            None
        );
    }

    #[test]
    fn delay_preset_ranges() {
        assert_eq!(DelayPreset::Polite.min_ms(), 500);
        assert_eq!(DelayPreset::Polite.max_ms(), 1500);
        assert_eq!(DelayPreset::Aggressive.min_ms(), 100);
        assert_eq!(DelayPreset::Aggressive.max_ms(), 500);
        assert_eq!(DelayPreset::Conservative.min_ms(), 1000);
        assert_eq!(DelayPreset::Conservative.max_ms(), 3000);
        let custom = DelayPreset::Custom {
            min_ms: 200,
            max_ms: 400,
        };
        assert_eq!(custom.min_ms(), 200);
        assert_eq!(custom.max_ms(), 400);
    }

    #[test]
    fn delay_preset_from_str() {
        assert_eq!(DelayPreset::from_str("polite"), DelayPreset::Polite);
        assert_eq!(DelayPreset::from_str("AGGRESSIVE"), DelayPreset::Aggressive);
        assert_eq!(
            DelayPreset::from_str("100-300"),
            DelayPreset::Custom {
                min_ms: 100,
                max_ms: 300
            }
        );
        assert_eq!(DelayPreset::from_str("garbage"), DelayPreset::Polite);
    }

    #[test]
    fn request_delay_stays_within_jitter_range() {
        let d = RequestDelay::new(DelayPreset::Polite);
        for _ in 0..200 {
            let dur = d.next_delay();
            // 500..=1500 with ±20% jitter => 400..=1800 ms
            let ms = dur.as_millis() as u64;
            assert!(
                (400..=1800).contains(&ms),
                "delay {ms}ms out of expected range"
            );
        }
    }

    #[test]
    fn request_delay_atomic_progresses() {
        let d = RequestDelay::new(DelayPreset::Aggressive);
        let _ = d.next_delay();
        let v1 = d.next_allowed_ns.load(Ordering::Relaxed);
        assert!(v1 > 0);
    }

    #[test]
    fn stealth_headers_include_required_keys() {
        let profile = &BROWSER_PROFILES[0];
        let url = Url::parse("https://example.com").unwrap();
        let mut extra = HashMap::new();
        extra.insert("X-Test".to_string(), "1".to_string());
        let h = StealthHeaders::build(profile, &url, &extra);
        for k in [
            "User-Agent",
            "Accept",
            "Accept-Language",
            "Accept-Encoding",
            "DNT",
            "Connection",
            "Upgrade-Insecure-Requests",
            "Sec-Fetch-Dest",
            "Sec-Fetch-Mode",
            "Sec-Fetch-Site",
            "Sec-Fetch-User",
            "Cache-Control",
            "X-Test",
        ] {
            assert!(h.get(k).is_some(), "missing header {k}");
        }
        // Chrome profile should include sec-ch-ua
        assert!(h.get("Sec-CH-UA").is_some());
    }

    #[test]
    fn stealth_headers_non_chromium_omits_sec_ch_ua() {
        let profile = BROWSER_PROFILES
            .iter()
            .find(|p| p.name == "Firefox-Windows")
            .unwrap();
        let url = Url::parse("https://example.com").unwrap();
        let h = StealthHeaders::build(profile, &url, &HashMap::new());
        assert!(h.get("Sec-CH-UA").is_none());
    }

    #[test]
    fn random_profile_returns_a_known_profile() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let p = random_profile(&mut rng);
        assert!(BROWSER_PROFILES.iter().any(|bp| bp.name == p.name));
    }

    #[test]
    fn browser_profiles_have_unique_names() {
        let names: Vec<&str> = BROWSER_PROFILES.iter().map(|p| p.name).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len());
        assert_eq!(BROWSER_PROFILES.len(), 5);
    }
}
