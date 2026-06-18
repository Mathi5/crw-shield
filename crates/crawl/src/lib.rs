//! BFS crawl engine.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crw_core::{CrawlRequest, CrawlStatus, CrwError, Format, Result, ScrapeData, ScrapeRequest};
use crw_extract::{
    extract_links, extract_main_content, extract_metadata, filter_tags, html_to_markdown,
};
use crw_fetch::{FetchResult, Fetcher};
use futures::stream::{FuturesUnordered, StreamExt};
use tracing::{debug, warn};
use url::Url;

pub struct CrawlEngine;

impl CrawlEngine {
    pub fn new() -> Self {
        Self
    }

    /// Returns `true` if `url` should be enqueued given a list of include /
    /// exclude glob-style patterns (case-sensitive on the path).
    pub fn should_visit(url: &Url, include: &[String], exclude: &[String]) -> bool {
        let path = url.path().trim_start_matches('/');
        for pat in exclude {
            if glob_match(pat, path) {
                return false;
            }
        }
        if include.is_empty() {
            return true;
        }
        include.iter().any(|pat| glob_match(pat, path))
    }

    /// Returns `true` if the URL belongs to the seed host (optionally also
    /// subdomains) and respects the `allow_external_links` / `allow_subdomains`
    /// knobs.
    pub fn is_in_scope(
        seed: &Url,
        candidate: &Url,
        allow_subdomains: bool,
        allow_external: bool,
    ) -> bool {
        if allow_external {
            return true;
        }
        match (seed.host_str(), candidate.host_str()) {
            (Some(s), Some(c)) => {
                if s == c {
                    return true;
                }
                if allow_subdomains && c.ends_with(&format!(".{s}")) {
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}

impl Default for CrawlEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let mut re = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' => re.push('.'),
            '(' | ')' | '[' | ']' | '{' | '}' | '+' | '|' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(ch);
            }
            c => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
        .map(|r| r.is_match(text))
        .unwrap_or(false)
}

/// Lightweight abstraction so the caller can plug in either the real HTTP
/// fetcher or a mock. Mirrors the `Fetcher` trait but returns a full
/// `ScrapeData` after running the standard extraction pipeline.
#[async_trait]
pub trait ScrapeRunner: Send + Sync {
    async fn scrape(&self, request: &ScrapeRequest) -> Result<ScrapeData>;
}

/// Adapter that turns any `Fetcher` (HTTP-only) into a `ScrapeRunner` applying
/// the same content pipeline the server uses for `/v2/scrape`.
pub struct FetcherScrapeRunner<F: Fetcher + ?Sized> {
    pub fetcher: Arc<F>,
}

#[async_trait]
impl<F: Fetcher + ?Sized> ScrapeRunner for FetcherScrapeRunner<F> {
    async fn scrape(&self, request: &ScrapeRequest) -> Result<ScrapeData> {
        let fetch: FetchResult = self.fetcher.fetch(request).await?;
        scrape_data_from_fetch(&fetch, request)
    }
}

/// Build a `ScrapeData` from a `FetchResult` honoring the standard
/// content-extraction pipeline (main content, tag filtering, markdown).
pub fn scrape_data_from_fetch(fetch: &FetchResult, req: &ScrapeRequest) -> Result<ScrapeData> {
    let formats: HashSet<Format> = req.formats.iter().copied().collect();
    let wants = |f: Format| formats.contains(&f);

    let mut html_for_extraction = fetch.html.clone();
    if req.only_main_content {
        html_for_extraction = extract_main_content(&html_for_extraction);
    }
    if !req.include_tags.is_empty() || !req.exclude_tags.is_empty() {
        html_for_extraction =
            filter_tags(&html_for_extraction, &req.include_tags, &req.exclude_tags);
    }

    let metadata = extract_metadata(&fetch.html, &fetch.final_url, fetch.status_code);

    let markdown = if wants(Format::Markdown) {
        Some(html_to_markdown(&html_for_extraction))
    } else {
        None
    };
    let html_out = if wants(Format::Html) {
        Some(html_for_extraction)
    } else {
        None
    };
    let raw_html = if wants(Format::RawHtml) {
        Some(fetch.html.clone())
    } else {
        None
    };
    let links = if wants(Format::Links) {
        Some(extract_links(&fetch.html, &fetch.final_url))
    } else {
        None
    };
    if wants(Format::Screenshot) {
        return Err(CrwError::NotImplemented(
            "screenshot is not available without CDP".into(),
        ));
    }

    Ok(ScrapeData {
        markdown,
        html: html_out,
        raw_html,
        links,
        screenshot: None,
        metadata,
    })
}

/// Normalize a URL for de-duplication: drop fragments and, optionally,
/// query parameters.
pub fn normalize_url(url: &Url, ignore_query_parameters: bool) -> String {
    let mut cloned = url.clone();
    cloned.set_fragment(None);
    if ignore_query_parameters {
        cloned.set_query(None);
    }
    cloned.to_string()
}

/// Run a BFS crawl driven by `runner`. Stops when the queue is exhausted or
/// the per-crawl `limit` is reached. Returns the list of scraped pages in BFS
/// order.
pub async fn crw_crawl<R: ScrapeRunner + ?Sized>(
    runner: Arc<R>,
    req: &CrawlRequest,
) -> Result<Vec<ScrapeData>> {
    let seed = Url::parse(&req.url).map_err(|e| CrwError::InvalidUrl(e.to_string()))?;
    let max_concurrency = req.max_concurrency.unwrap_or(5).max(1);
    let limit = req.limit.max(1);

    let base_scrape = req
        .scrape_options
        .clone()
        .unwrap_or_else(|| ScrapeRequest::default_for_url(req.url.clone()));

    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    let mut collected: Vec<ScrapeData> = Vec::new();

    let seed_key = normalize_url(&seed, req.ignore_query_parameters);
    visited.insert(seed_key);
    queue.push_back((seed.to_string(), 0));

    while !queue.is_empty() {
        let batch: Vec<(String, u32)> = queue.drain(..max_concurrency.min(queue.len())).collect();

        let mut tasks = FuturesUnordered::new();
        for (url, depth) in batch {
            let mut sub = base_scrape.clone();
            sub.url = url.clone();
            let runner = runner.clone();
            tasks.push(async move {
                let result = runner.scrape(&sub).await;
                (url, depth, result)
            });
        }

        while let Some((url, depth, result)) = tasks.next().await {
            match result {
                Ok(data) => {
                    let html_for_links = fetch_html_for_links(&data, &url);
                    let new_links = data.links.clone().unwrap_or_else(|| {
                        if html_for_links.is_empty() {
                            Vec::new()
                        } else {
                            extract_links(&html_for_links, &url)
                        }
                    });

                    let max_depth = req.max_discovery_depth.unwrap_or(u32::MAX);
                    if req.sitemap != crw_core::SitemapMode::Skip && depth < max_depth {
                        for link in new_links {
                            let parsed = match Url::parse(&link) {
                                Ok(u) => u,
                                Err(_) => continue,
                            };
                            if !CrawlEngine::is_in_scope(
                                &seed,
                                &parsed,
                                req.allow_subdomains,
                                req.allow_external_links,
                            ) {
                                continue;
                            }
                            if !CrawlEngine::should_visit(
                                &parsed,
                                &req.include_paths,
                                &req.exclude_paths,
                            ) {
                                continue;
                            }
                            let key = normalize_url(&parsed, req.ignore_query_parameters);
                            if visited.contains(&key) {
                                continue;
                            }
                            visited.insert(key);
                            queue.push_back((parsed.to_string(), depth + 1));
                        }
                    }

                    collected.push(data);
                    if collected.len() >= limit {
                        return Ok(collected);
                    }
                }
                Err(e) => {
                    warn!(url = %url, error = %e, "crawl: page failed");
                }
            }
        }
    }

    debug!(total = collected.len(), "crawl finished");
    Ok(collected)
}

/// Helper used to discover new links from a page when the user did not request
/// the `links` format. Falls back to the raw HTML stored in `raw_html`, then
/// `html`, then to nothing.
fn fetch_html_for_links(data: &ScrapeData, fallback_url: &str) -> String {
    if let Some(h) = &data.raw_html {
        if !h.is_empty() {
            return h.clone();
        }
    }
    if let Some(h) = &data.html {
        if !h.is_empty() {
            return h.clone();
        }
    }
    if let Some(md) = &data.markdown {
        if !md.is_empty() {
            return md.clone();
        }
    }
    let _ = fallback_url;
    String::new()
}

/// Convenience for callers (e.g. the server handler) that just want to map a
/// `CrawlRequest` + `CrawlStatus` to the in-memory job state.
pub fn default_status() -> CrawlStatus {
    CrawlStatus::Queued
}

pub fn placeholder_result<T>() -> Result<T> {
    Err(CrwError::NotImplemented(
        "crawl engine placeholder".to_string(),
    ))
}

// Silence unused-import lint for `Mutex` on platforms where the dev-only mock
// does not use it.
#[allow(dead_code)]
fn _ensure_mutex_used(_m: &Mutex<()>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn should_visit_no_filters_always_true() {
        let url = Url::parse("https://example.com/x").unwrap();
        assert!(CrawlEngine::should_visit(&url, &[], &[]));
    }

    #[test]
    fn exclude_path_filters_out() {
        let url = Url::parse("https://example.com/blog/foo").unwrap();
        assert!(!CrawlEngine::should_visit(
            &url,
            &[],
            &["blog/.*".to_string()]
        ));
        let url2 = Url::parse("https://example.com/docs/x").unwrap();
        assert!(CrawlEngine::should_visit(
            &url2,
            &[],
            &["blog/.*".to_string()]
        ));
    }

    #[test]
    fn include_path_filters_in() {
        let url = Url::parse("https://example.com/blog/x").unwrap();
        assert!(!CrawlEngine::should_visit(
            &url,
            &["docs/.*".to_string()],
            &[]
        ));
        let url2 = Url::parse("https://example.com/docs/x").unwrap();
        assert!(CrawlEngine::should_visit(
            &url2,
            &["docs/.*".to_string()],
            &[]
        ));
    }

    #[test]
    fn glob_question_mark_matches_single_char() {
        let url = Url::parse("https://example.com/abc").unwrap();
        assert!(CrawlEngine::should_visit(&url, &["ab?".to_string()], &[]));
        let url2 = Url::parse("https://example.com/abcd").unwrap();
        assert!(!CrawlEngine::should_visit(&url2, &["ab?".to_string()], &[]));
    }

    #[test]
    fn is_in_scope_respects_subdomains() {
        let seed = Url::parse("https://example.com").unwrap();
        let same = Url::parse("https://example.com/x").unwrap();
        let sub = Url::parse("https://docs.example.com/x").unwrap();
        let other = Url::parse("https://other.com/x").unwrap();
        assert!(CrawlEngine::is_in_scope(&seed, &same, false, false));
        assert!(!CrawlEngine::is_in_scope(&seed, &sub, false, false));
        assert!(CrawlEngine::is_in_scope(&seed, &sub, true, false));
        assert!(!CrawlEngine::is_in_scope(&seed, &other, true, false));
        assert!(CrawlEngine::is_in_scope(&seed, &other, false, true));
    }

    #[test]
    fn normalize_url_drops_fragment() {
        let u = Url::parse("https://example.com/x?a=1#frag").unwrap();
        let n = normalize_url(&u, false);
        assert_eq!(n, "https://example.com/x?a=1");
    }

    #[test]
    fn normalize_url_drops_query_when_requested() {
        let u = Url::parse("https://example.com/x?a=1&b=2#frag").unwrap();
        let n = normalize_url(&u, true);
        assert_eq!(n, "https://example.com/x");
    }

    #[test]
    fn placeholder_returns_not_implemented() {
        let r: crw_core::Result<()> = placeholder_result();
        assert!(matches!(r, Err(CrwError::NotImplemented(_))));
    }

    struct MockRunner {
        responses: Mutex<HashMap<String, ScrapeData>>,
    }

    impl MockRunner {
        fn new(pairs: &[(&str, &str, &[&str])]) -> Arc<Self> {
            let mut map = HashMap::new();
            for (url, body, links) in pairs {
                let abs_links: Vec<String> = links
                    .iter()
                    .map(|l| {
                        if l.starts_with("http") {
                            l.to_string()
                        } else {
                            format!("https://example.com{l}")
                        }
                    })
                    .collect();
                let data = ScrapeData {
                    raw_html: Some(body.to_string()),
                    links: Some(abs_links),
                    metadata: crw_core::ScrapeMetadata {
                        title: Some(format!("Page {url}")),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                map.insert(url.to_string(), data);
            }
            Arc::new(Self {
                responses: Mutex::new(map),
            })
        }
    }

    #[async_trait]
    impl ScrapeRunner for MockRunner {
        async fn scrape(&self, request: &ScrapeRequest) -> Result<ScrapeData> {
            let map = self.responses.lock().unwrap();
            map.get(&request.url)
                .cloned()
                .ok_or_else(|| CrwError::Fetch(format!("no mock for {}", request.url)))
        }
    }

    #[tokio::test]
    async fn crw_crawl_bfs_visits_pages() {
        let runner: Arc<MockRunner> = MockRunner::new(&[
            (
                "https://example.com/",
                "<html><body><a href='/a'>A</a></body></html>",
                &["/a"],
            ),
            (
                "https://example.com/a",
                "<html><body><a href='/b'>B</a></body></html>",
                &["/b"],
            ),
            (
                "https://example.com/b",
                "<html><body>leaf</body></html>",
                &[],
            ),
        ]);
        let mut req = CrawlRequest::new("https://example.com/");
        req.limit = 10;
        let result = crw_crawl(runner, &req).await.unwrap();
        let titles: Vec<String> = result
            .iter()
            .map(|d| d.metadata.title.clone().unwrap_or_default())
            .collect();
        assert_eq!(result.len(), 3);
        assert_eq!(
            titles,
            vec![
                "Page https://example.com/".to_string(),
                "Page https://example.com/a".to_string(),
                "Page https://example.com/b".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn crw_crawl_respects_limit() {
        let runner: Arc<MockRunner> = MockRunner::new(&[
            (
                "https://example.com/",
                "<html><body><a href='/a'>A</a></body></html>",
                &["/a"],
            ),
            (
                "https://example.com/a",
                "<html><body>leaf</body></html>",
                &[],
            ),
        ]);
        let mut req = CrawlRequest::new("https://example.com/");
        req.limit = 1;
        let result = crw_crawl(runner, &req).await.unwrap();
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn crw_crawl_dedupes_urls() {
        let runner: Arc<MockRunner> = MockRunner::new(&[
            (
                "https://example.com/",
                "<html><body><a href='/a'>A</a></body></html>",
                &["/a"],
            ),
            (
                "https://example.com/a",
                "<html><body><a href='/'>root</a></body></html>",
                &["/"],
            ),
        ]);
        let mut req = CrawlRequest::new("https://example.com/");
        req.limit = 50;
        let result = crw_crawl(runner, &req).await.unwrap();
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn crw_crawl_respects_exclude_paths() {
        let runner: Arc<MockRunner> = MockRunner::new(&[
            (
                "https://example.com/",
                "<html><body><a href='/blog'>B</a></body></html>",
                &["/blog"],
            ),
            (
                "https://example.com/blog",
                "<html><body>blog</body></html>",
                &[],
            ),
        ]);
        let mut req = CrawlRequest::new("https://example.com/");
        req.exclude_paths = vec!["blog".into()];
        req.limit = 50;
        let result = crw_crawl(runner, &req).await.unwrap();
        assert_eq!(result.len(), 1);
    }
}
