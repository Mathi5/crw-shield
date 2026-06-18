pub mod cdp;
pub mod http;
pub mod ladder;

pub use cdp::{chrome_available, CdpConfig, CdpFetchResult, CdpFetcher};
pub use http::{FetchResult, Fetcher, HttpFetcher};
pub use ladder::{
    metadata_from_fetch, scrape_data_from_ladder, scrape_via_ladder, FetchLadder, FetchSource,
    LadderResult,
};
