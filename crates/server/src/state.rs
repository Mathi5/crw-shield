use std::sync::{Arc, Mutex};

use crw_antibot::DelayPreset;
use crw_core::Config;
use crw_fetch::{Fetcher, HttpFetcher};

use crate::handlers::CrawlJob;

/// Concrete fetcher type the server uses. Aliasing it here keeps the rest of
/// the crate decoupled from the concrete HTTP/reqwest types.
pub type FetcherType = dyn Fetcher + Send + Sync;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub fetcher: Arc<FetcherType>,
    pub delay_preset: DelayPreset,
    pub jobs: Arc<Mutex<std::collections::HashMap<String, CrawlJob>>>,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        let preset = DelayPreset::from_str(&config.scrape_delay_preset);
        let http = HttpFetcher::new(60_000, config.stealth_enabled, preset)
            .expect("failed to build HttpFetcher");
        let fetcher: Arc<FetcherType> = Arc::new(http);
        Self {
            config: Arc::new(config),
            fetcher,
            delay_preset: preset,
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}
