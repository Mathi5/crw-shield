//! Link extraction from HTML. Resolves relative URLs against a base URL.

use scraper::{Html, Selector};
use url::Url;

pub fn extract_links(html: &str, base_url: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let base = Url::parse(base_url).ok();
    let mut out: Vec<String> = Vec::new();
    for el in doc.select(&sel) {
        if let Some(href) = el.value().attr("href") {
            let trimmed = href.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with("javascript:")
                || trimmed.starts_with("mailto:")
                || trimmed.starts_with("tel:")
            {
                continue;
            }
            let resolved = match &base {
                Some(b) => b.join(trimmed).ok(),
                None => Url::parse(trimmed).ok(),
            };
            if let Some(u) = resolved {
                out.push(u.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_absolute_links() {
        let html = r##"<html><body>
            <a href="https://a.com/">A</a>
            <a href="https://b.com/">B</a>
        </body></html>"##;
        let links = extract_links(html, "https://example.com");
        assert_eq!(
            links,
            vec!["https://a.com/".to_string(), "https://b.com/".to_string()]
        );
    }

    #[test]
    fn resolves_relative_links() {
        let html = r##"<html><body><a href="/foo">F</a><a href="bar">B</a></body></html>"##;
        let links = extract_links(html, "https://example.com/dir/");
        assert_eq!(
            links,
            vec![
                "https://example.com/foo".to_string(),
                "https://example.com/dir/bar".to_string()
            ]
        );
    }

    #[test]
    fn skips_empty_javascript_mailto() {
        let html = r##"<html><body>
            <a href="">empty</a>
            <a href="javascript:void(0)">js</a>
            <a href="mailto:a@b.c">mail</a>
            <a href="#x">frag</a>
            <a href="https://ok.com">ok</a>
        </body></html>"##;
        let links = extract_links(html, "https://example.com");
        assert_eq!(links, vec!["https://ok.com/".to_string()]);
    }

    #[test]
    fn empty_html_returns_empty() {
        let links = extract_links("", "https://example.com");
        assert!(links.is_empty());
    }
}
