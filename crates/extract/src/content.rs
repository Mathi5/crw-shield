//! Content extraction — stripping boilerplate and isolating the main content
//! area before downstream markdown conversion.

#[cfg(test)]
use crw_antibot::situation::diagnose;
use crw_antibot::situation::{SituationKind, SituationReport};
use scraper::{element_ref::ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};

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

/// Coarse classification of the input HTML. Drives scoring weights and
/// fallback decisions in [`extract_main_content_v2`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageType {
    /// News article, blog post, Wikipedia entry — long-form prose with one
    /// dominant `<article>` or `<main>` element.
    Article,
    /// E-commerce product page — title, price, description, specs, reviews.
    Product,
    /// Index / catalog / search results — many short items, no dominant prose.
    Listing,
    /// Forum thread / Q&A — question at top, answers below, votes, comments.
    Forum,
    /// API / technical documentation — heavy code blocks, table of contents.
    Doc,
    /// Phase D: homepage-style aggregator (added to match Firecrawl's
    /// `Collection` variant). Falls back to v3 noise filtering like `Listing`.
    Collection,
    /// Phase D: marketing, pricing, contact, terms-of-service pages
    /// (added to match Firecrawl's `Service` variant). These are usually
    /// mostly boilerplate; v3 returns whatever article-like content exists.
    Service,
    /// Cannot be classified confidently.
    #[default]
    Unknown,
}

/// Result of `extract_main_content_v2`. `markdown` is the actual content,
/// `quality` is a 0.0..=1.0 confidence score (drives the caller decision:
/// escalate to CDP/FlareSolverr if low), `page_type` is the coarse
/// classification, `used_fallback` is true when the permissive path was
/// taken because the strict path returned too little.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    pub markdown: String,
    pub quality: f32,
    pub page_type: PageType,
    pub used_fallback: bool,
}

impl ExtractionResult {
    /// True when the extraction looks usable: quality above 0.3 and a
    /// meaningful amount of text.
    pub fn is_usable(&self) -> bool {
        self.quality >= 0.3 && self.markdown.len() >= 200
    }
}

/// Phase C.1: extended extraction result that also carries the
/// "extraction reason" — a short string explaining *why* the quality is
/// what it is. Surfaced in the API response so operators can debug
/// "why did crw-shield return such low quality for this page?" without
/// having to re-run the classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractionReason {
    /// Strict selection returned a high-quality candidate.
    Strict,
    /// Largest-text-block heuristic returned a usable candidate.
    Heuristic,
    /// Permissive fallback used because nothing better was found.
    /// This is normal for SPAs and JS-only pages.
    Fallback,
    /// Phase C: the situation detector flagged this as `SoftNotFound`
    /// (real 404). We return the body unchanged so the caller can
    /// surface the error to its own user.
    SoftNotFound,
    /// Phase C: the situation detector flagged this as `JsOnly` and
    /// no JS-rendered DOM was available. The body is the empty SPA
    /// shell; caller should escalate to CDP.
    JsOnly,
    /// Phase C: anti-bot block detected. Quality is meaningless until
    /// the challenge is solved; caller should escalate.
    AntiBotBlock,
    /// Page is empty / too small to be useful.
    Empty,
}

#[derive(Debug, Clone)]
pub struct ExtractionResultWithReason {
    pub result: ExtractionResult,
    pub reason: ExtractionReason,
    /// Convenience: the `SituationKind` that influenced the result, if
    /// any. None when called without a situation.
    pub situation_kind: Option<String>,
}

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

// =========================================================================
// extract_main_content_v2 — quality scoring + automatic permissive fallback
// =========================================================================
//
// Behaviour mirrors Firecrawl's "If onlyMainContent results in empty markdown,
// retries with onlyMainContent: false". We compute a quality score for each
// candidate extraction; if no candidate clears the threshold we automatically
// fall back to a permissive path (just `strip_unwanted` of `<body>`) and
// surface that to the caller via `used_fallback: true`.

/// Quality threshold for accepting a strict candidate. Tuned conservatively.
const QUALITY_THRESHOLD: f32 = 0.3;
/// Slightly lower threshold for the `largest_text_block` fallback path.
const QUALITY_THRESHOLD_FALLBACK: f32 = 0.2;
/// Minimum visible-text length (chars) to consider an extraction non-empty.
const MIN_TEXT_LEN: usize = 200;

/// Extract main content with quality scoring and automatic fallback.
///
/// Pipeline:
/// 1. **Pre-clean**: run `strip_unwanted` to drop scripts/styles/navs/footers.
/// 2. **Classify**: pick a `PageType` (Article, Product, Listing, Forum, Doc,
///    Unknown) based on structural signals.
/// 3. **Score + select**: try CONTENT_SELECTORS in order. For each candidate,
///    compute a quality score based on text density, link density, and
///    structural bonuses. Use the first candidate whose score is above
///    `QUALITY_THRESHOLD` and whose visible text is at least `MIN_TEXT_LEN`.
/// 4. **Fallback**: if no candidate passes, retry in permissive mode (just
///    `strip_unwanted` of `<body>`). Mark `used_fallback = true` and assign a
///    low quality score (0.05..=0.2) so callers know to escalate.
/// 5. **Wikipedia tail cut**: drop References/See also/External links tail.
///
/// Returns an [`ExtractionResult`] with the cleaned HTML, the confidence
/// score, the classified page type, and whether the permissive fallback was
/// used.
pub fn extract_main_content_v2(html: &str) -> ExtractionResult {
    let cleaned = strip_unwanted(html);
    let doc = Html::parse_document(&cleaned);

    let page_type = classify_page_type(&doc);

    // Phase 1 — strict selection.
    for selector_str in CONTENT_SELECTORS {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = doc.select(&sel).next() {
                let content = if let Some(narrower) = drilldown_block(&doc, el) {
                    narrower.inner_html()
                } else {
                    el.inner_html()
                };
                let tailed = strip_wikipedia_tail(&content);
                let candidate = strip_unwanted(&tailed);
                let text_len = visible_text_len(&candidate);
                let quality = score_quality(&candidate, text_len, page_type);

                if quality >= QUALITY_THRESHOLD && text_len >= MIN_TEXT_LEN {
                    return ExtractionResult {
                        markdown: candidate,
                        quality,
                        page_type,
                        used_fallback: false,
                    };
                }
            }
        }
    }

    // Phase 2 — largest-text-block heuristic.
    if let Some(block) = largest_text_block(&doc) {
        let tailed = strip_wikipedia_tail(&block.inner_html());
        let candidate = strip_unwanted(&tailed);
        let text_len = visible_text_len(&candidate);
        let quality = score_quality(&candidate, text_len, page_type);

        if quality >= QUALITY_THRESHOLD_FALLBACK && text_len >= MIN_TEXT_LEN {
            return ExtractionResult {
                markdown: candidate,
                quality,
                page_type,
                used_fallback: false,
            };
        }
    }

    // Phase 3 — permissive fallback: strip and return. Quality is low by
    // design — this path is hit when the page is JS-only or has no clear
    // main content area. The caller can decide to escalate.
    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            let tailed = strip_wikipedia_tail(&body.inner_html());
            let candidate = strip_unwanted(&tailed);
            let text_len = visible_text_len(&candidate);
            let quality = if text_len >= MIN_TEXT_LEN { 0.2 } else { 0.05 };
            return ExtractionResult {
                markdown: candidate,
                quality,
                page_type,
                used_fallback: true,
            };
        }
    }

    // Phase 4 — absolute last resort.
    ExtractionResult {
        markdown: cleaned,
        quality: 0.0,
        page_type,
        used_fallback: true,
    }
}

// =========================================================================
// extract_main_content_v3 — Phase C: situation-aware extraction
// =========================================================================
//
// This is the *recommended* extraction entry point. It takes the
// `SituationReport` produced by the antibot detector (in addition to the
// raw HTML) and uses it to:
//
//   1. Short-circuit on `SoftNotFound`: the page is a real 404, no
//      amount of re-extraction will help. Return a low-quality result
//      with `reason = SoftNotFound` so the caller can surface the
//      original HTTP 404 to its own user without confusing them with
//      a "your extractor is bad" diagnosis.
//
//   2. Short-circuit on `JsOnly`: the HTTP fetcher returned a SPA
//      shell. The extraction *will* look like it found something
//      because there's a `<div id="root">` with a few scripts — but
//      the real content was never rendered. Force `quality = 0.1` and
//      `reason = JsOnly` so the caller knows to escalate to CDP.
//
//   3. Anti-bot blocks: when the report is an anti-bot provider
//      (Cloudflare, DataDome, ...) the extracted "content" is actually
//      the challenge page. Mark `reason = AntiBotBlock`.
//
//   4. Otherwise: behave exactly like v2. The reason field just
//      surfaces which of the v2 phases actually won.
//
// The result is wrapped in `ExtractionResultWithReason` for the API
// response. The legacy `extract_main_content_v2` keeps its signature
// so existing callers (tests, scripts) still work.
pub fn extract_main_content_v3(
    html: &str,
    situation: Option<&SituationReport>,
) -> ExtractionResultWithReason {
    use crate::content::situation_aware_decision;
    let base = extract_main_content_v2(html);
    let decision = situation_aware_decision(situation, html, &base);
    ExtractionResultWithReason {
        reason: decision.reason,
        situation_kind: decision.situation_kind,
        result: ExtractionResult {
            quality: decision.quality_override.unwrap_or(base.quality),
            ..base
        },
    }
}

/// Phase D: page-type-aware extraction router. v4 consults the *current*
/// page-type classification (from v3's preliminary pass), then delegates
/// to the best extractor for the job:
///
/// - `Article` / `Doc` → `firecrawl/html-extractor` (5-stage trafilatura-like
///   pipeline, page-type-aware scoring weights). Best at long-form prose
///   and technical documentation.
/// - `Product` / `Listing` / `Collection` / `Forum` / `Service` / `Unknown`
///   → `extract_main_content_v3` (our existing path). Better at e-commerce
///   noise filtering (the 22 `NOISE_SUBSTRINGS` heuristics) and forum
///   threads where antibot situation-awareness matters.
///
/// Returns the same `ExtractionResultWithReason` shape as v3 so the
/// `handlers.rs` call site and the public `extraction_quality` API field
/// don't change. The `reason` field is computed from the Firecrawl
/// quality score when the Firecrawl path is taken; from the v3 decision
/// otherwise.
///
/// IMPORTANT: the Firecrawl path is feature-gated on `firecrawl-extractor`.
/// When the feature is off, v4 behaves identically to v3 (no behavior
/// change, no overhead).
///
/// Note: the `url` argument is forwarded to the Firecrawl extractor for
/// relative→absolute link rewriting and scoring position hints. Without
/// it, the upstream still works (relative URLs stay as-is in markdown),
/// but link rewriting and some page-type signals are degraded.
pub fn extract_main_content_v4(
    html: &str,
    situation: Option<&SituationReport>,
    // `url` is only forwarded to the Firecrawl extractor. When the
    // `firecrawl-extractor` feature is off, the parameter is unused —
    // `#[allow(unused_variables)]` keeps the signature stable across
    // feature builds so call sites don't need to be `#[cfg]`-gated.
    #[allow(unused_variables)] url: &str,
) -> ExtractionResultWithReason {
    // Cheap pre-classification on the current v3 result. We reuse v3's
    // page-type detection so we don't double-parse the HTML for the common
    // case (Article/Doc) — v2 already scored it.
    let preliminary = extract_main_content_v3(html, situation);
    // `use_firecrawl` is the gate for the feature-gated delegation below.
    // Without the feature it's unused, hence the `#[allow]`.
    #[allow(unused_variables)]
    let use_firecrawl = matches!(
        preliminary.result.page_type,
        PageType::Article | PageType::Doc
    );

    // Feature-gated Firecrawl delegation. When the feature is on AND the
    // page-type is Article/Doc, we re-extract with the upstream pipeline
    // and merge the result. The v3 quality override is preserved so the
    // antibot situation-awareness still wins on real 404 / JS-only pages.
    #[cfg(feature = "firecrawl-extractor")]
    {
        if use_firecrawl {
            if let Some(fe_res) = crate::firecrawl_compat::extract_with_firecrawl(html, url) {
                let reason = crate::firecrawl_compat::reason_from_quality(fe_res.quality);
                return ExtractionResultWithReason {
                    reason,
                    situation_kind: preliminary.situation_kind,
                    result: ExtractionResult {
                        markdown: fe_res.markdown,
                        quality: fe_res.quality,
                        page_type: fe_res.page_type,
                        // Firecrawl's "used_fallback" is more nuanced than our
                        // v3 boolean (it tracks which of the 5 stages won).
                        // We expose it transparently so the API can surface
                        // it for debugging.
                        used_fallback: fe_res.used_fallback,
                    },
                };
            }
            // Fall through to v3 if Firecrawl returns None (empty input
            // edge case). v3 will give us a sensible result for thin pages.
        }
    }
    // Default path: v3 (covers Product/Listing/Forum/Unknown + the
    // no-feature build).
    preliminary
}

/// Decision produced by looking at the situation report. Cheap to
/// construct: no HTML parsing, no token scan. The caller applies the
/// decision to the v2 result.
#[derive(Debug, Clone)]
pub struct SituationDecision {
    pub reason: ExtractionReason,
    /// If set, override the v2 quality score with this value.
    pub quality_override: Option<f32>,
    /// Convenience: the situation kind that drove the decision.
    pub situation_kind: Option<String>,
}

/// Pure function: given a situation report (and the raw HTML for size
/// hints, plus the v2 baseline result), decide how to label and possibly
/// re-score the extraction.
///
/// **Important**: the situation *never* downgrades a high-quality v2
/// result. If v2 found 50k characters of real content, the fact that
/// the request went through Cloudflare's CDN does not turn that into
/// an "anti-bot block". A real anti-bot block is one where the body is
/// the challenge page itself (small, full of "checking your browser"),
/// not one where the body is the article you wanted to read.
///
/// The decision is:
///   1. If the situation is `CleanSuccess` (or no situation was
///      provided), pass through v2 untouched.
///   2. If v2 already produced a low-quality result (quality < 0.3 OR
///      markdown < 500 chars), trust the situation: tag the result
///      with the situation-derived reason and override the quality to
///      the appropriate low value.
///   3. If v2 produced a high-quality result but the situation
///      indicates a problem, **surface the situation in `reason` for
///      operator visibility** but do NOT override the quality. The
///      v2 score reflects what we actually extracted.
pub fn situation_aware_decision(
    situation: Option<&SituationReport>,
    html: &str,
    base: &ExtractionResult,
) -> SituationDecision {
    let high_quality = base.quality >= 0.3 && base.markdown.len() >= 500;

    let Some(rep) = situation else {
        // No situation: legacy behaviour, no override. The reason is
        // left at `Strict` (or whatever the caller sets); the v2 path
        // already produced the right value.
        return SituationDecision {
            reason: ExtractionReason::Strict,
            quality_override: None,
            situation_kind: None,
        };
    };
    let kind = rep.kind;
    let kind_str = Some(kind.as_str().to_string());

    if high_quality {
        // v2 found real content. The situation might still be useful
        // for the operator (e.g. "this page went through Cloudflare's
        // CDN") but it must not poison the quality score. We tag the
        // reason for visibility but skip the override.
        return SituationDecision {
            reason: ExtractionReason::Strict,
            quality_override: None,
            situation_kind: kind_str,
        };
    }

    // v2 produced a low-quality result. Trust the situation to label
    // it correctly.
    if matches!(kind, SituationKind::SoftNotFound) {
        return SituationDecision {
            reason: ExtractionReason::SoftNotFound,
            quality_override: Some(0.1),
            situation_kind: kind_str,
        };
    }
    if matches!(kind, SituationKind::JsOnly) {
        return SituationDecision {
            reason: ExtractionReason::JsOnly,
            quality_override: Some(0.1),
            situation_kind: kind_str,
        };
    }
    if kind.is_anti_bot() {
        return SituationDecision {
            reason: ExtractionReason::AntiBotBlock,
            quality_override: Some(0.05),
            situation_kind: kind_str,
        };
    }
    if matches!(kind, SituationKind::GeoBlocked | SituationKind::LoginWall) {
        return SituationDecision {
            reason: ExtractionReason::AntiBotBlock,
            quality_override: Some(0.1),
            situation_kind: kind_str,
        };
    }
    if matches!(
        kind,
        SituationKind::RateLimited | SituationKind::ServerError
    ) {
        return SituationDecision {
            reason: ExtractionReason::AntiBotBlock,
            quality_override: Some(0.05),
            situation_kind: kind_str,
        };
    }
    // Clean success / unknown / anything we didn't classify above.
    // The v2 path already produced the right value; just pass it
    // through with the right reason.
    if html.trim().len() < 200 {
        return SituationDecision {
            reason: ExtractionReason::Empty,
            quality_override: Some(0.05),
            situation_kind: kind_str,
        };
    }
    SituationDecision {
        reason: ExtractionReason::Strict,
        quality_override: None,
        situation_kind: kind_str,
    }
}

/// Heuristic page-type classification. Looks at the structural signals in
/// the document and picks the most likely type. Cheap, runs in O(n).
fn classify_page_type(doc: &Html) -> PageType {
    let count_sel = |sel_str: &str| -> usize {
        Selector::parse(sel_str)
            .map(|s| doc.select(&s).count())
            .unwrap_or(0)
    };
    let article_count = count_sel("article");
    let h1_count = count_sel("h1");
    let table_count = count_sel("table");
    let pre_count = count_sel("pre, code");
    let ul_ol_count = count_sel("ul, ol");
    let li_count = count_sel("li");

    // Doc: heavy code/table presence.
    if pre_count >= 5 || table_count >= 3 {
        return PageType::Doc;
    }
    // Forum/QA: many list items under article.
    if article_count >= 1 && li_count >= 10 {
        return PageType::Forum;
    }
    // Listing: many list items, no dominant article.
    if li_count >= 20 && ul_ol_count >= 3 {
        return PageType::Listing;
    }
    // Product: typical product page has price/cart markers.
    let has_price_marker = doc.tree.root().descendants().any(|n| {
        n.value()
            .as_element()
            .map(|e| {
                let cls = e.attr("class").unwrap_or("");
                let id = e.attr("id").unwrap_or("");
                let combined = format!("{cls} {id}").to_lowercase();
                combined.contains("price")
                    || combined.contains("product")
                    || combined.contains("buy")
                    || combined.contains("cart")
            })
            .unwrap_or(false)
    });
    if has_price_marker {
        return PageType::Product;
    }
    // Article: at least one article element or h1.
    if article_count >= 1 || h1_count >= 1 {
        return PageType::Article;
    }
    PageType::Unknown
}

/// Compute a quality score for an extracted HTML candidate. Score is in
/// 0.0..=1.0. Heuristics: text density, link density penalty, structural
/// bonuses for `<article>`/`<main>`/headings, length bonus, page-type weight.
fn score_quality(html: &str, text_len: usize, page_type: PageType) -> f32 {
    if html.is_empty() {
        return 0.0;
    }
    let total = html.len();
    let density = (text_len as f32) / (total as f32).max(1.0);

    let doc = Html::parse_fragment(html);
    let root = doc.tree.root();
    let mut a_count = 0usize;
    let mut tag_count = 0usize;
    for desc in root.descendants() {
        if let Some(el) = desc.value().as_element() {
            tag_count += 1;
            if el.name() == "a" {
                a_count += 1;
            }
        }
    }
    let link_density = if tag_count == 0 {
        1.0
    } else {
        a_count as f32 / tag_count as f32
    };
    let link_penalty = if link_density > 0.5 {
        0.3
    } else if link_density > 0.3 {
        0.6
    } else {
        1.0
    };

    let mut structural = 0.0_f32;
    if html.contains("<article") || html.contains("<main") {
        structural += 0.15;
    }
    if html.contains("<h1") || html.contains("<h2") {
        structural += 0.05;
    }

    let length_bonus = (text_len as f32 / 5000.0).min(0.2);

    let type_weight = match page_type {
        PageType::Article | PageType::Doc => 1.0,
        PageType::Product => 0.9,
        PageType::Forum => 0.85,
        PageType::Listing => 0.7,
        // Phase D: Collection (homepage aggregator) gets the same conservative
        // weight as Listing (mostly card grids, low prose density).
        PageType::Collection => 0.7,
        // Service pages (marketing, pricing, contact) are usually 80%
        // boilerplate — slightly lower weight, v3 noise filter will still
        // pick out any embedded article-like content.
        PageType::Service => 0.75,
        PageType::Unknown => 0.8,
    };

    let raw = density * link_penalty * type_weight + structural + length_bonus;
    raw.clamp(0.0, 1.0)
}

/// Count the visible text length of an HTML fragment. Strips tags and counts
/// non-whitespace characters.
fn visible_text_len(html: &str) -> usize {
    let mut in_tag = false;
    let mut count = 0usize;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag && !c.is_whitespace() {
            count += 1;
        }
    }
    count
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

    // =====================================================================
    // extract_main_content_v2 — quality scoring + automatic fallback
    // =====================================================================

    #[test]
    fn v2_returns_high_quality_for_wikipedia_article() {
        let html = r#"<html><body>
            <nav>Skip to content</nav>
            <main>
                <h1 id="firstHeading">Rust (programming language)</h1>
                <p>Rust is a multi-paradigm, general-purpose programming language
                that emphasizes performance, type safety, and concurrency. It
                enforces memory safety — that is, all references point to valid
                memory — without requiring the use of a garbage collector or
                reference counting present in other memory-safe languages.</p>
                <p>Designed by Graydon Hoare in 2006 and announced in 2010,
                Rust has been voted the "most loved programming language" in
                the Stack Overflow Developer Survey every year since 2016.</p>
                <p>It is syntactically similar to C++ but provides memory
                safety without garbage collection, and uses a borrow checker
                to validate reference lifetimes.</p>
            </main>
            <aside class="sidebar">Sidebar noise</aside>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert!(r.quality >= 0.5, "expected high quality, got {}", r.quality);
        assert!(r.markdown.contains("Rust"));
        assert!(!r.used_fallback);
        assert_eq!(r.page_type, PageType::Article);
    }

    #[test]
    fn v2_falls_back_on_js_only_page() {
        let html = r#"<html><body>
            <div id="root"></div>
            <script src="app.js"></script>
            <script>React.render(App, document.getElementById('root'));</script>
            <noscript>Please enable JS</noscript>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert!(r.used_fallback, "expected fallback on JS-only page");
        assert!(r.quality < 0.3, "quality should be low, got {}", r.quality);
        assert_eq!(r.page_type, PageType::Unknown);
    }

    #[test]
    fn v2_returns_low_quality_for_empty_input() {
        let r = extract_main_content_v2("");
        assert!(r.quality <= 0.1);
        assert!(r.used_fallback);
    }

    #[test]
    fn v2_classifies_product_page() {
        let html = r#"<html><body>
            <h1>Widget X</h1>
            <div class="price">$29.99</div>
            <div id="product-description">A great widget that solves all your problems in many ways.</div>
            <button class="buy-now">Buy now</button>
            <p>Free shipping on orders over $50. 30-day money-back guarantee. Customer reviews
            rate this product 4.5 out of 5 stars based on 1234 reviews. The widget is made
            of high-quality materials and comes with a 2-year warranty. Specifications include
            dimensions of 10x10x5 cm and weight of 250 grams.</p>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert_eq!(r.page_type, PageType::Product);
    }

    #[test]
    fn v2_classifies_doc_page() {
        let html = r#"<html><body>
            <h1>API Reference</h1>
            <pre><code>fn main() {}</code></pre>
            <pre><code>let x = 1;</code></pre>
            <pre><code>println!("hi");</code></pre>
            <pre><code>use std::io;</code></pre>
            <pre><code>struct Foo;</code></pre>
            <pre><code>impl Foo { fn new() -&gt; Self { Foo } }</code></pre>
            <table><tr><th>Col</th></tr><tr><td>1</td></tr><tr><td>2</td></tr><tr><td>3</td></tr></table>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert_eq!(r.page_type, PageType::Doc);
    }

    #[test]
    fn v2_classifies_forum_page() {
        let html = r#"<html><body>
            <article>
                <h1>Question title</h1>
                <p>Question body with enough text to pass the minimum text length threshold
                so we can verify the page-type classification without falling back to the
                permissive path.</p>
                <ul>
                    <li>Answer 1</li><li>Answer 2</li><li>Answer 3</li>
                    <li>Answer 4</li><li>Answer 5</li><li>Answer 6</li>
                    <li>Answer 7</li><li>Answer 8</li><li>Answer 9</li>
                    <li>Answer 10</li><li>Answer 11</li><li>Answer 12</li>
                </ul>
            </article>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert_eq!(r.page_type, PageType::Forum);
    }

    #[test]
    fn v2_quality_is_usable_helper() {
        let good = ExtractionResult {
            markdown: "x".repeat(500),
            quality: 0.8,
            page_type: PageType::Article,
            used_fallback: false,
        };
        assert!(good.is_usable());

        let empty = ExtractionResult {
            markdown: String::new(),
            quality: 0.0,
            page_type: PageType::Unknown,
            used_fallback: true,
        };
        assert!(!empty.is_usable());

        let borderline = ExtractionResult {
            markdown: "x".repeat(250),
            quality: 0.31,
            page_type: PageType::Article,
            used_fallback: false,
        };
        assert!(borderline.is_usable());
    }

    #[test]
    fn v2_keeps_wikipedia_tail_truncation() {
        let html = r#"<html><body>
            <main>
                <h1>Foo</h1>
                <p>Real content that should survive the extraction pipeline and end up
                in the markdown output, even when the permissive fallback is taken.</p>
                <h2 id="References">References</h2>
                <p>This should be cut at the References heading.</p>
            </main>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert!(r.markdown.contains("Real content"));
        assert!(!r.markdown.contains("This should be cut"));
    }

    #[test]
    fn v2_phase1_strict_path_keeps_classic_article() {
        // A typical Wikipedia article — should be picked up by the
        // `#mw-content-text` selector and classified as Article.
        let html = r#"<html><body>
            <div id="mw-content-text">
                <h1>Rust</h1>
                <p>Rust is a systems programming language with a focus on safety, speed,
                and concurrency. It accomplishes these goals by being memory safe without
                using garbage collection. Rust provides memory safety without garbage
                collection, and uses a borrow checker to validate reference lifetimes.</p>
                <p>The language has grown rapidly in popularity and is now used by major
                companies including Mozilla, Microsoft, Amazon, Google, and Dropbox for
                systems-level programming tasks where performance and reliability matter.</p>
            </div>
        </body></html>"#;
        let r = extract_main_content_v2(html);
        assert!(!r.used_fallback);
        assert!(r.quality > 0.3);
        assert!(r.markdown.contains("systems programming"));
        // mw-content-text is article-style.
        assert_eq!(r.page_type, PageType::Article);
    }

    // =====================================================================
    // Phase C: situation-aware extraction (v3)
    // =====================================================================

    fn synth_report(
        kind: crw_antibot::SituationKind,
        suggested: crw_antibot::SuggestedLadder,
    ) -> crw_antibot::SituationReport {
        crw_antibot::SituationReport {
            kind,
            suggested_ladder: suggested,
            status_code: Some(200),
            evidence: Vec::new(),
            notes: None,
        }
    }

    #[test]
    fn v3_soft_not_found_short_circuits_with_low_quality() {
        let html =
            "<html><body>page not found - the resource you requested does not exist</body></html>";
        // Pad so the page passes the small-payload heuristic in v2
        // (the situation detector is what should drive the decision).
        let padded = format!("{html}{}", "x".repeat(300));
        let report = synth_report(
            crw_antibot::SituationKind::SoftNotFound,
            crw_antibot::SuggestedLadder::None,
        );
        let r = extract_main_content_v3(&padded, Some(&report));
        assert_eq!(r.reason, ExtractionReason::SoftNotFound);
        assert!(r.result.quality <= 0.1);
    }

    #[test]
    fn v3_js_only_overrides_quality_downward() {
        let html = r#"<!DOCTYPE html>
<html><head><title>App</title></head><body>
<div id="root"></div>
<script src="/_next/static/chunks/main.js"></script>
<script src="/_next/static/chunks/app.js"></script>
<script src="/_next/static/chunks/framework.js"></script>
<script src="/_next/static/chunks/webpack.js"></script>
<script src="/_next/static/chunks/pages/_app.js"></script>
<script src="/_next/static/chunks/pages/index.js"></script>
</body></html>"#;
        let report = synth_report(
            crw_antibot::SituationKind::JsOnly,
            crw_antibot::SuggestedLadder::Cdp,
        );
        let r = extract_main_content_v3(html, Some(&report));
        assert_eq!(r.reason, ExtractionReason::JsOnly);
        assert!(r.result.quality <= 0.1);
    }

    #[test]
    fn v3_anti_bot_block_marks_quality_near_zero() {
        let html = r#"<html><body>Just a moment...<script src="https://challenges.cloudflare.com/"></script></body></html>"#;
        let report = synth_report(
            crw_antibot::SituationKind::CloudflareIuam,
            crw_antibot::SuggestedLadder::Cdp,
        );
        let r = extract_main_content_v3(html, Some(&report));
        assert_eq!(r.reason, ExtractionReason::AntiBotBlock);
        assert!(r.result.quality <= 0.05);
    }

    #[test]
    fn v3_clean_success_keeps_v2_quality() {
        let html = r#"<html><body>
            <main>
                <h1>Rust</h1>
                <p>Rust is a systems programming language with a focus on safety, speed,
                and concurrency. It accomplishes these goals by being memory safe without
                using garbage collection. Rust provides memory safety without garbage
                collection, and uses a borrow checker to validate reference lifetimes.</p>
                <p>The language has grown rapidly in popularity and is now used by major
                companies including Mozilla, Microsoft, Amazon, Google, and Dropbox for
                systems-level programming tasks where performance and reliability matter.</p>
            </main>
        </body></html>"#;
        let report = synth_report(
            crw_antibot::SituationKind::CleanSuccess,
            crw_antibot::SuggestedLadder::None,
        );
        let v2_baseline = extract_main_content_v2(html);
        let v3 = extract_main_content_v3(html, Some(&report));
        assert_eq!(v3.reason, ExtractionReason::Strict);
        // CleanSuccess with a real page should produce the same quality
        // as v2 (no override applied).
        assert!(
            (v3.result.quality - v2_baseline.quality).abs() < 0.01,
            "v3 quality {} should match v2 quality {}",
            v3.result.quality,
            v2_baseline.quality
        );
    }

    #[test]
    fn v3_no_situation_preserves_v2_behaviour() {
        let html = "<html><body><main><h1>Hi</h1><p>Some content here.</p></main></body></html>";
        let v2 = extract_main_content_v2(html);
        let v3 = extract_main_content_v3(html, None);
        assert_eq!(v3.reason, ExtractionReason::Strict);
        assert!(v3.situation_kind.is_none());
        assert!(v3.result.quality > 0.0);
        assert!(v2.quality > 0.0);
    }

    #[test]
    fn situation_aware_decision_empty_body_on_clean_success() {
        // A 200 OK with a body smaller than 200 chars should be
        // marked as Empty even when the detector thought it was clean
        // (the detector can miss very short bodies).
        let html = "<html><body>tiny</body></html>";
        let report = synth_report(
            crw_antibot::SituationKind::CleanSuccess,
            crw_antibot::SuggestedLadder::None,
        );
        let base = extract_main_content_v2(html);
        let d = situation_aware_decision(Some(&report), html, &base);
        assert_eq!(d.reason, ExtractionReason::Empty);
        assert_eq!(d.quality_override, Some(0.05));
    }

    #[test]
    fn cf_ray_alone_on_2xx_is_not_a_challenge() {
        // Regression test: a real Wikipedia article served through
        // Cloudflare's CDN has a cf-ray header but no challenge HTML.
        // The detector must NOT classify it as CloudflareIuam — that's
        // a false positive.
        let html = r#"<html><head><title>Rust</title></head>
        <body><main><h1>Rust</h1><p>Real article content here. Multiple
        sentences about the Rust programming language and its features
        like memory safety, zero-cost abstractions, and concurrency.
        This is the kind of long-form content we want to extract.</p>
        </main></body></html>"#;
        let headers = vec![("cf-ray".to_string(), "abc123-CDG".to_string())];
        let report = diagnose(html, Some(200), Some(&headers));
        assert_eq!(
            report.kind,
            SituationKind::CleanSuccess,
            "cf-ray alone on 2xx should not trigger Cloudflare detection"
        );
    }

    #[test]
    fn cf_ray_with_challenge_body_on_2xx_is_cloudflare_iuam() {
        // The body *does* contain a challenge marker, so the header
        // match is corroborated and we should detect CloudflareIuam.
        let html = r#"<!DOCTYPE html><html><head>
        <title>Just a moment...</title>
        <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
        </head><body>cf-mitigated</body></html>"#;
        let headers = vec![("cf-ray".to_string(), "abc123-CDG".to_string())];
        let report = diagnose(html, Some(200), Some(&headers));
        assert_eq!(report.kind, SituationKind::CloudflareIuam);
    }

    #[test]
    fn cf_ray_with_challenge_body_on_403_still_cloudflare() {
        // 4xx + cf-ray + challenge body — definitely a Cloudflare block.
        let html = r#"<html><head><title>Just a moment...</title></head>
        <body>cf-mitigated</body></html>"#;
        let headers = vec![("cf-ray".to_string(), "abc-CDG".to_string())];
        let report = diagnose(html, Some(403), Some(&headers));
        assert_eq!(report.kind, SituationKind::CloudflareIuam);
    }

    #[test]
    fn akamai_header_alone_on_2xx_is_not_a_block() {
        // Same logic: akamai-* headers on a 2xx response without any
        // body evidence is just an Akamai CDN cache hit, not a bot
        // manager block.
        let html = r#"<html><body><p>Real content with enough text to
        be a real page, not a bot challenge. We need several sentences
        to make sure the page is recognised as real content rather than
        an empty SPA shell or a challenge interstitial.</p></body></html>"#;
        let headers = vec![("x-akamai-transformed".to_string(), "9 9 9".to_string())];
        let report = diagnose(html, Some(200), Some(&headers));
        assert_eq!(
            report.kind,
            SituationKind::CleanSuccess,
            "akamai header alone on 2xx should not trigger Akamai detection"
        );
    }

    #[test]
    fn situation_does_not_downgrade_high_quality_v2_result() {
        // Regression test for Phase C's "false positive" bug: a real
        // Wikipedia article served through Cloudflare's CDN must NOT be
        // tagged as `AntiBotBlock` just because the response has a
        // `cf-ray` header. The v2 result is high-quality, so the
        // situation should be surfaced (kind recorded) but the quality
        // override must be skipped.
        let html = r#"<html><body>
            <div id="mw-content-text">
                <h1>Rust</h1>
                <p>Rust is a systems programming language with a focus on safety, speed,
                and concurrency. It accomplishes these goals by being memory safe without
                using garbage collection. Rust provides memory safety without garbage
                collection, and uses a borrow checker to validate reference lifetimes.</p>
                <p>The language has grown rapidly in popularity and is now used by major
                companies including Mozilla, Microsoft, Amazon, Google, and Dropbox for
                systems-level programming tasks where performance and reliability matter.</p>
            </div>
        </body></html>"#;
        let report = synth_report(
            crw_antibot::SituationKind::CloudflareIuam,
            crw_antibot::SuggestedLadder::Cdp,
        );
        let v2 = extract_main_content_v2(html);
        let v3 = extract_main_content_v3(html, Some(&report));
        // v2 found real content
        assert!(v2.quality >= 0.3, "v2 baseline should be high quality");
        assert!(
            v2.markdown.len() >= 500,
            "v2 baseline should have real text"
        );
        // v3 must NOT downgrade the quality
        assert!(
            v3.result.quality >= 0.3,
            "v3 quality {} should be at least v2 quality {}",
            v3.result.quality,
            v2.quality
        );
        // The reason stays Strict (real content was found) — but the
        // situation kind is recorded for the operator.
        assert_eq!(v3.reason, ExtractionReason::Strict);
        assert_eq!(v3.situation_kind.as_deref(), Some("cloudflare_iuam"));
    }

    #[test]
    fn situation_downgrades_low_quality_v2_result() {
        // The dual of the previous test: a SPA shell with a Cloudflare
        // signature should be tagged as AntiBotBlock with low quality.
        let html = r#"<!DOCTYPE html>
<html><head><title>Just a moment...</title>
<script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
</head><body>cf-mitigated</body></html>"#;
        let report = synth_report(
            crw_antibot::SituationKind::CloudflareIuam,
            crw_antibot::SuggestedLadder::Cdp,
        );
        let v2 = extract_main_content_v2(html);
        let v3 = extract_main_content_v3(html, Some(&report));
        // v2 should be low quality (challenge page)
        assert!(v2.quality < 0.3 || v2.markdown.len() < 500);
        // v3 should be tagged and overridden
        assert_eq!(v3.reason, ExtractionReason::AntiBotBlock);
        assert!(v3.result.quality <= 0.05);
    }

    #[test]
    fn v3_serialization_of_extraction_reason() {
        let html = "<html><body>page not found - not available in your country</body></html>";
        let padded = format!("{html}{}", "x".repeat(300));
        let report = synth_report(
            crw_antibot::SituationKind::SoftNotFound,
            crw_antibot::SuggestedLadder::None,
        );
        let r = extract_main_content_v3(&padded, Some(&report));
        let json = serde_json::to_string(&r.reason).unwrap();
        assert!(json.contains("SoftNotFound"));
    }
}
