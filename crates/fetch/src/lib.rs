pub mod cdp;
pub mod flaresolverr;
pub mod headed_hitl;
pub mod http;
pub mod ladder;
#[cfg(feature = "tls-fingerprint")]
pub mod tls_profile;
pub mod tls_proxy;

pub use cdp::{chrome_available, CdpConfig, CdpFetchResult, CdpFetcher};
pub use flaresolverr::{CookieInfo, FlareSolverrAllowlist, FlareSolverrClient, FlareSolverrResult};
pub use headed_hitl::{
    cookie_name_is_session_like, inject_cdp_cookies_into_jar, new_session_registry,
    HeadedHitlSession, SessionRegistry,
};
pub use http::{FetchResult, Fetcher, HttpFetcher};
pub use ladder::{
    metadata_from_fetch, scrape_data_from_ladder, scrape_via_ladder, FetchLadder, FetchSource,
    LadderResult,
};
#[cfg(feature = "tls-fingerprint")]
pub use tls_profile::{
    build_wreq_client, pick_emulation_for_profile, pick_emulation_for_profile_or_env,
    BrowserEmulation,
};
pub use tls_proxy::{TlsProxy, TlsProxyConfig};
