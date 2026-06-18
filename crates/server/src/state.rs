use std::sync::Arc;

use crw_antibot::DelayPreset;
use crw_core::Config;
use crw_fetch::HttpFetcher;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub fetcher: Arc<HttpFetcher>,
    pub delay_preset: DelayPreset,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        let preset = DelayPreset::from_str(&config.scrape_delay_preset);
        let fetcher = HttpFetcher::new(60_000, config.stealth_enabled, preset)
            .expect("failed to build HttpFetcher");
        Self {
            config: Arc::new(config),
            fetcher: Arc::new(fetcher),
            delay_preset: preset,
        }
    }
}
