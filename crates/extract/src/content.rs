//! Content extraction — stripping boilerplate and isolating the main content
//! area before downstream markdown conversion.

use scraper::{element_ref::ElementRef, Html, Selector};

const UNWANTED_TAGS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "link", "meta", "title", "head", "nav",
    "header", "footer", "aside", "form", "button", "input", "select", "textarea", "object",
    "embed", "applet", "audio", "video", "source", "track", "canvas", "template", "slot",
];

const CONTENT_SELECTORS: &[&str] = &[
    "main",
    "article",
    "[role=\"main\"]",
    "[role=\"article\"]",
    "#content",
    "#main",
    "#main-content",
    "#article",
    "#article-body",
    "#story",
    "#story-body",
    ".content",
    ".main",
    ".main-content",
    ".article",
    ".article-body",
    ".article-content",
    ".post",
    ".post-content",
    ".entry-content",
    ".page-content",
    ".story",
    ".story-body",
    "[itemprop=\"articleBody\"]",
];

/// Walk the tree and serialise it manually, skipping any element whose tag
/// (or any ancestor's tag) matches one of the unwanted selectors. Also
/// removes comments, hidden elements, and inline `style=` attributes.
pub fn strip_unwanted(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let mut out = String::with_capacity(html.len());
    let root = doc.tree.root();
    for child in root.children() {
        walk_and_serialise(child, &mut out, &mut Vec::new());
    }
    out
}

fn is_hidden(el: &ElementRef<'_>) -> bool {
    if let Some(class) = el.value().attr("class") {
        let lc = class.to_ascii_lowercase();
        let mut tokens = lc.split_whitespace();
        let hidden_markers = [
            "hidden",
            "display-none",
            "d-none",
            "invisible",
            "visually-hidden",
            "sr-only",
            "offscreen",
            "screen-reader",
            "collapse",
            "collapsed",
            "is-hidden",
            "js-hidden",
            "no-display",
            "no-print",
        ];
        if tokens.any(|t| hidden_markers.contains(&t)) {
            return true;
        }
    }
    if el
        .value()
        .attr("aria-hidden")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(style) = el.value().attr("style") {
        let s = style.to_ascii_lowercase();
        let compact: String = s.split_whitespace().collect::<Vec<_>>().join("");
        if compact.contains("display:none")
            || compact.contains("display:none;")
            || compact.contains("visibility:hidden")
            || compact.contains("visibility:hidden;")
            || compact.contains("visibility:collapse")
        {
            return true;
        }
    }
    if el.value().attr("hidden").is_some() {
        return true;
    }
    if el
        .value()
        .attr("data-state")
        .map(|v| v.eq_ignore_ascii_case("hidden") || v.eq_ignore_ascii_case("collapsed"))
        .unwrap_or(false)
    {
        return true;
    }
    false
}

fn walk_and_serialise<'a>(
    node: ego_tree::NodeRef<'a, scraper::node::Node>,
    out: &mut String,
    ancestor_unwanted: &mut Vec<bool>,
) {
    if let Some(el) = ElementRef::wrap(node) {
        let tag = el.value().name();
        let is_data_uri_img = tag == "img"
            && el
                .value()
                .attr("src")
                .is_some_and(|s| s.starts_with("data:image/"));
        let is_tag_unwanted =
            UNWANTED_TAGS.contains(&tag) || is_hidden(&el) || is_data_uri_img;
        ancestor_unwanted.push(is_tag_unwanted);
        if !is_tag_unwanted {
            serialise_open(el, out);
        }
        for child in node.children() {
            walk_and_serialise(child, out, ancestor_unwanted);
        }
        if !is_tag_unwanted {
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        ancestor_unwanted.pop();
    } else if !ancestor_unwanted.iter().any(|b| *b) {
        match node.value() {
            scraper::node::Node::Text(t) => out.push_str(t),
            scraper::node::Node::Comment(_) => {}
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
        if k.eq_ignore_ascii_case("style") {
            continue;
        }
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
    let _ = el;
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

/// Trimmed content extraction — returns the inner HTML of the main content
/// area of the page with boilerplate removed.
///
/// Strategy:
/// 1. Strip all boilerplate (scripts, styles, nav, hidden elements,
///    comments, inline styles).
/// 2. Prefer structural landmarks (`<main>`, `<article>`, `[role=main]`,
///    `#content`, `.content`, etc).
/// 3. Fall back to the `<div>` with the largest amount of text content.
pub fn extract_main_content(html: &str) -> String {
    let cleaned = strip_unwanted(html);
    let doc = Html::parse_document(&cleaned);

    for selector_str in CONTENT_SELECTORS {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = doc.select(&sel).next() {
                return el.inner_html();
            }
        }
    }

    if let Some(block) = largest_text_block(&doc) {
        return block.inner_html();
    }

    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            return body.inner_html();
        }
    }

    cleaned
}

fn text_length(el: ElementRef<'_>) -> usize {
    el.text().collect::<String>().len()
}

fn largest_text_block(doc: &Html) -> Option<ElementRef<'_>> {
    const MIN_LEN: usize = 100;
    const DENSITY_MIN_LEN: usize = 200;

    let body_sel = Selector::parse("body").ok()?;
    let block_sel = Selector::parse("div, section").ok()?;

    let body = doc.select(&body_sel).next()?;

    let mut best_density: Option<(f64, ElementRef<'_>)> = None;
    for el in body.select(&block_sel) {
        let text_len = text_length(el);
        if text_len > DENSITY_MIN_LEN {
            let html_len = el.inner_html().len();
            if html_len > 0 {
                let density = text_len as f64 / html_len as f64;
                if best_density.as_ref().is_none_or(|(b, _)| density > *b) {
                    best_density = Some((density, el));
                }
            }
        }
    }

    if let Some((_, el)) = best_density {
        return Some(el);
    }

    let mut best: Option<(usize, ElementRef<'_>)> = None;
    for el in body.select(&block_sel) {
        let len = text_length(el);
        if len >= MIN_LEN && best.as_ref().is_none_or(|(b, _)| len > *b) {
            best = Some((len, el));
        }
    }

    if best.is_none() {
        let p_sel = Selector::parse("p").ok()?;
        let mut best_p: Option<(usize, ElementRef<'_>)> = None;
        for el in doc.select(&p_sel) {
            let len = text_length(el);
            if len >= MIN_LEN && best_p.as_ref().is_none_or(|(b, _)| len > *b) {
                best_p = Some((len, el));
            }
        }
        best = best_p;
    }

    best.map(|(_, el)| el)
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
                scraper::node::Node::Comment(_) => {}
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
    fn strip_unwanted_removes_comments() {
        let html = "<div><!-- a comment --><p>ok</p><!-- another --></div>";
        let out = strip_unwanted(html);
        assert!(!out.contains("<!--"));
        assert!(!out.contains("comment"));
        assert!(out.contains("<p>ok</p>"));
    }

    #[test]
    fn strip_unwanted_removes_hidden() {
        let html = r#"<div><p style="display:none">hidden</p><p>visible</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("hidden"));
        assert!(out.contains("visible"));
    }

    #[test]
    fn strip_unwanted_removes_aria_hidden() {
        let html = r#"<div><div aria-hidden="true"><p>hidden</p></div><p>visible</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("hidden"));
        assert!(out.contains("visible"));
    }

    #[test]
    fn strip_unwanted_strips_inline_styles() {
        let html = r#"<p style="color:red">hi</p>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("style="));
        assert!(out.contains("hi"));
    }

    #[test]
    fn extract_main_content_prefers_main_tag() {
        let html = r#"<html><body><nav>nav</nav><main><h1>Hi</h1><p>Content</p></main><footer>foot</footer></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("Hi"));
        assert!(main.contains("Content"));
        assert!(!main.contains("nav</nav>"));
        assert!(!main.contains("foot"));
    }

    #[test]
    fn extract_main_content_falls_back_to_body() {
        let html = r#"<html><body><p>only body</p></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("only body"));
    }

    #[test]
    fn extract_main_content_uses_article() {
        let html =
            r#"<html><body><article><h1>Title</h1><p>Article text</p></article></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("Article text"));
    }

    #[test]
    fn extract_main_content_strips_scripts_in_kept_section() {
        let html = r#"<html><body><main><script>alert(1)</script><p>real</p></main></body></html>"#;
        let main = extract_main_content(html);
        assert!(!main.contains("<script"));
        assert!(!main.contains("alert"));
        assert!(main.contains("real"));
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

    #[test]
    fn strip_unwanted_removes_head_element() {
        let html = r#"<html><head><title>Title</title><style>body{}</style></head><body><p>hi</p></body></html>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("<head"));
        assert!(!out.contains("<title"));
        assert!(!out.contains("<style"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn strip_unwanted_removes_header_and_aside() {
        let html = r#"<div><header>head</header><aside>side</aside><p>body</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("<header"));
        assert!(!out.contains("<aside"));
        assert!(out.contains("<p>body</p>"));
    }

    #[test]
    fn strip_unwanted_removes_noscript_and_svg_with_text() {
        let html =
            r#"<div><noscript>Enable JS</noscript><svg><text>SVG text</text></svg><p>ok</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("<noscript"));
        assert!(!out.contains("<svg"));
        assert!(!out.contains("Enable JS"));
        assert!(!out.contains("SVG text"));
        assert!(out.contains("<p>ok</p>"));
    }

    #[test]
    fn strip_unwanted_detects_hidden_class() {
        let html =
            r#"<div><p class="hidden">x</p><p class="d-none">y</p><p class="show">z</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains(">x<"));
        assert!(!out.contains(">y<"));
        assert!(out.contains("z"));
    }

    #[test]
    fn strip_unwanted_detects_visibility_collapse() {
        let html = r#"<div><p style="visibility: collapse">gone</p><p>here</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("gone"));
        assert!(out.contains("here"));
    }

    #[test]
    fn strip_unwanted_detects_display_none_with_spaces() {
        let html = r#"<div><p style="display:  none  !important">x</p><p>y</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains(">x<"));
        assert!(out.contains(">y<"));
    }

    #[test]
    fn extract_main_content_uses_role_main() {
        let html = r#"<html><body><div role="main"><p>role-main body</p></div></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("role-main body"));
    }

    #[test]
    fn extract_main_content_uses_id_content() {
        let html = r#"<html><body><div id="content"><p>id-content body</p></div></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("id-content body"));
    }

    #[test]
    fn extract_main_content_uses_class_content() {
        let html =
            r#"<html><body><div class="content"><p>class-content body</p></div></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("class-content body"));
    }

    #[test]
    fn extract_main_content_falls_back_to_largest_div() {
        let long = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(10);
        let html = format!(
            r#"<html><body><div class="sidebar"><p>short</p></div><div class="article"><p>{long}</p></div></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains(&long));
    }

    #[test]
    fn extract_main_content_strips_inline_styles_in_kept_section() {
        let html = r#"<html><body><main><p style="color:red">kept</p></main></body></html>"#;
        let main = extract_main_content(html);
        assert!(!main.contains("color:red"));
        assert!(main.contains("kept"));
    }

    #[test]
    fn strip_unwanted_removes_data_uri_images() {
        let html = r#"<div><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" alt="big" /><p>text</p><img src="https://example.com/real.png" alt="ok" /></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("data:image/"), "data URI image should be removed: {out}");
        assert!(!out.contains("base64"), "base64 content should not appear: {out}");
        assert!(out.contains("https://example.com/real.png"), "normal img should be kept: {out}");
        assert!(out.contains("text"));
    }
}
