use chrono::{DateTime, Utc};
use crw_antibot::situation::SituationReport;
use serde::{Deserialize, Serialize};

fn default_formats() -> Vec<Format> {
    vec![Format::Markdown]
}

fn true_default() -> bool {
    true
}

fn default_limit() -> u32 {
    10
}

fn default_timeout() -> u32 {
    60_000
}

fn default_max_age() -> u64 {
    172_800_000
}

fn default_crawl_limit() -> usize {
    10_000
}

fn default_sitemap() -> SitemapMode {
    SitemapMode::Include
}

// ---------------------------------------------------------------------------------------
// Format enum (ScrapeRequest.formats)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Format {
    Markdown,
    Html,
    RawHtml,
    Links,
    Screenshot,
}

// ---------------------------------------------------------------------------------------
// Browser action
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrowserAction {
    #[serde(rename_all = "camelCase")]
    Wait { milliseconds: u64 },
    #[serde(rename_all = "camelCase")]
    Click { selector: String },
    #[serde(rename_all = "camelCase")]
    Screenshot { full_page: bool },
    #[serde(rename_all = "camelCase")]
    Write { text: String },
    #[serde(rename_all = "camelCase")]
    Press { key: String },
    #[serde(rename_all = "camelCase")]
    Scroll {
        direction: ScrollDirection,
        amount: u32,
    },
    #[serde(rename_all = "camelCase")]
    Scrape {},
    #[serde(rename_all = "camelCase")]
    ExecuteJavascript { script: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    Down,
}

// ---------------------------------------------------------------------------------------
// Proxy mode
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Basic,
    Enhanced,
    #[default]
    Auto,
}

// ---------------------------------------------------------------------------------------
// ScrapeRequest
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeRequest {
    pub url: String,

    #[serde(default = "default_formats")]
    pub formats: Vec<Format>,

    #[serde(default = "true_default")]
    pub only_main_content: bool,

    #[serde(default)]
    pub include_tags: Vec<String>,

    #[serde(default)]
    pub exclude_tags: Vec<String>,

    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,

    #[serde(default)]
    pub wait_for: u64,

    #[serde(default)]
    pub mobile: bool,

    #[serde(default)]
    pub skip_tls_verification: bool,

    #[serde(default = "default_timeout")]
    pub timeout: u32,

    #[serde(default)]
    pub actions: Vec<BrowserAction>,

    #[serde(default)]
    pub remove_base64_images: bool,

    #[serde(default = "true_default")]
    pub block_ads: bool,

    #[serde(default)]
    pub proxy: ProxyMode,

    #[serde(default = "default_max_age")]
    pub max_age: u64,

    #[serde(default = "true_default")]
    pub store_in_cache: bool,
}

impl ScrapeRequest {
    pub fn default_for_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            formats: default_formats(),
            only_main_content: true,
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            headers: std::collections::HashMap::new(),
            wait_for: 0,
            mobile: false,
            skip_tls_verification: false,
            timeout: default_timeout(),
            actions: Vec::new(),
            remove_base64_images: false,
            block_ads: true,
            proxy: ProxyMode::Auto,
            max_age: default_max_age(),
            store_in_cache: true,
        }
    }
}

// ---------------------------------------------------------------------------------------
// ScrapeMetadata / ScrapeData / ScrapeResponse
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub source_url: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "statusCode")]
    pub status_code: Option<u16>,
    pub error: Option<String>,
    #[serde(default)]
    pub og_title: Option<String>,
    #[serde(default)]
    pub og_description: Option<String>,
    #[serde(default)]
    pub og_image: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Confidence score of the markdown extraction (0.0..=1.0). Drives the
    /// caller's decision to escalate to CDP or FlareSolverr when low.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_quality: Option<f32>,
    /// Coarse page type classification: "article" | "product" | "listing"
    /// | "forum" | "doc" | "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_type: Option<String>,
    /// Phase B: structured diagnosis of the HTTP response — provider name,
    /// suggested ladder step, and the evidence that triggered the
    /// detection. Optional so existing serialised payloads still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub situation: Option<SituationReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    pub metadata: ScrapeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeResponse {
    pub success: bool,
    pub data: ScrapeData,
}

impl ScrapeResponse {
    pub fn ok(data: ScrapeData) -> Self {
        Self {
            success: true,
            data,
        }
    }
}

// ---------------------------------------------------------------------------------------
// SitemapMode
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SitemapMode {
    Skip,
    #[default]
    Include,
    Only,
}

// ---------------------------------------------------------------------------------------
// CrawlRequest
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlRequest {
    pub url: String,

    #[serde(default)]
    pub exclude_paths: Vec<String>,

    #[serde(default)]
    pub include_paths: Vec<String>,

    #[serde(default)]
    pub max_discovery_depth: Option<u32>,

    #[serde(default = "default_sitemap")]
    pub sitemap: SitemapMode,

    #[serde(default = "true_default")]
    pub ignore_query_parameters: bool,

    #[serde(default)]
    pub regex_on_full_url: bool,

    #[serde(default = "default_crawl_limit")]
    pub limit: usize,

    #[serde(default)]
    pub crawl_entire_domain: bool,

    #[serde(default)]
    pub allow_external_links: bool,

    #[serde(default)]
    pub allow_subdomains: bool,

    #[serde(default)]
    pub ignore_robots_txt: bool,

    #[serde(default)]
    pub delay: Option<f64>,

    #[serde(default)]
    pub max_concurrency: Option<usize>,

    #[serde(default)]
    pub scrape_options: Option<ScrapeRequest>,
}

impl CrawlRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            exclude_paths: Vec::new(),
            include_paths: Vec::new(),
            max_discovery_depth: None,
            sitemap: SitemapMode::Include,
            ignore_query_parameters: true,
            regex_on_full_url: false,
            limit: default_crawl_limit(),
            crawl_entire_domain: false,
            allow_external_links: false,
            allow_subdomains: false,
            ignore_robots_txt: false,
            delay: None,
            max_concurrency: None,
            scrape_options: None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// CrawlResponse / CrawlStatusResponse / CrawlStatus
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrawlStatus {
    Scraping,
    Completed,
    Failed,
    Cancelled,
    Queued,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlResponse {
    pub success: bool,
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub credits_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrawlStatusResponse {
    pub status: CrawlStatus,

    #[serde(default)]
    pub total: u32,

    #[serde(default)]
    pub completed: u32,

    #[serde(default)]
    pub credits_used: u32,

    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub duration: Option<f64>,

    #[serde(default)]
    pub next: Option<String>,

    #[serde(default)]
    pub data: Vec<ScrapeData>,

    #[serde(default)]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------------------
// MapRequest / MapResponse
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapRequest {
    pub url: String,

    #[serde(default)]
    pub search: Option<String>,

    #[serde(default = "default_sitemap")]
    pub sitemap: SitemapMode,

    #[serde(default)]
    pub include_subdomains: bool,

    #[serde(default = "true_default")]
    pub ignore_query_parameters: bool,

    #[serde(default)]
    pub ignore_cache: bool,

    #[serde(default = "default_crawl_limit")]
    pub limit: usize,

    #[serde(default)]
    pub timeout: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapLink {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapResponse {
    pub success: bool,
    #[serde(default)]
    pub links: Vec<MapLink>,
}

// ---------------------------------------------------------------------------------------
// SearchRequest / SearchResponse
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSource {
    Web,
    Images,
    News,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,

    #[serde(default = "default_limit")]
    pub limit: u32,

    #[serde(default = "default_sources")]
    pub sources: Vec<SearchSource>,

    #[serde(default)]
    pub include_domains: Vec<String>,

    #[serde(default)]
    pub exclude_domains: Vec<String>,

    #[serde(default)]
    pub tbs: Option<String>,

    #[serde(default = "default_timeout")]
    pub timeout: u32,

    #[serde(default)]
    pub ignore_invalid_urls: bool,

    #[serde(default)]
    pub scrape_options: Option<ScrapeRequest>,
}

fn default_sources() -> Vec<SearchSource> {
    vec![SearchSource::Web]
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultItem {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchData {
    #[serde(default)]
    pub web: Vec<SearchResultItem>,
    #[serde(default)]
    pub images: Vec<SearchResultItem>,
    #[serde(default)]
    pub news: Vec<SearchResultItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub success: bool,
    pub data: SearchData,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub credits_used: u32,
}

// ---------------------------------------------------------------------------------------
// ErrorResponse
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
    pub message: String,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

impl ErrorResponse {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: false,
            error: code.into(),
            message: message.into(),
            details: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrape_request_default_deserialize_minimal() {
        let json = r##"{"url":"https://example.com"}"##;
        let req: ScrapeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.url, "https://example.com");
        assert_eq!(req.formats, vec![Format::Markdown]);
        assert!(req.only_main_content);
        assert!(req.block_ads);
        assert!(req.store_in_cache);
        assert_eq!(req.timeout, 60_000);
        assert_eq!(req.max_age, 172_800_000);
        assert_eq!(req.proxy, ProxyMode::Auto);
    }

    #[test]
    fn scrape_request_full_deserialize() {
        let json = r##"{
            "url":"https://example.com",
            "formats":["markdown","html","links"],
            "onlyMainContent":false,
            "includeTags":["article"],
            "excludeTags":["nav"],
            "headers":{"X-Foo":"bar"},
            "waitFor":1500,
            "mobile":true,
            "skipTlsVerification":true,
            "timeout":30000,
            "actions":[{"type":"wait","milliseconds":1000},{"type":"click","selector":"#x"}],
            "removeBase64Images":true,
            "blockAds":false,
            "proxy":"basic",
            "maxAge":60000,
            "storeInCache":false
        }"##;
        let req: ScrapeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.url, "https://example.com");
        assert_eq!(
            req.formats,
            vec![Format::Markdown, Format::Html, Format::Links]
        );
        assert!(!req.only_main_content);
        assert_eq!(req.include_tags, vec!["article"]);
        assert_eq!(req.exclude_tags, vec!["nav"]);
        assert_eq!(req.headers.get("X-Foo").unwrap(), "bar");
        assert_eq!(req.wait_for, 1500);
        assert!(req.mobile);
        assert!(req.skip_tls_verification);
        assert_eq!(req.timeout, 30_000);
        assert_eq!(req.actions.len(), 2);
        assert!(req.remove_base64_images);
        assert!(!req.block_ads);
        assert_eq!(req.proxy, ProxyMode::Basic);
        assert_eq!(req.max_age, 60_000);
        assert!(!req.store_in_cache);
    }

    #[test]
    fn scrape_response_serialize_camelcase() {
        let resp = ScrapeResponse::ok(ScrapeData {
            markdown: Some("# Hi".into()),
            html: None,
            raw_html: None,
            links: None,
            screenshot: None,
            metadata: ScrapeMetadata {
                title: Some("Hi".into()),
                status_code: Some(200),
                ..Default::default()
            },
        });
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["data"]["markdown"], "# Hi");
        assert_eq!(v["data"]["metadata"]["title"], "Hi");
        assert_eq!(v["data"]["metadata"]["statusCode"], 200);
        assert!(v["data"].get("html").is_none());
    }

    #[test]
    fn crawl_request_deserialize() {
        let json = r##"{
            "url":"https://example.com",
            "limit":100,
            "sitemap":"only",
            "allowExternalLinks":true,
            "scrapeOptions":{"url":"https://example.com"}
        }"##;
        let req: CrawlRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.url, "https://example.com");
        assert_eq!(req.limit, 100);
        assert_eq!(req.sitemap, SitemapMode::Only);
        assert!(req.allow_external_links);
        assert!(req.scrape_options.is_some());
    }

    #[test]
    fn crawl_status_response_serialize() {
        let resp = CrawlStatusResponse {
            status: CrawlStatus::Scraping,
            total: 10,
            completed: 5,
            credits_used: 5,
            expires_at: None,
            created_at: None,
            completed_at: None,
            duration: None,
            next: None,
            data: vec![],
            error: None,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["status"], "scraping");
        assert_eq!(v["total"], 10);
        assert_eq!(v["completed"], 5);
    }

    #[test]
    fn map_request_deserialize_defaults() {
        let json = r##"{"url":"https://example.com"}"##;
        let req: MapRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.url, "https://example.com");
        assert_eq!(req.sitemap, SitemapMode::Include);
        assert!(req.ignore_query_parameters);
        assert_eq!(req.limit, 10_000);
        assert!(!req.include_subdomains);
        assert!(req.search.is_none());
    }

    #[test]
    fn map_response_serialize() {
        let resp = MapResponse {
            success: true,
            links: vec![MapLink {
                url: "https://example.com/a".into(),
                title: Some("A".into()),
                description: None,
            }],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["links"][0]["url"], "https://example.com/a");
        assert_eq!(v["links"][0]["title"], "A");
    }

    #[test]
    fn search_request_deserialize() {
        let json = r##"{"query":"rust","limit":5,"sources":["web","news"]}"##;
        let req: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.query, "rust");
        assert_eq!(req.limit, 5);
        assert_eq!(req.sources, vec![SearchSource::Web, SearchSource::News]);
        assert_eq!(req.timeout, 60_000);
    }

    #[test]
    fn search_response_serialize() {
        let resp = SearchResponse {
            success: true,
            data: SearchData {
                web: vec![SearchResultItem {
                    title: Some("t".into()),
                    url: "https://example.com".into(),
                    ..Default::default()
                }],
                images: vec![],
                news: vec![],
            },
            warning: None,
            id: Some("abc".into()),
            credits_used: 1,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["data"]["web"][0]["url"], "https://example.com");
        assert_eq!(v["creditsUsed"], 1);
    }

    #[test]
    fn error_response_serialize() {
        let e = ErrorResponse::new("INVALID_URL", "bad");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["error"], "INVALID_URL");
        assert_eq!(v["message"], "bad");
        assert!(v.get("details").is_none() || v["details"].is_null());
    }

    #[test]
    fn proxy_mode_roundtrip() {
        for s in ["basic", "enhanced", "auto"] {
            let m: ProxyMode = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            let back = serde_json::to_string(&m).unwrap();
            assert_eq!(back.trim_matches('"'), s);
        }
    }

    #[test]
    fn browser_action_deserialize() {
        let json = r##"[{"type":"wait","milliseconds":500},{"type":"click","selector":"#x"}]"##;
        let actions: Vec<BrowserAction> = serde_json::from_str(json).unwrap();
        assert_eq!(actions.len(), 2);
        matches!(actions[0], BrowserAction::Wait { .. });
        matches!(actions[1], BrowserAction::Click { .. });
    }
}
