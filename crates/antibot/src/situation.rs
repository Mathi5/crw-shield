//! `FetchSituation` — taxonomic classification of HTTP / CDP / anti-bot outcomes.
//!
//! Phase B replaces the boolean "`is_challenge` / `is_empty`" model with a
//! structured `FetchSituation` enum and a `SituationReport` that carries
//! both the diagnosis and the evidence that supports it. The ladder reads
//! the report to choose the *appropriate* next step (CDP, FlareSolverr,
//! retry, none) instead of escalating blindly.
//!
//! See `providers.toml` for the catalogue of known anti-bot provider
//! signatures the detector consults.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::situation::providers::ProviderEntry;

/// High-level category of the detected situation.
///
/// The enum is intentionally closed and flat — `FetchSituation` below names
/// the *exact* variant (e.g. `CloudflareIuam`). Use `FetchSituation::kind()`
/// when you only need the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SituationKind {
    /// Successful fetch with no anti-bot fingerprint detected.
    CleanSuccess,
    /// Page returned 2xx but content is empty / JS-only / soft-blocked.
    JsOnly,
    /// Cloudflare IUAM (JS challenge, no captcha).
    CloudflareIuam,
    /// Cloudflare Turnstile (invisible captcha challenge).
    CloudflareTurnstile,
    /// DataDome captcha / interstitial.
    DataDomeCaptcha,
    /// Akamai Bot Manager (fingerprinting + sensor data).
    AkamaiBotManager,
    /// Imperva / Incapsula WAF.
    ImpervaIncapsula,
    /// PerimeterX (now HUMAN) captcha.
    PerimeterX,
    /// reCAPTCHA v2/v3.
    Recaptcha,
    /// hCaptcha.
    Hcaptcha,
    /// AWS WAF.
    AwsWaf,
    /// Shape Security / F5.
    ShapeSecurity,
    /// Kasada (browser-side challenge).
    Kasada,
    /// Friendly Captcha (invisible).
    FriendlyCaptcha,
    /// GeeTest captcha.
    GeeTest,
    /// FunCaptcha / Arkose Labs.
    Funcaptcha,
    /// Yandex SmartCaptcha.
    YandexSmartCaptcha,
    /// Distil Networks WAF.
    Distil,
    /// Reblaze WAF.
    Reblaze,
    /// Generic device-fingerprint wall.
    GenericFingerprint,
    /// Generic "access denied" 403 (no specific provider identified).
    GenericAccessDenied,
    /// Generic "please verify you are a human" page.
    GenericVerify,
    /// Generic CDN/WAF block.
    GenericCdnBlock,
    /// Server-side rate limiting (429 or "rate limit" text).
    RateLimited,
    /// Geo-restriction (no software ladder can solve).
    GeoBlocked,
    /// Login wall (paywall, member-only, ...).
    LoginWall,
    /// Soft 404 (real "page not found" with HTTP 200/404, not a bot block).
    SoftNotFound,
    /// Server-side 5xx (retry with backoff).
    ServerError,
    /// Provider or situation we don't recognise.
    Unknown,
}

impl SituationKind {
    /// Short identifier used in JSON output and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SituationKind::CleanSuccess => "clean_success",
            SituationKind::JsOnly => "js_only",
            SituationKind::CloudflareIuam => "cloudflare_iuam",
            SituationKind::CloudflareTurnstile => "cloudflare_turnstile",
            SituationKind::DataDomeCaptcha => "datadome_captcha",
            SituationKind::AkamaiBotManager => "akamai_bot_manager",
            SituationKind::ImpervaIncapsula => "imperva_incapsula",
            SituationKind::PerimeterX => "perimeterx",
            SituationKind::Recaptcha => "recaptcha",
            SituationKind::Hcaptcha => "hcaptcha",
            SituationKind::AwsWaf => "aws_waf",
            SituationKind::ShapeSecurity => "shape_security",
            SituationKind::Kasada => "kasada",
            SituationKind::FriendlyCaptcha => "friendly_captcha",
            SituationKind::GeeTest => "geetest",
            SituationKind::Funcaptcha => "funcaptcha",
            SituationKind::YandexSmartCaptcha => "yandex_smartcaptcha",
            SituationKind::Distil => "distil",
            SituationKind::Reblaze => "reblaze",
            SituationKind::GenericFingerprint => "generic_fingerprint",
            SituationKind::GenericAccessDenied => "generic_access_denied",
            SituationKind::GenericVerify => "generic_verify",
            SituationKind::GenericCdnBlock => "generic_cdn_block",
            SituationKind::RateLimited => "rate_limited",
            SituationKind::GeoBlocked => "geo_blocked",
            SituationKind::LoginWall => "login_wall",
            SituationKind::SoftNotFound => "soft_not_found",
            SituationKind::ServerError => "server_error",
            SituationKind::Unknown => "unknown",
        }
    }

    /// Stable identifier for a generic situation. Used by the ladder to
    /// route escalation decisions.
    pub fn is_anti_bot(self) -> bool {
        matches!(
            self,
            SituationKind::CloudflareIuam
                | SituationKind::CloudflareTurnstile
                | SituationKind::DataDomeCaptcha
                | SituationKind::AkamaiBotManager
                | SituationKind::ImpervaIncapsula
                | SituationKind::PerimeterX
                | SituationKind::Recaptcha
                | SituationKind::Hcaptcha
                | SituationKind::AwsWaf
                | SituationKind::ShapeSecurity
                | SituationKind::Kasada
                | SituationKind::FriendlyCaptcha
                | SituationKind::GeeTest
                | SituationKind::Funcaptcha
                | SituationKind::YandexSmartCaptcha
                | SituationKind::Distil
                | SituationKind::Reblaze
                | SituationKind::GenericFingerprint
                | SituationKind::GenericAccessDenied
                | SituationKind::GenericVerify
                | SituationKind::GenericCdnBlock
        )
    }

    /// Suggest the next ladder step for this situation. The ladder may
    /// override this (e.g. when CDP is not configured) but the suggestion is
    /// the smart default.
    pub fn suggested_ladder(self) -> SuggestedLadder {
        match self {
            SituationKind::CleanSuccess => SuggestedLadder::None,
            SituationKind::JsOnly => SuggestedLadder::Cdp,
            SituationKind::CloudflareIuam => SuggestedLadder::Cdp,
            SituationKind::CloudflareTurnstile => SuggestedLadder::Cdp,
            SituationKind::DataDomeCaptcha => SuggestedLadder::FlareSolverr,
            SituationKind::AkamaiBotManager => SuggestedLadder::Cdp,
            SituationKind::ImpervaIncapsula => SuggestedLadder::Cdp,
            SituationKind::PerimeterX => SuggestedLadder::FlareSolverr,
            SituationKind::Recaptcha => SuggestedLadder::Cdp,
            SituationKind::Hcaptcha => SuggestedLadder::Cdp,
            SituationKind::AwsWaf => SuggestedLadder::Cdp,
            SituationKind::ShapeSecurity => SuggestedLadder::Cdp,
            SituationKind::Kasada => SuggestedLadder::FlareSolverr,
            SituationKind::FriendlyCaptcha => SuggestedLadder::Cdp,
            SituationKind::GeeTest => SuggestedLadder::Cdp,
            SituationKind::Funcaptcha => SuggestedLadder::FlareSolverr,
            SituationKind::YandexSmartCaptcha => SuggestedLadder::Cdp,
            SituationKind::Distil => SuggestedLadder::Cdp,
            SituationKind::Reblaze => SuggestedLadder::Cdp,
            SituationKind::GenericFingerprint => SuggestedLadder::Cdp,
            SituationKind::GenericAccessDenied => SuggestedLadder::Cdp,
            SituationKind::GenericVerify => SuggestedLadder::Cdp,
            SituationKind::GenericCdnBlock => SuggestedLadder::Cdp,
            SituationKind::RateLimited => SuggestedLadder::RetryWithDelay,
            SituationKind::GeoBlocked => SuggestedLadder::None,
            SituationKind::LoginWall => SuggestedLadder::Cdp,
            SituationKind::SoftNotFound => SuggestedLadder::None,
            SituationKind::ServerError => SuggestedLadder::RetryWithDelay,
            SituationKind::Unknown => SuggestedLadder::Cdp,
        }
    }
}

impl fmt::Display for SituationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the ladder should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuggestedLadder {
    /// Stay where we are (success, soft 404, geo-block).
    None,
    /// Escalate to the next level (CDP if available, else FlareSolverr).
    Cdp,
    /// Jump straight to FlareSolverr (skip CDP — it's known to be
    /// ineffective or counterproductive for this provider).
    FlareSolverr,
    /// Wait and retry the same step.
    RetryWithDelay,
}

impl SuggestedLadder {
    pub fn as_str(self) -> &'static str {
        match self {
            SuggestedLadder::None => "none",
            SuggestedLadder::Cdp => "cdp",
            SuggestedLadder::FlareSolverr => "flaresolverr",
            SuggestedLadder::RetryWithDelay => "retry_with_delay",
        }
    }
}

impl fmt::Display for SuggestedLadder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The structured diagnosis: a `kind` plus the evidence that supports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SituationReport {
    /// The diagnosed situation.
    pub kind: SituationKind,
    /// Suggested next ladder step. The ladder may override based on what
    /// fetchers it has configured.
    pub suggested_ladder: SuggestedLadder,
    /// HTTP status code of the response, if any.
    pub status_code: Option<u16>,
    /// Tokens / phrases that triggered the detection. Useful for debugging
    /// and surfaced in the API response so the operator can audit.
    pub evidence: Vec<Evidence>,
    /// Free-form notes from the provider entry.
    pub notes: Option<String>,
}

impl SituationReport {
    /// Construct a `CleanSuccess` report.
    pub fn clean() -> Self {
        Self {
            kind: SituationKind::CleanSuccess,
            suggested_ladder: SuggestedLadder::None,
            status_code: None,
            evidence: Vec::new(),
            notes: None,
        }
    }

    /// True if the ladder should escalate from the HTTP fetcher to a more
    /// expensive step.
    pub fn should_escalate(&self) -> bool {
        !matches!(self.suggested_ladder, SuggestedLadder::None)
    }

    /// True if this is a real anti-bot block (challenge, captcha, or WAF).
    pub fn is_anti_bot(&self) -> bool {
        self.kind.is_anti_bot()
    }
}

impl Default for SituationReport {
    fn default() -> Self {
        Self::clean()
    }
}

/// One specific piece of evidence that contributed to the detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The kind of evidence (token, status code, header).
    pub kind: EvidenceKind,
    /// The actual token / value (e.g. `"cf-mitigated"`, `403`, `"cf-ray"`).
    pub value: String,
    /// The provider name from `providers.toml` that this evidence supports.
    pub provider: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    /// Matched a token from `providers.toml`.
    Token,
    /// Matched a status code.
    StatusCode,
    /// Matched a response header.
    Header,
}

impl fmt::Display for EvidenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvidenceKind::Token => f.write_str("token"),
            EvidenceKind::StatusCode => f.write_str("status_code"),
            EvidenceKind::Header => f.write_str("header"),
        }
    }
}

/// Run the situation detector over an HTML / status-code / headers tuple.
///
/// This is the *replacement* for `challenge_detect::detect_challenge` and
/// `detect_empty_or_blocked`. The legacy functions are now thin wrappers
/// around this.
///
/// **Detection philosophy**: we want to flag *real* anti-bot blocks
/// without producing false positives on every page that happens to be
/// served through Cloudflare's CDN. The compromise:
///
///   1. If the status code is 4xx/5xx, header-based detection is enough
///      — the server is telling us something is wrong.
///   2. If the status code is 2xx, we require BOTH a provider header
///      AND at least one provider-specific token in the body. The
///      combination is what tells us the response is a challenge page
///      and not just an article served by a CDN.
pub fn diagnose(
    html: &str,
    status_code: Option<u16>,
    headers: Option<&[(String, String)]>,
) -> SituationReport {
    // Pre-tokenize the body once.
    let lower = html.to_ascii_lowercase();
    let body_has_tokens = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    // 1. Look at headers first — they're the cheapest signal.
    if let Some(hs) = headers {
        if let Some(ev) = match_header(hs) {
            // Phase C.2: require corroborating body evidence when the
            // status is 2xx. A cf-ray header on a 200 response is just
            // a Cloudflare CDN cache hit; cf-ray + "checking your
            // browser" is a real challenge.
            let status_2xx = matches!(status_code, Some(200..=299));
            if status_2xx {
                let requires = header_body_tokens(ev.provider_name);
                if !body_has_tokens(requires) {
                    // Skip the header-based detection — the body looks
                    // clean. The token-scan step below will catch real
                    // challenges.
                } else {
                    return build_report(
                        ev.kind,
                        ev.suggested_ladder,
                        status_code,
                        vec![Evidence {
                            kind: EvidenceKind::Header,
                            value: ev.header_name.to_string(),
                            provider: ev.provider_name.to_string(),
                        }],
                        Some(ev.notes),
                    );
                }
            } else {
                // Non-2xx: trust the header.
                return build_report(
                    ev.kind,
                    ev.suggested_ladder,
                    status_code,
                    vec![Evidence {
                        kind: EvidenceKind::Header,
                        value: ev.header_name.to_string(),
                        provider: ev.provider_name.to_string(),
                    }],
                    Some(ev.notes),
                );
            }
        }
    }

    // 2. Check status code against the rate-limit / server-error buckets.
    if let Some(code) = status_code {
        match code {
            429 => {
                return build_report(
                    SituationKind::RateLimited,
                    SuggestedLadder::RetryWithDelay,
                    Some(code),
                    vec![Evidence {
                        kind: EvidenceKind::StatusCode,
                        value: code.to_string(),
                        provider: "generic_rate_limit".to_string(),
                    }],
                    Some("Back off and retry."),
                );
            }
            500..=599 => {
                return build_report(
                    SituationKind::ServerError,
                    SuggestedLadder::RetryWithDelay,
                    Some(code),
                    vec![Evidence {
                        kind: EvidenceKind::StatusCode,
                        value: code.to_string(),
                        provider: "server_error".to_string(),
                    }],
                    Some("Server-side issue, not bot detection."),
                );
            }
            _ => {}
        }
    }

    // 3. Token-scan the HTML. Score every provider, pick the highest.
    // Tokens shorter than 4 characters are skipped — they cause too
    // many false positives (`"dd_"`, `"id_"`, `"js"`, etc. match
    // legitimate HTML).
    let mut best: Option<(usize, &ProviderEntry, Vec<Evidence>)> = None;
    for entry in providers::all() {
        let mut hits = 0usize;
        let mut ev = Vec::new();
        for token in &entry.tokens_lower {
            if token.len() < 4 {
                continue;
            }
            if let Some(pos) = lower.find(token) {
                hits += 1;
                ev.push(Evidence {
                    kind: EvidenceKind::Token,
                    value: format!("\"{token}\"@{pos}"),
                    provider: entry.name.to_string(),
                });
                if hits >= 2 {
                    break;
                }
            }
        }
        if hits > 0 && best.as_ref().is_none_or(|(s, _, _)| hits > *s) {
            best = Some((hits, entry, ev));
        }
    }

    if let Some((_, entry, ev)) = best {
        return build_report(
            entry.kind,
            entry.suggested_ladder,
            status_code,
            ev,
            Some(entry.notes),
        );
    }

    // 4. Heuristic: empty / JS-only.
    if detect_empty_or_blocked_html(html) {
        return build_report(
            SituationKind::JsOnly,
            SuggestedLadder::Cdp,
            status_code,
            Vec::new(),
            Some("Page is empty, JS-only, or too small to be useful."),
        );
    }

    // 5. Nothing detected.
    let mut report = SituationReport::clean();
    report.status_code = status_code;
    report
}

/// Body tokens that, when paired with a provider header, confirm a real
/// anti-bot block (vs. a CDN cache hit). The list is intentionally
/// short — we want to be conservative.
fn header_body_tokens(provider: &str) -> &'static [&'static str] {
    match provider {
        "cloudflare_iuam" => &[
            "checking your browser before accessing",
            "cf-mitigated",
            "cf_chl_",
            "challenges.cloudflare.com",
            "cf-chl-bypass",
            "turnstile-v0",
            "ray id",
            "performing security verification",
            "verifying you are human",
            "please verify you are a human",
        ],
        "cloudflare_turnstile" => &[
            "challenges.cloudflare.com/turnstile",
            "cf-turnstile",
            "turnstile.render",
        ],
        "imperva_incapsula" => &[
            "_incapsula_resource",
            "incap_ses_",
            "visid_incap_",
            "pardon our interruption",
            "you have been blocked",
            "request rejected",
        ],
        "akamai_bot_manager" => &[
            "akamai challenge",
            "akamai/abot",
            "akamai-bm",
            "pm_akamai",
            "x-amz-rid",
            "ghostbox",
            "akamai-bm-sc",
        ],
        "datadome_captcha" => &[
            "ddc-captcha",
            "datadome.co",
            "geo.captcha-delivery",
            "captcha-delivery.com",
            "datadome-captcha",
            "datadome-protected",
        ],
        "kasada" => &["kasada", "kpsdk", "kasada-akamai"],
        "perimeterx" => &[
            "perimeterx",
            "px-captcha",
            "px-app",
            "_pxappid",
            "pxcaptcha",
            "humansecurity",
        ],
        "aws_waf" => &[
            "aws-waf",
            "x-amz-waf",
            "request rejected",
            "this request was blocked",
        ],
        _ => &[
            "captcha",
            "challenge",
            "checking your browser",
            "access denied",
        ],
    }
}

fn build_report(
    kind: SituationKind,
    suggested: SuggestedLadder,
    status_code: Option<u16>,
    evidence: Vec<Evidence>,
    notes: Option<&'static str>,
) -> SituationReport {
    SituationReport {
        kind,
        suggested_ladder: suggested,
        status_code,
        evidence,
        notes: notes.map(String::from),
    }
}

/// Re-exported for tests. Loaded lazily from `providers.toml` at first use.
/// The file is parsed once and cached in a `static` via `Lazy`.
pub(crate) mod providers {
    use super::*;
    use std::sync::OnceLock;

    /// One row from `providers.toml`.
    #[derive(Debug, Clone)]
    pub(super) struct ProviderEntry {
        pub name: &'static str,
        pub kind: SituationKind,
        pub suggested_ladder: SuggestedLadder,
        pub tokens_lower: Vec<&'static str>,
        pub notes: &'static str,
    }

    static CACHE: OnceLock<Vec<ProviderEntry>> = OnceLock::new();

    pub(super) fn all() -> &'static [ProviderEntry] {
        CACHE.get_or_init(load_from_toml)
    }

    fn load_from_toml() -> Vec<ProviderEntry> {
        // The TOML is embedded at compile time. This avoids a runtime
        // dependency on the file being on disk — important for
        // `cargo install --path` users and for the Docker image.
        const TOML_STR: &str = include_str!("providers.toml");
        match parse_providers(TOML_STR) {
            Ok(v) => v,
            Err(e) => {
                // The TOML is in-tree and must parse. If it doesn't, that's
                // a build-time bug, so we panic loudly. We never want to
                // silently fall back to "no providers detected".
                panic!("providers.toml failed to parse: {e}");
            }
        }
    }

    fn parse_providers(s: &str) -> std::result::Result<Vec<ProviderEntry>, String> {
        let root: toml::Value = toml::from_str(s).map_err(|e| e.to_string())?;
        let table = root
            .as_table()
            .ok_or_else(|| "root is not a table".to_string())?;
        let providers = table
            .get("providers")
            .and_then(|v| v.as_table())
            .ok_or_else(|| "missing [providers] table".to_string())?;

        let mut out = Vec::with_capacity(providers.len());
        for (name, value) in providers {
            let entry_table = value
                .as_table()
                .ok_or_else(|| format!("provider {name} is not a table"))?;
            // The TOML key (`cloudflare_iuam`, `datadome_captcha`, ...) maps
            // directly to a `SituationKind` variant. The `kind` field is a
            // coarser category kept for operator documentation.
            let kind = situation_kind_from_toml_key(name).ok_or_else(|| {
                format!(
                    "provider {name} has no matching SituationKind — \
                     add a mapping in `situation_kind_from_toml_key`"
                )
            })?;
            let suggested_ladder = entry_table
                .get("suggested_ladder")
                .and_then(|v| v.as_str())
                .map(ladder_from_str)
                .transpose()?
                .unwrap_or(SuggestedLadder::Cdp);
            let notes = entry_table
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tokens: Vec<String> = entry_table
                .get("tokens")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let tokens_lower: Vec<&'static str> = tokens
                .into_iter()
                .map(|t| t.to_ascii_lowercase())
                .map(|t| Box::leak(t.into_boxed_str()) as &'static str)
                .collect();
            let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
            let notes_static: &'static str = Box::leak(notes.to_string().into_boxed_str());
            out.push(ProviderEntry {
                name: name_static,
                kind,
                suggested_ladder,
                tokens_lower,
                notes: notes_static,
            });
        }
        Ok(out)
    }

    /// Map a TOML provider key to a `SituationKind` variant. Returns `None`
    /// if the TOML has a key that doesn't match any variant — that should
    /// be a build-time error.
    fn situation_kind_from_toml_key(key: &str) -> Option<SituationKind> {
        Some(match key {
            "cloudflare_iuam" => SituationKind::CloudflareIuam,
            "cloudflare_turnstile" => SituationKind::CloudflareTurnstile,
            "datadome_captcha" => SituationKind::DataDomeCaptcha,
            "akamai_bot_manager" => SituationKind::AkamaiBotManager,
            "imperva_incapsula" => SituationKind::ImpervaIncapsula,
            "perimeterx" => SituationKind::PerimeterX,
            "recaptcha" => SituationKind::Recaptcha,
            "hcaptcha" => SituationKind::Hcaptcha,
            "aws_waf" => SituationKind::AwsWaf,
            "shape_security" => SituationKind::ShapeSecurity,
            "kasada" => SituationKind::Kasada,
            "friendly_captcha" => SituationKind::FriendlyCaptcha,
            "geetest" => SituationKind::GeeTest,
            "funcaptcha" => SituationKind::Funcaptcha,
            "yandex_smartcaptcha" => SituationKind::YandexSmartCaptcha,
            "distil" => SituationKind::Distil,
            "reblaze" => SituationKind::Reblaze,
            "generic_fingerprint" => SituationKind::GenericFingerprint,
            "generic_access_denied" => SituationKind::GenericAccessDenied,
            "generic_verify" => SituationKind::GenericVerify,
            "generic_cdn_block" => SituationKind::GenericCdnBlock,
            "generic_rate_limit" => SituationKind::RateLimited,
            "geo_block" => SituationKind::GeoBlocked,
            "login_wall" => SituationKind::LoginWall,
            "soft_404" => SituationKind::SoftNotFound,
            "server_error" => SituationKind::ServerError,
            "js_only" => SituationKind::JsOnly,
            _ => return None,
        })
    }

    fn ladder_from_str(s: &str) -> std::result::Result<SuggestedLadder, String> {
        Ok(match s {
            "none" => SuggestedLadder::None,
            "cdp" => SuggestedLadder::Cdp,
            "flaresolverr" => SuggestedLadder::FlareSolverr,
            "retry-with-delay" => SuggestedLadder::RetryWithDelay,
            other => return Err(format!("unknown suggested_ladder {other:?}")),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct HeaderHit {
    header_name: &'static str,
    provider_name: &'static str,
    kind: SituationKind,
    suggested_ladder: SuggestedLadder,
    notes: &'static str,
}

fn match_header(headers: &[(String, String)]) -> Option<HeaderHit> {
    for (name, _value) in headers {
        let n = name.to_ascii_lowercase();
        if n == "cf-ray" || n == "cf-mitigated" {
            return Some(HeaderHit {
                header_name: "cf-ray",
                provider_name: "cloudflare_iuam",
                kind: SituationKind::CloudflareIuam,
                suggested_ladder: SuggestedLadder::Cdp,
                notes: "Cloudflare Ray ID present.",
            });
        }
        if n == "x-iinfo" || n == "incap-ses" {
            return Some(HeaderHit {
                header_name: "x-iinfo",
                provider_name: "imperva_incapsula",
                kind: SituationKind::ImpervaIncapsula,
                suggested_ladder: SuggestedLadder::Cdp,
                notes: "Imperva / Incapsula headers present.",
            });
        }
        if n == "x-akamai-transformed" || n.starts_with("x-akamai-") || n == "akamai-grn" {
            return Some(HeaderHit {
                header_name: "x-akamai-transformed",
                provider_name: "akamai_bot_manager",
                kind: SituationKind::AkamaiBotManager,
                suggested_ladder: SuggestedLadder::Cdp,
                notes: "Akamai headers present.",
            });
        }
        if n == "x-dd-b" || n == "x-datadome" {
            return Some(HeaderHit {
                header_name: "x-dd-b",
                provider_name: "datadome_captcha",
                kind: SituationKind::DataDomeCaptcha,
                suggested_ladder: SuggestedLadder::FlareSolverr,
                notes: "DataDome headers present.",
            });
        }
        if n.starts_with("x-kpsdk-") {
            return Some(HeaderHit {
                header_name: "x-kpsdk-ct",
                provider_name: "kasada",
                kind: SituationKind::Kasada,
                suggested_ladder: SuggestedLadder::FlareSolverr,
                notes: "Kasada SDK headers present.",
            });
        }
        if n.starts_with("x-px-") {
            return Some(HeaderHit {
                header_name: "x-px-cookie",
                provider_name: "perimeterx",
                kind: SituationKind::PerimeterX,
                suggested_ladder: SuggestedLadder::FlareSolverr,
                notes: "PerimeterX headers present.",
            });
        }
        if n == "x-amzn-requestid" || n.starts_with("x-amz-waf-") {
            return Some(HeaderHit {
                header_name: "x-amzn-requestid",
                provider_name: "aws_waf",
                kind: SituationKind::AwsWaf,
                suggested_ladder: SuggestedLadder::Cdp,
                notes: "AWS WAF headers present.",
            });
        }
    }
    None
}

/// Heuristic: HTML is empty / JS-only / anti-bot landing page.
pub fn detect_empty_or_blocked_html(html: &str) -> bool {
    let trimmed = html.trim();
    if trimmed.len() < 200 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Body visible-chars heuristic for SPA shells.
    let has_doctype = lower.starts_with("<!doctype") || lower.starts_with("<html");
    let body_tag_open = lower.find("<body");
    if let (true, Some(start)) = (has_doctype, body_tag_open) {
        let after = &lower[start..];
        if let Some(close_offset) = after.find("</body>") {
            let body_inner = &after[..close_offset];
            let tag_count = body_inner.matches('<').count();
            let stripped: String = body_inner
                .split('<')
                .filter_map(|s| s.find('>').map(|i| &s[i + 1..]))
                .collect();
            let visible_chars: usize = stripped.chars().filter(|c| !c.is_whitespace()).count();
            if visible_chars < 100 && tag_count > 5 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_html_diagnoses_clean_success() {
        let html = r#"<!DOCTYPE html><html><head><title>Hi</title></head>
        <body><main><h1>Hello</h1><p>Real content with enough text to look like a real page
        and not a bot challenge or empty SPA shell. We need at least a few hundred characters
        of visible text to confidently say this is a real page, not a JS-only stub.</p>
        <p>More text here. Even more text. We keep going. The quick brown fox jumps over the
        lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox.</p>
        </main></body></html>"#;
        let report = diagnose(html, Some(200), None);
        assert_eq!(report.kind, SituationKind::CleanSuccess);
        assert_eq!(report.suggested_ladder, SuggestedLadder::None);
        assert!(!report.should_escalate());
    }

    #[test]
    fn cloudflare_iuam_diagnosed() {
        let html = r#"<!DOCTYPE html><html><head>
        <title>Just a moment...</title>
        <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
        </head><body>cf-mitigated Ray ID: abc-def-ghi</body></html>"#;
        let report = diagnose(html, Some(403), None);
        // Cloudflare IUAM and Turnstile share a lot of tokens. The detector
        // should pick one — either is fine, but it MUST be a Cloudflare
        // variant and MUST suggest CDP.
        assert!(matches!(
            report.kind,
            SituationKind::CloudflareIuam | SituationKind::CloudflareTurnstile
        ));
        assert_eq!(report.suggested_ladder, SuggestedLadder::Cdp);
        assert!(!report.evidence.is_empty());
    }

    #[test]
    fn datadome_diagnosed() {
        let html = r#"<html><body><div class="ddc-captcha">
        <script src="https://geo.captcha-delivery.com/captcha.js"></script>
        </div></body></html>"#;
        let report = diagnose(html, Some(403), None);
        assert_eq!(report.kind, SituationKind::DataDomeCaptcha);
        assert_eq!(report.suggested_ladder, SuggestedLadder::FlareSolverr);
    }

    #[test]
    fn rate_limit_diagnosed_from_status() {
        let report = diagnose("<html>ok</html>", Some(429), None);
        assert_eq!(report.kind, SituationKind::RateLimited);
        assert_eq!(report.suggested_ladder, SuggestedLadder::RetryWithDelay);
    }

    #[test]
    fn geo_block_diagnosed() {
        let html =
            "<html><body>Sorry, this content is not available in your country.</body></html>";
        // Pad to >= 200 chars to defeat the small-payload heuristic.
        let padded = format!("{html}{}", "x".repeat(300));
        let report = diagnose(&padded, Some(403), None);
        assert_eq!(report.kind, SituationKind::GeoBlocked);
        assert_eq!(report.suggested_ladder, SuggestedLadder::None);
    }

    #[test]
    fn js_only_diagnosed_from_spa_shell() {
        let html = r#"<!DOCTYPE html>
<html><head><title>App</title></head><body>
<script src="/_next/static/chunks/main.js"></script>
<script src="/_next/static/chunks/app.js"></script>
<script src="/_next/static/chunks/framework.js"></script>
<script src="/_next/static/chunks/webpack.js"></script>
<script src="/_next/static/chunks/pages/_app.js"></script>
<script src="/_next/static/chunks/pages/index.js"></script>
</body></html>"#;
        let report = diagnose(html, Some(200), None);
        // Could be JsOnly (heuristic) or GenericFingerprint if a token hit
        // first. Either is fine — both should suggest CDP.
        assert!(matches!(
            report.kind,
            SituationKind::JsOnly | SituationKind::GenericFingerprint
        ));
        assert_eq!(report.suggested_ladder, SuggestedLadder::Cdp);
    }

    #[test]
    fn akamai_diagnosed_from_header() {
        let headers = vec![("x-akamai-transformed".to_string(), "9 9 9".to_string())];
        let report = diagnose("<html>ok</html>", Some(403), Some(&headers));
        assert_eq!(report.kind, SituationKind::AkamaiBotManager);
        assert_eq!(report.suggested_ladder, SuggestedLadder::Cdp);
        assert!(report
            .evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::Header));
    }

    #[test]
    fn server_error_diagnosed() {
        let report = diagnose("<html>Internal Server Error</html>", Some(500), None);
        assert_eq!(report.kind, SituationKind::ServerError);
        assert_eq!(report.suggested_ladder, SuggestedLadder::RetryWithDelay);
    }

    #[test]
    fn kasada_via_header() {
        let headers = vec![("x-kpsdk-ct".to_string(), "abc".to_string())];
        let report = diagnose("<html>ok</html>", Some(403), Some(&headers));
        assert_eq!(report.kind, SituationKind::Kasada);
        assert_eq!(report.suggested_ladder, SuggestedLadder::FlareSolverr);
    }

    #[test]
    fn soft_404_404_status() {
        // Soft 404 isn't a token-based detection in this version, but a
        // 404 with "page not found" content should be classified as a real
        // 404-like situation. The detector still runs token scan first.
        let html = "<html><body>page not found</body></html>".to_string();
        // Need to pad to defeat the small-payload heuristic.
        let padded = format!("{html}{}", "x".repeat(300));
        let report = diagnose(&padded, Some(404), None);
        // The token "page not found" comes from `soft_404` provider. The
        // detector should pick it and suggest None.
        assert_eq!(report.kind, SituationKind::SoftNotFound);
        assert_eq!(report.suggested_ladder, SuggestedLadder::None);
    }

    #[test]
    fn is_anti_bot_predicate() {
        assert!(SituationKind::CloudflareIuam.is_anti_bot());
        assert!(SituationKind::DataDomeCaptcha.is_anti_bot());
        assert!(!SituationKind::CleanSuccess.is_anti_bot());
        assert!(!SituationKind::JsOnly.is_anti_bot());
        assert!(!SituationKind::SoftNotFound.is_anti_bot());
        assert!(!SituationKind::GeoBlocked.is_anti_bot());
    }

    #[test]
    fn evidence_kind_serializes_to_string() {
        let e = Evidence {
            kind: EvidenceKind::Token,
            value: "cf-mitigated".into(),
            provider: "cloudflare_iuam".to_string(),
        };
        let json = serde_json::to_string(&e).unwrap();
        // Default serde derive serializes unit variants as bare strings.
        assert!(json.contains("\"Token\""), "got: {json}");
    }
}
