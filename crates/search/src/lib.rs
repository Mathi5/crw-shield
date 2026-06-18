//! SearXNG proxy.
//!
//! TODO: Phase 2 — wire SearXNG client, optionally scrape each result.

use crw_core::{CrwError, Result, SearchData, SearchRequest, SearchResponse};

pub struct SearchClient {
    pub base_url: String,
    pub token: Option<String>,
}

impl SearchClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token,
        }
    }

    pub async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse> {
        Err(CrwError::NotImplemented("search — Phase 2".to_string()))
    }

    pub fn empty_response(&self, req: &SearchRequest) -> SearchResponse {
        let _ = req;
        SearchResponse {
            success: true,
            data: SearchData::default(),
            warning: Some("SEARXNG_URL not configured".to_string()),
            id: None,
            credits_used: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_not_implemented() {
        let client = SearchClient::new("http://localhost", None);
        let req = SearchRequest {
            query: "x".into(),
            limit: 5,
            sources: vec![crw_core::SearchSource::Web],
            include_domains: vec![],
            exclude_domains: vec![],
            tbs: None,
            timeout: 60_000,
            ignore_invalid_urls: false,
            scrape_options: None,
        };
        let r = client.search(&req).await;
        assert!(matches!(r, Err(CrwError::NotImplemented(_))));
    }

    #[test]
    fn empty_response_has_warning() {
        let client = SearchClient::new("http://localhost", None);
        let req = SearchRequest {
            query: "x".into(),
            limit: 5,
            sources: vec![crw_core::SearchSource::Web],
            include_domains: vec![],
            exclude_domains: vec![],
            tbs: None,
            timeout: 60_000,
            ignore_invalid_urls: false,
            scrape_options: None,
        };
        let r = client.empty_response(&req);
        assert!(r.success);
        assert!(r.warning.is_some());
    }
}
