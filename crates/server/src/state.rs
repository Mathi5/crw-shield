use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crw_antibot::{DelayPreset, HostCounters};
use crw_core::Config;
use crw_fetch::{CdpFetcher, FetchLadder, FlareSolverrClient, HttpFetcher};

use crate::handlers::CrawlJob;

/// Concrete fetcher type the server uses. Aliasing it here keeps the rest of
/// the crate decoupled from the concrete HTTP/reqwest types.
pub type FetchLadderType = FetchLadder;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub ladder: Arc<FetchLadder>,
    pub delay_preset: DelayPreset,
    pub jobs: Arc<Mutex<std::collections::HashMap<String, CrawlJob>>>,
    /// Per-host rotation bookkeeping shared across all requests.
    /// `Arc<Mutex<…>>` is `Clone`-friendly so this state survives `AppState`
    /// clones (each request gets the same backing map).
    pub host_counters: HostCounters,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        let preset = DelayPreset::from_str(&config.scrape_delay_preset);
        let http = Arc::new(
            HttpFetcher::new(60_000, config.stealth_enabled, preset)
                .expect("failed to build HttpFetcher"),
        );
        let cdp = if config.cdp_enabled {
            Some(Arc::new(CdpFetcher::with_default()))
        } else {
            None
        };
        let flaresolverr = match config.flaresolverr_url.as_deref() {
            Some(url) if !url.is_empty() => match FlareSolverrClient::new(url) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!(error=%e, "failed to build FlareSolverrClient; disabling");
                    None
                }
            },
            _ => None,
        };
        let ladder = Arc::new(FetchLadder::new(http, cdp, flaresolverr));
        Self {
            config: Arc::new(config),
            ladder,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            host_counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
