//! HTML → Markdown conversion.

use htmd::HtmlToMarkdown;

/// Convert an HTML fragment to Markdown.
///
/// Returns an empty string if the input is empty. Note: the conversion is
/// intentionally tolerant of malformed HTML.
pub fn html_to_markdown(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let converter = HtmlToMarkdown::new();
    converter.convert(html).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(html_to_markdown(""), "");
        assert_eq!(html_to_markdown("   \n  "), "");
    }

    #[test]
    fn converts_heading_and_paragraph() {
        let md = html_to_markdown("<h1>Hello</h1><p>World</p>");
        assert!(md.contains("Hello"), "missing heading: {md}");
        assert!(md.contains("World"), "missing paragraph: {md}");
        assert!(md.contains('#'), "missing # marker: {md}");
    }

    #[test]
    fn converts_links() {
        let md = html_to_markdown(r#"<a href="https://example.com">Click</a>"#);
        assert!(md.contains("https://example.com"), "missing url: {md}");
        assert!(md.contains("Click"), "missing text: {md}");
    }

    #[test]
    fn converts_list() {
        let md = html_to_markdown("<ul><li>a</li><li>b</li></ul>");
        assert!(
            md.contains("a") && md.contains("b"),
            "missing list items: {md}"
        );
    }

    #[test]
    fn strips_inline_tags() {
        let md = html_to_markdown("<p>Hello <strong>World</strong></p>");
        assert!(md.contains("Hello"));
        assert!(md.contains("World"));
    }
}
