//! SearXNG proxy.
//!
//! Wraps a SearXNG instance and maps its JSON response to the Firecrawl
//! `SearchResponse` shape used by the rest of the crate. When `SEARXNG_URL` is
//! not configured the client returns a well-formed empty response with a
//! warning instead of erroring out — that way the API stays up in dev.

use crw_core::{
    CrwError, Result, SearchData, SearchRequest, SearchResponse, SearchResultItem, SearchSource,
};
use serde::Deserialize;

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

    /// Build the SearXNG `/search` URL for a request.
    fn build_url(&self, req: &SearchRequest) -> Result<String> {
        let mut base = url::Url::parse(&self.base_url)
            .map_err(|e| CrwError::Config(format!("invalid SEARXNG_URL: {e}")))?;
        {
            let mut segments = base
                .path_segments_mut()
                .map_err(|_| CrwError::Config("SEARXNG_URL is not a valid base URL".to_string()))?;
            segments.pop_if_empty();
            segments.push("search");
        }
        let wants_news = req.sources.iter().any(|s| matches!(s, SearchSource::News));
        let wants_images = req
            .sources
            .iter()
            .any(|s| matches!(s, SearchSource::Images));
        let categories = if wants_news {
            "news"
        } else if wants_images {
            "images"
        } else {
            "general"
        };
        {
            let mut q = base.query_pairs_mut();
            q.append_pair("q", &req.query);
            q.append_pair("format", "json");
            q.append_pair("categories", categories);
            if req.limit > 0 {
                q.append_pair("count", &req.limit.to_string());
            }
            if let Some(tbs) = req.tbs.as_deref() {
                q.append_pair("time_range", tbs);
            }
        }
        Ok(base.to_string())
    }

    /// Run a search against SearXNG and map the response.
    pub async fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        if self.base_url.is_empty() {
            return Ok(self.empty_response(req));
        }
        let url = self.build_url(req)?;

        let client = reqwest::Client::builder()
            .user_agent("crw-shield/0.1 (+search)")
            .build()
            .map_err(|e| CrwError::Fetch(format!("searxng client build: {e}")))?;

        let mut request = client.get(&url).header("Accept", "application/json");
        if let Some(token) = self.token.as_deref() {
            if !token.is_empty() {
                request = request.bearer_auth(token);
            }
        }

        let response = request
            .send()
            .await
            .map_err(|e| CrwError::Fetch(format!("searxng request: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| CrwError::Fetch(format!("searxng body: {e}")))?;
        if !status.is_success() {
            return Err(CrwError::Http {
                status: status.as_u16(),
                message: format!("searxng returned HTTP {status}: {body}"),
            });
        }

        let parsed: SearxngResponse = serde_json::from_str(&body).map_err(|e| {
            CrwError::Internal(format!("searxng returned invalid JSON: {e}; body: {body}"))
        })?;

        Ok(map_response(req, parsed))
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

// ---------------------------------------------------------------------------------------
// SearXNG JSON model — only the fields we care about are decoded.
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
    #[serde(default)]
    answers: Vec<serde_json::Value>,
    #[serde(default)]
    infoboxes: Vec<serde_json::Value>,
    #[serde(default)]
    suggestions: Vec<String>,
    #[serde(default)]
    number_of_results: Option<u64>,
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SearxngResult {
    url: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    snippet: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    engine: Option<String>,
    #[serde(default)]
    img_src: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    thumbnail_src: Option<String>,
    #[serde(default, rename = "publishedDate")]
    #[allow(dead_code)]
    published_date: Option<String>,
}

fn pick_description(r: &SearxngResult) -> Option<String> {
    r.snippet
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            r.content
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

fn map_response(req: &SearchRequest, parsed: SearxngResponse) -> SearchResponse {
    let wants_news = req.sources.iter().any(|s| matches!(s, SearchSource::News));
    let wants_images = req
        .sources
        .iter()
        .any(|s| matches!(s, SearchSource::Images));

    let mut web = Vec::new();
    let mut images = Vec::new();
    let mut news = Vec::new();

    for r in parsed.results {
        let category = r
            .category
            .as_deref()
            .unwrap_or("general")
            .to_ascii_lowercase();
        let description = pick_description(&r);

        if wants_images || category == "images" {
            let image_url = r.img_src.clone().unwrap_or_else(|| r.url.clone());
            images.push(SearchResultItem {
                title: r.title.clone().filter(|s| !s.is_empty()),
                description: description.clone(),
                url: image_url,
                markdown: None,
            });
            continue;
        }

        if wants_news || category == "news" {
            news.push(SearchResultItem {
                title: r.title.clone().filter(|s| !s.is_empty()),
                description: description.clone(),
                url: r.url.clone(),
                markdown: None,
            });
            continue;
        }

        web.push(SearchResultItem {
            title: r.title.clone().filter(|s| !s.is_empty()),
            description,
            url: r.url.clone(),
            markdown: None,
        });
    }

    if !parsed.infoboxes.is_empty() || !parsed.answers.is_empty() {
        let mut summary = String::new();
        for answer in &parsed.answers {
            if let Some(s) = answer.as_str() {
                summary.push_str(s);
                summary.push('\n');
            }
        }
        for info in &parsed.infoboxes {
            if let Some(content) = info.get("content").and_then(|v| v.as_str()) {
                summary.push_str(content);
                summary.push('\n');
            }
        }
        if let Some(first) = web.first_mut() {
            let combined = match first.description.as_deref() {
                Some(existing) => format!("{existing}\n{summary}"),
                None => summary,
            };
            first.description = Some(combined);
        }
    }

    let _ = parsed.number_of_results;
    let _ = parsed.suggestions;
    let _ = parsed.query;

    SearchResponse {
        success: true,
        data: SearchData { web, images, news },
        warning: None,
        id: None,
        credits_used: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req() -> SearchRequest {
        SearchRequest {
            query: "x".into(),
            limit: 5,
            sources: vec![crw_core::SearchSource::Web],
            include_domains: vec![],
            exclude_domains: vec![],
            tbs: None,
            timeout: 60_000,
            ignore_invalid_urls: false,
            scrape_options: None,
        }
    }

    #[tokio::test]
    async fn search_empty_url_returns_warning_response() {
        let client = SearchClient::new("", None);
        let r = client.search(&req()).await.unwrap();
        assert!(r.warning.is_some());
    }

    #[test]
    fn build_url_encodes_query() {
        let client = SearchClient::new("http://localhost:8888/", None);
        let url = client.build_url(&req()).unwrap();
        assert!(url.contains("/search"));
        assert!(url.contains("q=x"));
        assert!(url.contains("format=json"));
    }

    #[test]
    fn map_response_routes_web_results() {
        let parsed = SearxngResponse {
            results: vec![SearxngResult {
                url: "https://example.com".into(),
                title: Some("Title".into()),
                content: Some("snippet".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let resp = map_response(&req(), parsed);
        assert_eq!(resp.data.web.len(), 1);
        assert_eq!(resp.data.images.len(), 0);
        assert_eq!(resp.data.news.len(), 0);
        assert_eq!(resp.data.web[0].title.as_deref(), Some("Title"));
        assert_eq!(resp.data.web[0].url, "https://example.com");
    }

    #[test]
    fn map_response_routes_news_results() {
        let parsed = SearxngResponse {
            results: vec![SearxngResult {
                url: "https://example.com/n".into(),
                title: Some("News".into()),
                category: Some("news".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut r = req();
        r.sources = vec![SearchSource::News];
        let resp = map_response(&r, parsed);
        assert_eq!(resp.data.news.len(), 1);
        assert_eq!(resp.data.web.len(), 0);
    }

    #[test]
    fn map_response_routes_image_results() {
        let parsed = SearxngResponse {
            results: vec![SearxngResult {
                url: "https://example.com/p".into(),
                title: Some("Img".into()),
                img_src: Some("https://example.com/p.jpg".into()),
                category: Some("images".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut r = req();
        r.sources = vec![SearchSource::Images];
        let resp = map_response(&r, parsed);
        assert_eq!(resp.data.images.len(), 1);
        assert_eq!(resp.data.images[0].url, "https://example.com/p.jpg");
    }

    #[test]
    fn empty_response_has_warning() {
        let client = SearchClient::new("http://localhost", None);
        let r = client.empty_response(&req());
        assert!(r.success);
        assert!(r.warning.is_some());
    }

    #[tokio::test]
    async fn search_sends_request_to_searxng() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("q".into(), "rust".into()),
                mockito::Matcher::UrlEncoded("format".into(), "json".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({
                    "results": [
                        {"url": "https://rust-lang.org", "title": "Rust", "content": "Lang"}
                    ],
                    "number_of_results": 1
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = SearchClient::new(server.url(), None);
        let mut r = req();
        r.query = "rust".into();
        let resp = client.search(&r).await.unwrap();
        assert_eq!(resp.data.web.len(), 1);
        assert_eq!(resp.data.web[0].url, "https://rust-lang.org");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn search_uses_bearer_token() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::Any)
            .match_header("authorization", "Bearer abc")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({"results": []}).to_string())
            .create_async()
            .await;

        let client = SearchClient::new(server.url(), Some("abc".into()));
        let resp = client.search(&req()).await.unwrap();
        assert_eq!(resp.data.web.len(), 0);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn search_returns_http_error_on_non_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/search")
            .match_query(mockito::Matcher::Any)
            .with_status(502)
            .with_body("bad gateway")
            .create_async()
            .await;

        let client = SearchClient::new(server.url(), None);
        let err = client.search(&req()).await.unwrap_err();
        match err {
            CrwError::Http { status, .. } => assert_eq!(status, 502),
            other => panic!("expected Http error, got {other:?}"),
        }
        mock.assert_async().await;
    }
}
