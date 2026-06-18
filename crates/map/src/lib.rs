//! URL discovery (sitemap + link extraction).
//!
//! TODO: Phase 2 — fetch sitemap.xml, walk links, respect robots.txt.

use crw_core::{CrwError, MapLink, MapRequest, MapResponse, Result};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

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
            Ok(Event::Empty(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name == b"url" {
                    // empty <url/> — skip
                }
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

pub async fn discover(_req: &MapRequest) -> Result<MapResponse> {
    Err(CrwError::NotImplemented("map — Phase 2".to_string()))
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

    #[tokio::test]
    async fn discover_not_implemented() {
        let req = MapRequest {
            url: "https://example.com".into(),
            search: None,
            sitemap: Default::default(),
            include_subdomains: false,
            ignore_query_parameters: true,
            ignore_cache: false,
            limit: 100,
            timeout: None,
        };
        let r = discover(&req).await;
        assert!(matches!(r, Err(CrwError::NotImplemented(_))));
    }
}
