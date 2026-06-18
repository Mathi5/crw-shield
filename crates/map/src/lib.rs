//! URL discovery (sitemap + link extraction).

use crw_core::{CrwError, MapLink, MapRequest, MapResponse, Result, SitemapMode};
use crw_extract::extract_links;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tracing::{debug, warn};
use url::Url;

pub fn parse_sitemap(xml: &str) -> Result<Vec<MapLink>> {
    if !xml.trim_start().starts_with('<') {
        return Err(CrwError::Extraction(
            "sitemap parse: input is not XML".to_string(),
        ));
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out: Vec<MapLink> = Vec::new();
    let mut current: Option<MapLink> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name == b"url" {
                    current = Some(MapLink {
                        url: String::new(),
                        title: None,
                        description: None,
                    });
                }
            }
            Ok(Event::Empty(_)) => {
                // skip empty <url/>
            }
            Ok(Event::Text(t)) => {
                if let Some(cur) = current.as_mut() {
                    let bytes = t.into_inner();
                    let s = String::from_utf8_lossy(&bytes).to_string();
                    if cur.url.is_empty() {
                        cur.url = s;
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name == b"url" {
                    if let Some(cur) = current.take() {
                        if !cur.url.is_empty() {
                            out.push(cur);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(CrwError::Extraction(format!("sitemap parse: {e}")));
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Detect whether `xml` looks like a sitemap *index* (root element
/// `<sitemapindex>`) vs a regular `<urlset>` sitemap. The heuristic is
/// intentionally cheap — used by `discover` to pick the right parser.
pub fn looks_like_sitemap_index(xml: &str) -> bool {
    let lower = xml.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    if let Some(after_langle) = trimmed.strip_prefix('<') {
        let name: String = after_langle
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        return name == "sitemapindex";
    }
    false
}

/// Parse a sitemap *index* and return the list of child sitemap URLs declared
/// in `<sitemap><loc>...</loc></sitemap>` blocks.
pub fn parse_sitemap_index(xml: &str) -> Result<Vec<String>> {
    if !xml.trim_start().starts_with('<') {
        return Err(CrwError::Extraction(
            "sitemap index parse: input is not XML".to_string(),
        ));
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name == b"sitemap" {
                    current = Some(String::new());
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(cur) = current.as_mut() {
                    let bytes = t.into_inner();
                    let s = String::from_utf8_lossy(&bytes).to_string();
                    if cur.is_empty() {
                        *cur = s;
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name == b"sitemap" {
                    if let Some(loc) = current.take() {
                        if !loc.is_empty() {
                            out.push(loc);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(CrwError::Extraction(format!("sitemap index parse: {e}")));
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Fetch the body of `url` as text using a lightweight reqwest client. Used
/// by `discover` to grab sitemaps and seed pages.
async fn fetch_text(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("crw-shield/0.1 (+https://github.com/crw-shield)")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| CrwError::Fetch(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CrwError::Fetch(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CrwError::Http {
            status: status.as_u16(),
            message: format!("HTTP {status} for {url}"),
        });
    }
    resp.text()
        .await
        .map_err(|e| CrwError::Fetch(e.to_string()))
}

fn in_scope(seed: &Url, candidate: &Url, include_subdomains: bool) -> bool {
    match (seed.host_str(), candidate.host_str()) {
        (Some(s), Some(c)) => {
            if s == c {
                return true;
            }
            if include_subdomains && c.ends_with(&format!(".{s}")) {
                return true;
            }
            false
        }
        _ => false,
    }
}

fn normalize_for_dedupe(raw: &str, ignore_query_parameters: bool) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    let mut cloned = parsed;
    cloned.set_fragment(None);
    if ignore_query_parameters {
        cloned.set_query(None);
    }
    Some(cloned.to_string())
}

/// Resolve a sitemap `<loc>` value against the URL the sitemap was fetched
/// from. Falls back to the original string when parsing fails.
fn resolve_against(loc: &str, base: &str) -> Option<String> {
    let parsed = Url::parse(loc).ok();
    match parsed {
        Some(u) if u.has_authority() => Some(u.to_string()),
        _ => Url::parse(base)
            .ok()
            .and_then(|b| b.join(loc).ok())
            .map(|u| u.to_string()),
    }
}

/// Discover URLs reachable from `req.url` by combining sitemap parsing and
/// HTML link extraction. `discover` honors the `sitemap` mode:
/// - `Skip`   — only the seed HTML page is inspected
/// - `Include` (default) — sitemap is parsed, then the seed page is fetched
///   for any links not already found
/// - `Only`   — only the sitemap (and any sitemap index it points to) is used
pub async fn discover(req: &MapRequest) -> Result<MapResponse> {
    let seed = Url::parse(&req.url).map_err(|e| CrwError::InvalidUrl(e.to_string()))?;

    let mut collected: Vec<MapLink> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let push = |collected: &mut Vec<MapLink>,
                seen: &mut std::collections::HashSet<String>,
                raw: &str,
                ignore_query: bool| {
        if let Some(norm) = normalize_for_dedupe(raw, ignore_query) {
            if seen.insert(norm.clone()) {
                collected.push(MapLink {
                    url: norm,
                    title: None,
                    description: None,
                });
            }
        }
    };

    if req.sitemap != SitemapMode::Skip {
        let sitemap_candidates = sitemap_candidate_urls(&seed);
        for sitemap_url in sitemap_candidates {
            match fetch_text(&sitemap_url).await {
                Ok(body) => {
                    if looks_like_sitemap_index(&body) {
                        match parse_sitemap_index(&body) {
                            Ok(child_indexes) => {
                                for child in child_indexes {
                                    if let Ok(child_body) = fetch_text(&child).await {
                                        if let Ok(links) = parse_sitemap(&child_body) {
                                            for link in links {
                                                let resolved =
                                                    resolve_against(&link.url, &sitemap_url)
                                                        .unwrap_or_else(|| link.url.clone());
                                                let parsed = Url::parse(&resolved).ok();
                                                if let Some(p) = parsed {
                                                    if !in_scope(&seed, &p, req.include_subdomains)
                                                    {
                                                        continue;
                                                    }
                                                }
                                                push(
                                                    &mut collected,
                                                    &mut seen,
                                                    &resolved,
                                                    req.ignore_query_parameters,
                                                );
                                            }
                                        } else {
                                            warn!(child=%child, "sitemap child parse failed");
                                        }
                                    } else {
                                        debug!(child=%child, "sitemap child fetch failed");
                                    }
                                }
                            }
                            Err(e) => {
                                debug!(error=%e, "sitemap index parse failed");
                            }
                        }
                    } else {
                        match parse_sitemap(&body) {
                            Ok(links) => {
                                for link in links {
                                    let resolved = resolve_against(&link.url, &sitemap_url)
                                        .unwrap_or_else(|| link.url.clone());
                                    let parsed = Url::parse(&resolved).ok();
                                    if let Some(p) = parsed {
                                        if !in_scope(&seed, &p, req.include_subdomains) {
                                            continue;
                                        }
                                    }
                                    push(
                                        &mut collected,
                                        &mut seen,
                                        &resolved,
                                        req.ignore_query_parameters,
                                    );
                                }
                            }
                            Err(e) => {
                                debug!(error=%e, "sitemap parse failed (non-fatal)");
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!(url=%sitemap_url, error=%e, "sitemap fetch failed (non-fatal)");
                }
            }
        }
    }

    if req.sitemap != SitemapMode::Only {
        match fetch_text(req.url.trim_end_matches('/')).await {
            Ok(body) => {
                for link in extract_links(&body, seed.as_ref()) {
                    let parsed = match Url::parse(&link) {
                        Ok(u) => u,
                        Err(_) => continue,
                    };
                    if !in_scope(&seed, &parsed, req.include_subdomains) {
                        continue;
                    }
                    push(
                        &mut collected,
                        &mut seen,
                        &link,
                        req.ignore_query_parameters,
                    );
                }
            }
            Err(e) => {
                debug!(error=%e, "seed page fetch failed (non-fatal)");
            }
        }
    }

    if let Some(search) = req.search.as_deref() {
        let needle = search.to_ascii_lowercase();
        if !needle.is_empty() {
            collected.retain(|link| link.url.to_ascii_lowercase().contains(&needle));
        }
    }

    if collected.len() > req.limit {
        collected.truncate(req.limit);
    }

    Ok(MapResponse {
        success: true,
        links: collected,
    })
}

/// Build the list of sitemap URLs to try for a given seed URL, in priority
/// order: `robots.txt`-style `sitemap.xml` next to the seed, plus the seed
/// itself (some sites expose their sitemap at the root).
fn sitemap_candidate_urls(seed: &Url) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(mut s) = seed.clone().join("sitemap.xml") {
        s.set_fragment(None);
        out.push(s.to_string());
    }
    if let Some(host) = seed.host_str() {
        let scheme = seed.scheme();
        out.push(format!("{scheme}://{host}/sitemap.xml"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sitemap_extracts_urls() {
        let xml = r#"<?xml version="1.0"?>
            <urlset>
              <url><loc>https://a.com/1</loc></url>
              <url><loc>https://a.com/2</loc></url>
              <url><loc>https://a.com/3</loc></url>
            </urlset>"#;
        let links = parse_sitemap(xml).unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].url, "https://a.com/1");
        assert_eq!(links[2].url, "https://a.com/3");
    }

    #[test]
    fn parse_empty_sitemap_returns_empty() {
        let links = parse_sitemap(r#"<?xml version="1.0"?><urlset></urlset>"#).unwrap();
        assert!(links.is_empty());
    }

    #[test]
    fn parse_invalid_xml_returns_error() {
        let links = parse_sitemap("not xml");
        assert!(links.is_err());
    }

    #[test]
    fn parse_sitemap_index_extracts_child_loc() {
        let xml = r#"<?xml version="1.0"?>
            <sitemapindex>
              <sitemap><loc>https://a.com/s1.xml</loc></sitemap>
              <sitemap><loc>https://a.com/s2.xml</loc></sitemap>
            </sitemapindex>"#;
        let links = parse_sitemap_index(xml).unwrap();
        assert_eq!(links, vec!["https://a.com/s1.xml", "https://a.com/s2.xml"]);
    }

    #[test]
    fn sitemap_candidate_urls_uses_seed_origin() {
        let seed = Url::parse("https://example.com/blog/").unwrap();
        let candidates = sitemap_candidate_urls(&seed);
        assert!(candidates
            .iter()
            .any(|u| u == "https://example.com/sitemap.xml"));
        assert!(candidates
            .iter()
            .any(|u| u == "https://example.com/blog/sitemap.xml"));
    }

    #[test]
    fn in_scope_filters_external_domains() {
        let seed = Url::parse("https://example.com").unwrap();
        let same = Url::parse("https://example.com/x").unwrap();
        let sub = Url::parse("https://docs.example.com/x").unwrap();
        let other = Url::parse("https://other.com/x").unwrap();
        assert!(in_scope(&seed, &same, false));
        assert!(!in_scope(&seed, &sub, false));
        assert!(in_scope(&seed, &sub, true));
        assert!(!in_scope(&seed, &other, true));
    }

    #[tokio::test]
    async fn discover_invalid_url_returns_error() {
        let req = MapRequest {
            url: "not a url".into(),
            ..MapRequest {
                url: String::new(),
                search: None,
                sitemap: SitemapMode::Include,
                include_subdomains: false,
                ignore_query_parameters: true,
                ignore_cache: false,
                limit: 10_000,
                timeout: None,
            }
        };
        let r = discover(&req).await;
        assert!(matches!(r, Err(CrwError::InvalidUrl(_))));
    }
}
