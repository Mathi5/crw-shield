//! HTML metadata extraction (title, description, OG tags, language, etc).

use crw_core::ScrapeMetadata;
use scraper::{Html, Selector};

pub fn extract_metadata(html: &str, url: &str, status_code: u16) -> ScrapeMetadata {
    let doc = Html::parse_document(html);
    let mut meta = ScrapeMetadata {
        url: Some(url.to_string()),
        source_url: Some(url.to_string()),
        status_code: Some(status_code),
        ..Default::default()
    };

    meta.title = select_text(&doc, "title")
        .or_else(|| select_attr(&doc, "meta[property=\"og:title\"]", "content"));
    meta.description = select_attr(&doc, "meta[name=\"description\"]", "content")
        .or_else(|| select_attr(&doc, "meta[property=\"og:description\"]", "content"));
    meta.og_title = select_attr(&doc, "meta[property=\"og:title\"]", "content");
    meta.og_description = select_attr(&doc, "meta[property=\"og:description\"]", "content");
    meta.og_image = select_attr(&doc, "meta[property=\"og:image\"]", "content");
    meta.author = select_attr(&doc, "meta[name=\"author\"]", "content");

    if let Some(kw) = select_attr(&doc, "meta[name=\"keywords\"]", "content") {
        meta.keywords = kw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    meta.language = select_attr(&doc, "html", "lang")
        .or_else(|| select_attr(&doc, "meta[http-equiv=\"content-language\"]", "content"));

    // Phase D: Schema.org JSON-LD extraction. Done last because it can
    // short-circuit on parse failure and we want the other metadata
    // fields to still be populated.
    meta.schema_org_data = extract_schema_org(html);

    meta
}

/// Extract the first `<script type="application/ld+json">` block, parse
/// it as JSON, and return the value. Returns `None` if no JSON-LD is
/// present or the JSON is invalid.
///
/// Schema.org can be a single object or an array of objects — we return
/// the parsed value as-is so the client can introspect `@graph` arrays
/// (a common pattern in news articles) if they want to.
pub fn extract_schema_org(html: &str) -> Option<serde_json::Value> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).ok()?;
    let script = doc.select(&sel).next()?;
    let raw = script.text().collect::<String>();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn select_text(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn select_attr(doc: &Html, selector: &str, attr: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr(attr).map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_description() {
        let html = r#"
            <html lang="en">
              <head>
                <title>Hello World</title>
                <meta name="description" content="A description"/>
              </head>
              <body></body>
            </html>
        "#;
        let m = extract_metadata(html, "https://example.com", 200);
        assert_eq!(m.title.as_deref(), Some("Hello World"));
        assert_eq!(m.description.as_deref(), Some("A description"));
        assert_eq!(m.language.as_deref(), Some("en"));
        assert_eq!(m.url.as_deref(), Some("https://example.com"));
        assert_eq!(m.status_code, Some(200));
    }

    #[test]
    fn falls_back_to_og_tags() {
        let html = r#"
            <html>
              <head>
                <meta property="og:title" content="OG Title"/>
                <meta property="og:description" content="OG Desc"/>
                <meta property="og:image" content="https://example.com/i.png"/>
              </head>
            </html>
        "#;
        let m = extract_metadata(html, "https://example.com", 200);
        assert_eq!(m.title.as_deref(), Some("OG Title"));
        assert_eq!(m.description.as_deref(), Some("OG Desc"));
        assert_eq!(m.og_image.as_deref(), Some("https://example.com/i.png"));
    }

    #[test]
    fn extracts_keywords() {
        let html = r#"<html><head><meta name="keywords" content="a, b, c"></head></html>"#;
        let m = extract_metadata(html, "u", 200);
        assert_eq!(m.keywords, vec!["a", "b", "c"]);
    }

    // Phase D: Schema.org JSON-LD tests. Cover the three relevant branches:
    // (1) present and valid, (2) absent, (3) present but invalid JSON.

    #[test]
    fn extracts_schema_org_jsonld() {
        let html = r#"
            <html>
              <head>
                <script type="application/ld+json">
                  {"@context": "https://schema.org", "@type": "Article", "headline": "Test"}
                </script>
              </head>
              <body></body>
            </html>
        "#;
        let sd = extract_schema_org(html).expect("schema_org present");
        assert_eq!(sd["@type"], "Article");
        assert_eq!(sd["headline"], "Test");
    }

    #[test]
    fn missing_schema_org_returns_none() {
        let html = r#"<html><body>No JSON-LD here.</body></html>"#;
        assert!(extract_schema_org(html).is_none());
    }

    #[test]
    fn invalid_json_schema_org_returns_none() {
        let html = r#"
            <html>
              <head>
                <script type="application/ld+json">this is { not valid json</script>
              </head>
            </html>
        "#;
        // Malformed JSON must not panic — we surface None so the caller
        // can fall back gracefully.
        assert!(extract_schema_org(html).is_none());
    }

    #[test]
    fn schema_org_populated_via_extract_metadata() {
        let html = r#"
            <html>
              <head>
                <title>Test</title>
                <script type="application/ld+json">
                  {"@type": "Recipe", "name": "Pancakes"}
                </script>
              </head>
            </html>
        "#;
        let m = extract_metadata(html, "https://example.com", 200);
        let sd = m.schema_org_data.expect("schema_org_data populated");
        assert_eq!(sd["@type"], "Recipe");
        // The other metadata fields must still be populated.
        assert_eq!(m.title.as_deref(), Some("Test"));
    }

    #[test]
    fn extracts_author() {
        let html = r#"<html><head><meta name="author" content="Alice"></head></html>"#;
        let m = extract_metadata(html, "u", 200);
        assert_eq!(m.author.as_deref(), Some("Alice"));
    }

    #[test]
    fn empty_html_yields_only_url_and_status() {
        let m = extract_metadata("", "https://example.com", 500);
        assert_eq!(m.url.as_deref(), Some("https://example.com"));
        assert_eq!(m.status_code, Some(500));
        assert!(m.title.is_none());
        assert!(m.description.is_none());
    }
}
