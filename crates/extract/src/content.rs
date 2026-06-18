//! Content extraction — stripping boilerplate and isolating the main content
//! area before downstream markdown conversion.

use scraper::{element_ref::ElementRef, Html, Selector};

const UNWANTED_TAGS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "link", "meta", "title", "head", "nav",
    "header", "footer", "aside", "form", "button", "input", "select", "textarea", "object",
    "embed", "applet", "audio", "video", "source", "track", "canvas", "template", "slot",
];

/// Noise class/id substrings. An element is treated as boilerplate when the
/// concatenation of its class and id attributes (lower-cased) contains any of
/// these substrings.
const NOISE_SUBSTRINGS: &[&str] = &[
    "sidebar",
    "table-of-contents",
    "infobox",
    "navbox",
    "breadcrumb",
    "cookie",
    "consent",
    "banner",
    "disqus",
    "advert",
    "popup",
    "modal",
    "newsletter",
    "subscribe",
    "printfooter",
    "catlinks",
    "mw-panel",
    "mw-navigation",
    "sitesub",
    "jump-to-nav",
    "mw-editsection",
    "mw-jump-link",
    "mw-empty-elt",
    "mw-cite-backlink",
    "thumbcaption",
    "mw-metadata",
    "vertical-navbox",
    "reflist",
    "mw-references",
    "authority-control",
    "mw-indicators",
    "sistersitebox",
    "mbox",
    "ambox",
    "ombox",
    "hatnote",
    "shortdescription",
    "sphinxsidebar",
    "sphinxfooter",
    "copyright",
    "dropdown",
    "skip-to",
    "skip-link",
    "skiplinks",
    "promo",
    "promotional",
    "widget",
    "site-footer",
    "site-header",
    "page-footer",
    "page-header",
    "global-nav",
    "global-footer",
    "global-header",
    "main-nav",
    "primary-nav",
    "secondary-nav",
    "social-share",
    "social-links",
    "social-icons",
    "follow-us",
    "site-map",
    "sitemap",
    "references",
    "bibliography",
    "external-links",
    "further-reading",
    "see-also",
    "navigation",
    "edit-section",
    "collapsible",
    "collapsibletable",
    "metadata",
    "portal",
    "sister-project",
    "articlefooter",
    "pagefooter",
    "endnote",
    "citation",
];

/// Noise class/id tokens (whitespace-split). An element is treated as
/// boilerplate when any individual token in its class or id attribute equals
/// one of these (case-insensitive).
const NOISE_EXACT_TOKENS: &[&str] = &[
    "toc",
    "share",
    "social",
    "related",
    "recommended",
    "comments",
    "comment",
    "footer",
    "sidebar",
    "navbox",
    "infobox",
    "hatnote",
    "ambox",
    "ombox",
    "mbox",
    "mbox-small",
    "catlinks",
    "reference",
    "references",
    "reflist",
    "bibliography",
    "navigation",
    "advert",
    "advertisement",
    "promo",
    "sponsored",
    "mw-empty-elt",
    "mw-jump-link",
    "thumbcaption",
    "metadata",
];

/// Noise class/id prefixes. An element is treated as boilerplate when any
/// whitespace-split token in its class or id attribute starts with one of
/// these (case-insensitive).
const NOISE_PREFIXES: &[&str] = &["ad-", "ads-", "adv-", "sponsor"];

const CONTENT_SELECTORS: &[&str] = &[
    "#mw-content-text",
    ".mw-parser-output",
    "[data-testid=\"post-container\"]",
    ".Post",
    "shreddit-post",
    ".js-post-body",
    ".s-prose",
    ".main-page-content",
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

fn is_noise_element(el: &ElementRef<'_>) -> bool {
    let tag = el.value().name();
    // Headings are intentionally never treated as noise here. They may have
    // ids like "External_links" or "References" that overlap with our noise
    // substrings, but the headings themselves are part of the article
    // structure and are processed by `strip_wikipedia_tail` separately.
    if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        return false;
    }
    let tokens: Vec<String> = {
        let mut all: Vec<String> = Vec::new();
        if let Some(class) = el.value().attr("class") {
            all.extend(
                class
                    .split_whitespace()
                    .map(|t| t.to_ascii_lowercase().replace('_', "-")),
            );
        }
        if let Some(id) = el.value().attr("id") {
            all.extend(
                id.split_whitespace()
                    .map(|t| t.to_ascii_lowercase().replace('_', "-")),
            );
        }
        all
    };

    for token in &tokens {
        if NOISE_EXACT_TOKENS.contains(&token.as_str()) {
            return true;
        }
        for prefix in NOISE_PREFIXES {
            if token.starts_with(prefix) {
                return true;
            }
        }
    }

    let combined = tokens.join(" ");
    if NOISE_SUBSTRINGS
        .iter()
        .any(|needle| combined.contains(needle))
    {
        // Don't suppress the main content container itself just because its
        // outer wrapper happens to mention a noise word.
        if matches!(tag, "main" | "article") {
            return false;
        }
        if el
            .value()
            .attr("role")
            .is_some_and(|r| r.eq_ignore_ascii_case("main"))
        {
            return false;
        }
        return true;
    }

    false
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
        let is_tag_unwanted = UNWANTED_TAGS.contains(&tag)
            || is_hidden(&el)
            || is_data_uri_img
            || is_noise_element(&el);
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
///    comments, inline styles, noise-classed elements).
/// 2. Prefer structural landmarks (`<main>`, `<article>`, `[role=main]`,
///    `#content`, `.content`, etc).
/// 3. If a landmark selection still wraps more than 90% of the body text,
///    drill into a narrower content element.
/// 4. Fall back to the `<div>` with the highest content score.
/// 5. After each selection, run a Wikipedia-tail pass to remove everything
///    from the first "References"/"See also"/etc. heading onward.
pub fn extract_main_content(html: &str) -> String {
    let cleaned = strip_unwanted(html);
    let doc = Html::parse_document(&cleaned);

    for selector_str in CONTENT_SELECTORS {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = doc.select(&sel).next() {
                let content = if let Some(narrower) = drilldown_block(&doc, el) {
                    narrower.inner_html()
                } else {
                    el.inner_html()
                };
                // Truncate at the first "References"/"See also"/etc. heading
                // BEFORE running a second noise strip, otherwise the heading
                // itself (whose id contains words like "External_links") would
                // be removed and we'd lose the cut marker.
                let tailed = strip_wikipedia_tail(&content);
                return strip_unwanted(&tailed);
            }
        }
    }

    if let Some(block) = largest_text_block(&doc) {
        let tailed = strip_wikipedia_tail(&block.inner_html());
        return strip_unwanted(&tailed);
    }

    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            let tailed = strip_wikipedia_tail(&body.inner_html());
            return strip_unwanted(&tailed);
        }
    }

    strip_unwanted(&cleaned)
}

/// Headings that mark the end of an article's main content. The list is
/// matched against the heading's `id` (after lowercasing and converting
/// underscores to hyphens) as a substring, and against the heading's text
/// content (trimmed, lowercased) for an exact match.
const TAIL_HEADING_ID_SUBSTRINGS: &[&str] = &[
    "references",
    "see-also",
    "external-links",
    "further-reading",
    "notes",
    "bibliography",
    "cite-net",
];

const TAIL_HEADING_TEXT_EXACT: &[&str] = &[
    "references",
    "see also",
    "external links",
    "further reading",
    "notes",
    "bibliography",
];

/// Remove the Wikipedia "tail" — everything from the first
/// References/See also/External links/Further reading/Notes/Bibliography
/// heading onward — from an HTML fragment. This is safe to apply to any
/// page: non-Wikipedia pages will not contain an `<h2 id="References">` and
/// the function will return the input unchanged.
fn strip_wikipedia_tail(html: &str) -> String {
    if html.is_empty() {
        return html.to_string();
    }

    let doc = Html::parse_fragment(html);
    let root = doc.tree.root();

    // Walk the tree in document order. When we encounter a target heading
    // element, set the `cut` flag and skip every subsequent node (and its
    // descendants).
    let mut out = String::with_capacity(html.len());
    let mut cut = false;
    for child in root.children() {
        walk_with_cut(child, &mut out, &mut cut);
    }
    if out.is_empty() {
        return html.to_string();
    }
    out
}

fn walk_with_cut<'a>(
    node: ego_tree::NodeRef<'a, scraper::node::Node>,
    out: &mut String,
    cut: &mut bool,
) {
    if let Some(el) = ElementRef::wrap(node) {
        if is_tail_heading(&el) {
            *cut = true;
            return;
        }
        if !*cut {
            serialise_open(el, out);
        }
        for child in node.children() {
            walk_with_cut(child, out, cut);
        }
        if !*cut {
            out.push_str("</");
            out.push_str(el.value().name());
            out.push('>');
        }
    } else if !*cut {
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

fn is_tail_heading(el: &ElementRef<'_>) -> bool {
    let tag = el.value().name();
    if !matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        return false;
    }

    if let Some(id) = el.value().attr("id") {
        let normalized = id.to_ascii_lowercase().replace('_', "-");
        if TAIL_HEADING_ID_SUBSTRINGS
            .iter()
            .any(|needle| normalized.contains(needle))
        {
            return true;
        }
    }

    let text = el.text().collect::<String>().trim().to_ascii_lowercase();
    TAIL_HEADING_TEXT_EXACT.iter().any(|needle| text == *needle)
}

/// Drill-down selectors used when a landmark selection is still too broad
/// (e.g. Wikipedia's `<article>` wrapping references, navboxes, etc).
const DRILLDOWN_SELECTORS: &[&str] = &[
    "#mw-content-text",
    ".mw-parser-output",
    ".article-content",
    ".article-body",
    ".post-content",
    ".entry-content",
    ".page-content",
    ".story-body",
    ".main-page-content",
    "[data-testid=\"post-container\"]",
    "[data-testid=\"post-content\"]",
    "shreddit-post",
    ".Post",
];

fn drilldown_block<'a>(doc: &'a Html, parent: ElementRef<'a>) -> Option<ElementRef<'a>> {
    let body_sel = Selector::parse("body").ok()?;
    let body = doc.select(&body_sel).next()?;
    let body_text_len = text_length(body);
    if body_text_len == 0 {
        return None;
    }
    let parent_text_len = text_length(parent);
    if parent_text_len == 0 || (parent_text_len as f64 / body_text_len as f64) < 0.9 {
        return None;
    }

    let mut best: Option<(f64, ElementRef<'_>)> = None;
    for selector_str in DRILLDOWN_SELECTORS {
        let Ok(sel) = Selector::parse(selector_str) else {
            continue;
        };
        let candidate = match parent.select(&sel).next() {
            Some(el) => el,
            None => continue,
        };
        let candidate_len = text_length(candidate);
        if candidate_len == 0 {
            continue;
        }
        if (candidate_len as f64 / parent_text_len as f64) >= 0.85 {
            continue;
        }
        let html_len = candidate.inner_html().len();
        if html_len == 0 {
            continue;
        }
        let density = candidate_len as f64 / html_len as f64;
        let score = density * (candidate_len as f64).max(1.0).ln();
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, candidate));
        }
    }
    best.map(|(_, el)| el)
}

fn text_length(el: ElementRef<'_>) -> usize {
    el.text().collect::<String>().len()
}

fn largest_text_block(doc: &Html) -> Option<ElementRef<'_>> {
    const MIN_LEN: usize = 50;
    const DENSITY_MIN_LEN: usize = 100;

    let body_sel = Selector::parse("body").ok()?;
    let block_sel = Selector::parse("div, section, article, main").ok()?;
    let heading_sel = Selector::parse("h1, h2, h3, h4, h5, h6").ok()?;
    let paragraph_sel = Selector::parse("p").ok()?;
    let link_sel = Selector::parse("a").ok()?;

    let body = doc.select(&body_sel).next()?;

    let mut best: Option<(f64, ElementRef<'_>)> = None;

    for el in body.select(&block_sel) {
        let text_len = text_length(el);
        if text_len < DENSITY_MIN_LEN {
            continue;
        }

        let html_len = el.inner_html().len();
        if html_len == 0 {
            continue;
        }

        let density = text_len as f64 / html_len as f64;
        let heading_count = el.select(&heading_sel).count();
        let paragraph_count = el.select(&paragraph_sel).count();
        let total_link_text_len: usize = el.select(&link_sel).map(|a| text_length(a)).sum();
        let link_density = if text_len > 0 {
            total_link_text_len as f64 / text_len as f64
        } else {
            0.0
        };

        let mut score =
            text_len as f64 * density + heading_count as f64 * 50.0 + paragraph_count as f64 * 10.0
                - link_density * text_len as f64;

        let mut penalty_attr = String::new();
        if let Some(class) = el.value().attr("class") {
            penalty_attr.push(' ');
            penalty_attr.push_str(&class.to_ascii_lowercase());
        }
        if let Some(id) = el.value().attr("id") {
            penalty_attr.push(' ');
            penalty_attr.push_str(&id.to_ascii_lowercase());
        }
        let penalty_tokens = ["filter", "facet", "sidebar", "nav", "menu", "navigation"];
        if penalty_tokens
            .iter()
            .any(|t| penalty_attr.split_whitespace().any(|w| w == *t))
        {
            score -= text_len as f64 * 0.7;
        }

        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, el));
        }
    }

    if let Some((_, el)) = best {
        return Some(el);
    }

    let mut best_fallback: Option<(usize, ElementRef<'_>)> = None;
    for el in body.select(&block_sel) {
        let len = text_length(el);
        if len >= MIN_LEN && best_fallback.as_ref().is_none_or(|(b, _)| len > *b) {
            best_fallback = Some((len, el));
        }
    }

    if best_fallback.is_none() {
        let mut best_p: Option<(usize, ElementRef<'_>)> = None;
        for el in doc.select(&paragraph_sel) {
            let len = text_length(el);
            if len >= MIN_LEN && best_p.as_ref().is_none_or(|(b, _)| len > *b) {
                best_p = Some((len, el));
            }
        }
        best_fallback = best_p;
    }

    best_fallback.map(|(_, el)| el)
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
        assert!(
            !out.contains("data:image/"),
            "data URI image should be removed: {out}"
        );
        assert!(
            !out.contains("base64"),
            "base64 content should not appear: {out}"
        );
        assert!(
            out.contains("https://example.com/real.png"),
            "normal img should be kept: {out}"
        );
        assert!(out.contains("text"));
    }

    #[test]
    fn strip_unwanted_removes_sidebar_by_class() {
        let html =
            r#"<div><aside class="sidebar"><p>navigation links</p></aside><p>main</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("navigation links"),
            "sidebar text should be removed: {out}"
        );
        assert!(out.contains("main"));
    }

    #[test]
    fn strip_unwanted_removes_infobox_by_class() {
        let html = r#"<div><table class="infobox"><tr><td>info data</td></tr></table><p>article</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("info data"),
            "infobox content should be removed: {out}"
        );
        assert!(out.contains("article"));
    }

    #[test]
    fn strip_unwanted_removes_navbox_by_class() {
        let html = r#"<div><div class="navbox"><p>nav links</p></div><p>body</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("nav links"),
            "navbox should be removed: {out}"
        );
        assert!(out.contains("body"));
    }

    #[test]
    fn strip_unwanted_removes_references_by_id() {
        let html = r#"<div><ol class="references"><li>ref1</li></ol><p>body text</p></div>"#;
        let out = strip_unwanted(html);
        assert!(!out.contains("ref1"), "references should be removed: {out}");
        assert!(out.contains("body text"));
    }

    #[test]
    fn strip_unwanted_removes_advert_prefix() {
        let html = r#"<div><div class="ad-banner"><p>sponsored</p></div><div class="ad-wrapper"><p>ads</p></div><p>content</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("sponsored"),
            "ad-banner should be removed: {out}"
        );
        assert!(
            !out.contains(">ads<"),
            "ad-wrapper should be removed: {out}"
        );
        assert!(out.contains("content"));
    }

    #[test]
    fn strip_unwanted_removes_sponsored_prefix() {
        let html = r#"<div><div class="sponsored-content"><p>promo</p></div><p>real</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("promo"),
            "sponsored content should be removed: {out}"
        );
        assert!(out.contains("real"));
    }

    #[test]
    fn strip_unwanted_removes_cookie_banner() {
        let html = r#"<div><div id="cookie-banner"><p>Accept cookies</p></div><p>page</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("Accept cookies"),
            "cookie banner should be removed: {out}"
        );
        assert!(out.contains("page"));
    }

    #[test]
    fn strip_unwanted_removes_hatnote() {
        let html =
            r#"<div><div class="hatnote">see also other thing</div><p>main content</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("see also other thing"),
            "hatnote should be removed: {out}"
        );
        assert!(out.contains("main content"));
    }

    #[test]
    fn strip_unwanted_removes_share_token() {
        let html = r##"<div><div class="share"><a href="#">Tweet</a></div><p>content</p></div>"##;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("Tweet"),
            "share section should be removed: {out}"
        );
        assert!(out.contains("content"));
    }

    #[test]
    fn strip_unwanted_removes_comments_section() {
        let html = r#"<div><div class="comments"><p>user feedback</p></div><p>article</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("user feedback"),
            "comments should be removed: {out}"
        );
        assert!(out.contains("article"));
    }

    #[test]
    fn strip_unwanted_preserves_main_tag_even_with_noise_class() {
        let html = r#"<div><main class="main-content has-nav"><p>real content</p></main></div>"#;
        let out = strip_unwanted(html);
        assert!(out.contains("real content"));
    }

    #[test]
    fn strip_unwanted_preserves_article_even_with_noise_class() {
        let html =
            r#"<div><article class="article navigation-wrapper"><p>body</p></article></div>"#;
        let out = strip_unwanted(html);
        assert!(out.contains("body"));
    }

    #[test]
    fn strip_unwanted_removes_collapsible() {
        let html =
            r#"<div><div class="collapsible"><p>hidden content</p></div><p>visible</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("hidden content"),
            "collapsible should be removed: {out}"
        );
        assert!(out.contains("visible"));
    }

    #[test]
    fn strip_unwanted_removes_breadcrumbs() {
        let html = r#"<div><nav class="breadcrumbs"><a>Home</a> &gt; <a>Page</a></nav><p>content</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("Home"),
            "breadcrumbs should be removed: {out}"
        );
        assert!(out.contains("content"));
    }

    #[test]
    fn extract_main_content_drills_down_wikipedia_style() {
        let article_body =
            "<p>Real article content that is reasonably long for extraction to work properly.</p>"
                .repeat(5);
        let navbox = "<div class=\"navbox\"><p>navbox data</p></div>";
        let refs = "<ol class=\"references\"><li>ref 1</li></ol>";
        let html = format!(
            r#"<html><body><article><div id=\"mw-content-text\"><div class=\"mw-parser-output\">{article_body}{navbox}{refs}</div></div></article></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains("Real article content"));
        assert!(
            !main.contains("navbox data"),
            "navbox should be stripped: {main}"
        );
        assert!(
            !main.contains("ref 1"),
            "references should be stripped: {main}"
        );
    }

    #[test]
    fn extract_main_content_uses_mw_content_text_selector() {
        let html = r#"<html><body><div id="mw-content-text"><p>Wikipedia article text</p></div></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("Wikipedia article text"));
    }

    #[test]
    fn extract_main_content_uses_reddit_post_selector() {
        let html = r#"<html><body><div><shreddit-post><p>Reddit post content</p></shreddit-post></div></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("Reddit post content"));
    }

    #[test]
    fn extract_main_content_prefers_article_content_over_largest_block() {
        let long_padding = "lorem ipsum dolor sit amet ".repeat(30);
        let html = format!(
            r#"<html><body><div class="article-content"><p>real article body here</p></div><div class="comments-section"><p>{long_padding}</p></div></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains("real article body here"));
    }

    #[test]
    fn extract_main_content_penalises_sidebar_in_largest_block() {
        let sidebar_text = "nav link a nav link b nav link c nav link d ".repeat(20);
        let article_text = "Real article body content for testing. ".repeat(20);
        let html = format!(
            r#"<html><body><div class="sidebar"><p>{sidebar_text}</p></div><div class="article"><p>{article_text}</p></div></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains("Real article body content"));
    }

    #[test]
    fn extract_main_content_strips_noise_inside_main() {
        let html = r#"<html><body><main><h1>Title</h1><p>body</p><div class="advert">promo</div><div id="cookie-banner">accept</div></main></body></html>"#;
        let main = extract_main_content(html);
        assert!(main.contains("body"));
        assert!(
            !main.contains("promo"),
            "advert inside main should be stripped: {main}"
        );
        assert!(
            !main.contains("accept"),
            "cookie banner inside main should be stripped: {main}"
        );
    }

    #[test]
    fn test_underscore_id_normalized_to_hyphen() {
        let html = r#"<div><div id="External_links"><p>noise</p></div><p>body</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("noise"),
            "id with underscores should be matched as noise: {out}"
        );
        assert!(out.contains("body"));
    }

    #[test]
    fn test_underscore_class_normalized_to_hyphen() {
        let html = r#"<div><div class="external_links"><p>noise</p></div><p>body</p></div>"#;
        let out = strip_unwanted(html);
        assert!(
            !out.contains("noise"),
            "class with underscores should be matched as noise: {out}"
        );
        assert!(out.contains("body"));
    }

    #[test]
    fn strip_wikipedia_tail_removes_references_section() {
        let html = r#"<div><p>Article body.</p><h2>References</h2><ol><li>ref1</li><li>ref2</li></ol></div>"#;
        let out = strip_wikipedia_tail(html);
        assert!(out.contains("Article body."));
        assert!(
            !out.contains("ref1"),
            "references section should be removed: {out}"
        );
        assert!(
            !out.contains("ref2"),
            "references section should be removed: {out}"
        );
        assert!(
            !out.contains("References"),
            "References heading itself should be removed: {out}"
        );
    }

    #[test]
    fn strip_wikipedia_tail_removes_see_also_section() {
        let html = r#"<div><p>Article body.</p><h2>See also</h2><ul><li>Other</li></ul></div>"#;
        let out = strip_wikipedia_tail(html);
        assert!(out.contains("Article body."));
        assert!(
            !out.contains("Other"),
            "see also section should be removed: {out}"
        );
        assert!(
            !out.contains("See also"),
            "See also heading should be removed: {out}"
        );
    }

    #[test]
    fn strip_wikipedia_tail_preserves_content_without_tail_sections() {
        let html = r#"<div><h1>Title</h1><p>First paragraph.</p><h2>Section A</h2><p>Content A.</p><h2>Section B</h2><p>Content B.</p></div>"#;
        let out = strip_wikipedia_tail(html);
        assert!(out.contains("First paragraph."));
        assert!(out.contains("Content A."));
        assert!(out.contains("Content B."));
        assert!(out.contains("Title"));
    }

    #[test]
    fn strip_wikipedia_tail_uses_heading_id() {
        let html = r#"<div><p>Article body.</p><h2 id="References">Bibliography</h2><ol><li>ref1</li></ol></div>"#;
        let out = strip_wikipedia_tail(html);
        assert!(out.contains("Article body."));
        assert!(
            !out.contains("ref1"),
            "section after References-id heading should be removed: {out}"
        );
        assert!(
            !out.contains("Bibliography"),
            "heading after References id should be removed: {out}"
        );
    }

    #[test]
    fn strip_wikipedia_tail_uses_heading_id_with_underscores() {
        let html = r#"<div><p>Article body.</p><h2 id="Further_reading">Further reading</h2><ul><li>book</li></ul></div>"#;
        let out = strip_wikipedia_tail(html);
        assert!(out.contains("Article body."));
        assert!(
            !out.contains("book"),
            "section after Further_reading id should be removed: {out}"
        );
    }

    #[test]
    fn extract_main_content_strips_wikipedia_references() {
        let article_body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20);
        let html = format!(
            r#"<html><body><div id="mw-content-text"><div class="mw-parser-output"><p>{article_body}</p><h2 id="References">References</h2><ol class="references"><li>ref1</li><li>ref2</li><li>ref3</li></ol></div></div></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains("Lorem ipsum"));
        assert!(
            !main.contains("ref1"),
            "references list should be removed: {main}"
        );
        assert!(
            !main.contains("ref2"),
            "references list should be removed: {main}"
        );
        assert!(
            !main.contains("ref3"),
            "references list should be removed: {main}"
        );
    }

    #[test]
    fn extract_main_content_strips_wikipedia_external_links() {
        let article_body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20);
        let html = format!(
            r#"<html><body><div id="mw-content-text"><div class="mw-parser-output"><p>{article_body}</p><h2 id="External_links">External links</h2><ul><li><a href="https://example.com">example</a></li></ul></div></div></body></html>"#
        );
        let main = extract_main_content(&html);
        assert!(main.contains("Lorem ipsum"));
        assert!(
            !main.contains("example.com"),
            "External links section should be removed: {main}"
        );
        assert!(
            !main.contains("example"),
            "External links list should be removed: {main}"
        );
    }
}
