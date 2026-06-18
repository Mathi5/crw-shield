//! Content extraction — stripping boilerplate and filtering by tag.

use scraper::{element_ref::ElementRef, Html, Selector};

const UNWANTED_SELECTORS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "link", "meta", "nav", "footer", "aside",
];

/// Walk the tree and serialise it manually, skipping any element whose tag
/// (or any ancestor's tag) matches one of the unwanted selectors.
pub fn strip_unwanted(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let mut out = String::with_capacity(html.len());
    let root = doc.tree.root();
    for child in root.children() {
        walk_and_serialise(child, &mut out, &mut Vec::new());
    }
    out
}

fn walk_and_serialise<'a>(
    node: ego_tree::NodeRef<'a, scraper::node::Node>,
    out: &mut String,
    ancestor_unwanted: &mut Vec<bool>,
) {
    if let Some(el) = ElementRef::wrap(node) {
        let tag = el.value().name();
        let is_unwanted = UNWANTED_SELECTORS.contains(&tag);
        ancestor_unwanted.push(is_unwanted);
        if !is_unwanted {
            serialise_open(el, out);
        }
        for child in node.children() {
            walk_and_serialise(child, out, ancestor_unwanted);
        }
        if !is_unwanted {
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        ancestor_unwanted.pop();
    } else if !ancestor_unwanted.iter().any(|b| *b) {
        // text / doctype / comment — emit if no ancestor is unwanted
        match node.value() {
            scraper::node::Node::Text(t) => out.push_str(t),
            scraper::node::Node::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            scraper::node::Node::Doctype(d) => {
                out.push_str("<!DOCTYPE ");
                out.push_str(&d.name);
                out.push('>');
            }
            _ => {}
        }
    }
}

fn serialise_open(el: ElementRef<'_>, out: &mut String) {
    let name = el.value().name();
    out.push('<');
    out.push_str(name);
    for (k, v) in el.value().attrs() {
        out.push(' ');
        out.push_str(k);
        if !v.is_empty() {
            out.push_str("=\"");
            push_escaped_attr(out, v);
            out.push('"');
        }
    }
    if matches!(name, "br" | "img" | "hr" | "meta" | "link" | "input") {
        out.push_str(" />");
        return;
    }
    out.push('>');
    let _ = el; // silence last-use if compiler can't see
}

fn push_escaped_attr(out: &mut String, v: &str) {
    for ch in v.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// Trimmed content extraction — returns the inner HTML of `<main>` or
/// `<article>` or `<body>` when `only_main_content` is requested. Falls back to
/// stripping boilerplate tags.
pub fn extract_main_content(html: &str) -> String {
    let doc = Html::parse_document(html);
    for selector_str in ["main", "article", "[role=\"main\"]", "body"] {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = doc.select(&sel).next() {
                return el.inner_html();
            }
        }
    }
    html.to_string()
}

/// Remove elements whose tag is in `exclude_tags` and keep only the ones in
/// `include_tags` (when `include_tags` is non-empty). The result is the
/// serialised HTML of the surviving elements.
pub fn filter_tags(html: &str, include_tags: &[String], exclude_tags: &[String]) -> String {
    if include_tags.is_empty() && exclude_tags.is_empty() {
        return html.to_string();
    }
    let include: Vec<String> = include_tags
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let exclude: Vec<String> = exclude_tags
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();

    let doc = Html::parse_fragment(html);
    let mut out = String::new();
    let root = doc.tree.root();
    let mut ancestors: Vec<String> = Vec::new();
    for child in root.children() {
        walk_filter(child, &mut out, &mut ancestors, &include, &exclude);
    }
    out
}

fn walk_filter<'a>(
    node: ego_tree::NodeRef<'a, scraper::node::Node>,
    out: &mut String,
    ancestors: &mut Vec<String>,
    include: &[String],
    exclude: &[String],
) {
    if let Some(el) = ElementRef::wrap(node) {
        let name = el.value().name().to_ascii_lowercase();
        let mut keep = true;
        if exclude.iter().any(|t| t == &name) {
            keep = false;
        }
        if !include.is_empty() && !include.iter().any(|t| t == &name) {
            keep = false;
        }
        ancestors.push(name);
        if keep {
            serialise_open(el, out);
            for child in node.children() {
                walk_filter(child, out, ancestors, include, exclude);
            }
            out.push_str("</");
            out.push_str(el.value().name());
            out.push('>');
        } else {
            for child in node.children() {
                walk_filter(child, out, ancestors, include, exclude);
            }
        }
        ancestors.pop();
    } else {
        // text / doctype / comment
        let descendant_keep = ancestors.is_empty()
            || ancestors
                .iter()
                .all(|name| !exclude.iter().any(|t| t == name))
                && (include.is_empty()
                    || ancestors
                        .iter()
                        .any(|name| include.iter().any(|t| t == name)));
        if descendant_keep {
            match node.value() {
                scraper::node::Node::Text(t) => out.push_str(t),
                scraper::node::Node::Comment(c) => {
                    out.push_str("<!--");
                    out.push_str(c);
                    out.push_str("-->");
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_unwanted_removes_script_and_style() {
        let html = r#"<div><script>alert(1)</script><p>ok</p><style>p{}</style></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("<script"));
        assert!(!out.contains("<style"));
        assert!(out.contains("<p>ok</p>"));
    }

    #[test]
    fn strip_unwanted_removes_nav_footer() {
        let html = r#"<div><nav>x</nav><p>body</p><footer>y</footer></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("<nav"));
        assert!(!out.contains("<footer"));
        assert!(out.contains("<p>body</p>"));
    }

    #[test]
    fn extract_main_content_prefers_main_tag() {
        let html = r#"<html><body><nav>nav</nav><main><h1>Hi</h1><p>Content</p></main><footer>foot</footer></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("Hi"));
        assert!(main.contains("Content"));
    }

    #[test]
    fn extract_main_content_falls_back_to_body() {
        let html = r#"<html><body><p>only body</p></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("only body"));
    }

    #[test]
    fn filter_tags_excludes_named_tags() {
        let html = r#"<div><p>keep</p><nav>drop</nav><p>keep2</p></div>"#;
        let out = filter_tags(html, &[], &["nav".into()]);
        assert!(out.contains("keep"));
        assert!(out.contains("keep2"));
        assert!(!out.contains("drop"));
    }

    #[test]
    fn filter_tags_includes_only_named_tags() {
        let html = r#"<div><h1>t</h1><p>p</p><span>s</span></div>"#;
        let out = filter_tags(html, &["h1".into()], &[]);
        assert!(out.contains("<h1"));
        assert!(!out.contains("<p"));
        assert!(!out.contains("<span"));
    }

    #[test]
    fn filter_tags_no_filters_returns_original() {
        let html = "<p>x</p>";
        assert_eq!(filter_tags(html, &[], &[]), html);
    }
}
