//! Adapter layer over the optional `html-extractor` dependency
//! ([firecrawl/html-extractor](https://github.com/firecrawl/html-extractor),
//! Apache-2.0). Translates its public API into crw-shield's internal
//! `ExtractionResult` shape so the rest of the codebase can stay
//! dependency-agnostic.
//!
//! **Compiled only when the `firecrawl-extractor` feature is enabled.**
//! Without the feature, every public function in this module still exists
//! (so call sites don't need `#[cfg]`), but returns `None` / no-ops.
//!
//! Copyright 2026 crw-shield contributors.
//! Includes code from firecrawl/html-extractor (Apache-2.0). See NOTICE.

use crate::content::{ExtractionReason, ExtractionResult};
// `PageType` is only used inside the feature-gated `map_page_type` function.
// Keeping the import unconditional would generate an `unused_imports` warning
// on the no-feature build.

/// Re-export the upstream `PageType` enum so call sites can pattern-match
/// against the Firecrawl variants when they need to (e.g. the v4 router
/// checks for `Article` / `Doc` upstream classification).
#[cfg(feature = "firecrawl-extractor")]
pub use html_extractor::PageType as FirecrawlPageType;

/// Map an upstream Firecrawl `PageType` to our internal `PageType`.
///
/// Both enums cover the same 8-way classification. The mapping is
/// 1:1 with no information loss: every upstream variant has a matching
/// internal variant. The reverse is also true, so `From`/`Into` could be
/// derived — we keep an explicit function so future-proofing is one edit.
#[cfg(feature = "firecrawl-extractor")]
pub fn map_page_type(ft: html_extractor::PageType) -> crate::content::PageType {
    use crate::content::PageType;
    use html_extractor::PageType as Fp;
    match ft {
        Fp::Article => PageType::Article,
        Fp::Product => PageType::Product,
        Fp::Listing => PageType::Listing,
        Fp::Forum => PageType::Forum,
        Fp::Documentation => PageType::Doc,
        Fp::Collection => PageType::Collection,
        Fp::Service => PageType::Service,
        Fp::Other => PageType::Unknown,
    }
}

/// Run the 5-stage Firecrawl extractor and return our `ExtractionResult`.
///
/// Returns `None` when:
/// - the `firecrawl-extractor` feature is off
/// - the upstream extractor reports an error (parse failure, conflicting
///   options)
/// - the input is empty (upstream returns `EmptyInput` → empty markdown)
///
/// The caller is expected to fall back to `extract_main_content_v3` on
/// `None`.
#[cfg(feature = "firecrawl-extractor")]
pub fn extract_with_firecrawl(html: &str, url: &str) -> Option<ExtractionResult> {
    let opts = html_extractor::ExtractOptions {
        url: if url.is_empty() {
            None
        } else {
            Some(url.to_string())
        },
        ..Default::default()
    };
    let res = html_extractor::extract(html, &opts).ok()?;

    // Upstream treats empty input as a soft error. We propagate the empty
    // result so the caller can decide whether to fall back (v3 might do
    // better on thin content).
    if res.markdown.trim().is_empty() {
        return None;
    }

    let used_fallback = res.stats.as_ref().is_some_and(|s| s.used_fallback);

    Some(ExtractionResult {
        markdown: res.markdown,
        quality: res.extraction_quality as f32,
        page_type: map_page_type(res.page_type),
        used_fallback,
    })
}

/// Feature-off stub: returns `None` so the caller's fallback (v3) runs.
#[cfg(not(feature = "firecrawl-extractor"))]
pub fn extract_with_firecrawl(_html: &str, _url: &str) -> Option<ExtractionResult> {
    None
}

/// Map an extraction quality score (0.0–1.0) to our `ExtractionReason` enum.
///
/// Thresholds chosen to match our existing Phase C taxonomy:
/// - `>= 0.7` → `Strict` (high confidence, likely hit the article body
///   first try)
/// - `0.3..0.7` → `Heuristic` (got something, not great — operator may
///   want to inspect the output)
/// - `< 0.3` → `Fallback` (low quality, caller should consider
///   escalating to FS/CDP or returning an error)
pub fn reason_from_quality(quality: f32) -> ExtractionReason {
    if quality >= 0.7 {
        ExtractionReason::Strict
    } else if quality >= 0.3 {
        ExtractionReason::Heuristic
    } else {
        ExtractionReason::Fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Always-available test: `reason_from_quality` is feature-agnostic.
    // (The feature-gated tests for `map_page_type` and `extract_with_firecrawl`
    // are in their own #[cfg] block below.)

    #[test]
    fn reason_thresholds_above_strict() {
        assert_eq!(reason_from_quality(1.0), ExtractionReason::Strict);
        assert_eq!(reason_from_quality(0.95), ExtractionReason::Strict);
        assert_eq!(reason_from_quality(0.70), ExtractionReason::Strict);
    }

    #[test]
    fn reason_thresholds_heuristic_band() {
        assert_eq!(reason_from_quality(0.69), ExtractionReason::Heuristic);
        assert_eq!(reason_from_quality(0.50), ExtractionReason::Heuristic);
        assert_eq!(reason_from_quality(0.30), ExtractionReason::Heuristic);
    }

    #[test]
    fn reason_thresholds_below_fallback() {
        assert_eq!(reason_from_quality(0.29), ExtractionReason::Fallback);
        assert_eq!(reason_from_quality(0.10), ExtractionReason::Fallback);
        assert_eq!(reason_from_quality(0.00), ExtractionReason::Fallback);
    }

    #[test]
    fn extract_with_firecrawl_stub_returns_none_without_feature() {
        // Without the feature, the wrapper is a no-op stub.
        #[cfg(not(feature = "firecrawl-extractor"))]
        assert!(
            extract_with_firecrawl("<html><body><p>hi</p></body></html>", "https://x").is_none()
        );
    }

    #[cfg(feature = "firecrawl-extractor")]
    mod with_feature {
        use super::*;
        use crate::content::PageType;

        #[test]
        fn page_type_mapping_is_bijective() {
            // All 8 upstream variants must map to a distinct internal variant.
            let cases = [
                (html_extractor::PageType::Article, PageType::Article),
                (html_extractor::PageType::Forum, PageType::Forum),
                (html_extractor::PageType::Product, PageType::Product),
                (html_extractor::PageType::Listing, PageType::Listing),
                (html_extractor::PageType::Documentation, PageType::Doc),
                (html_extractor::PageType::Collection, PageType::Collection),
                (html_extractor::PageType::Service, PageType::Service),
                (html_extractor::PageType::Other, PageType::Unknown),
            ];
            for (firecrawl, ours) in cases {
                assert_eq!(map_page_type(firecrawl), ours);
            }
        }

        #[test]
        fn extracts_basic_article() {
            let html = r#"
                <html>
                  <head><title>Test</title></head>
                  <body>
                    <nav>Nav junk</nav>
                    <article>
                      <h1>Hello World</h1>
                      <p>This is the main content. It has enough text to be useful for the extractor to identify it as a real article body.</p>
                      <p>Multiple paragraphs help the density score. We add even more text here to push past the 25-char minimum threshold that html-extractor enforces by default.</p>
                    </article>
                    <footer>Footer junk</footer>
                  </body>
                </html>
            "#;
            let res = extract_with_firecrawl(html, "https://example.com/test")
                .expect("html-extractor should be enabled and HTML is valid");
            assert!(
                res.markdown.contains("Hello World"),
                "missing H1: {}",
                res.markdown
            );
            assert!(
                res.markdown.contains("main content"),
                "missing article body: {}",
                res.markdown
            );
            // Footer/nav should be stripped by the 5-stage pipeline.
            assert!(
                !res.markdown.contains("Nav junk"),
                "nav leaked: {}",
                res.markdown
            );
            assert!(
                !res.markdown.contains("Footer junk"),
                "footer leaked: {}",
                res.markdown
            );
            assert!(
                res.quality > 0.0,
                "quality should be > 0, got {}",
                res.quality
            );
        }

        #[test]
        fn empty_input_returns_none() {
            // Upstream returns Ok(empty ExtractResult) for empty input; our
            // wrapper treats that as a signal to fall back to v3.
            assert!(extract_with_firecrawl("", "https://x").is_none());
            assert!(extract_with_firecrawl("   \n  ", "https://x").is_none());
        }

        #[test]
        fn tiny_page_still_returns_some() {
            // Sanity check: a page with even a tiny bit of content should
            // produce *some* output, not None. This documents the wrapper's
            // contract that we only fall back on truly empty input.
            let html = r#"<html><body><p>hi</p></body></html>"#;
            let res = extract_with_firecrawl(html, "https://x");
            assert!(
                res.is_some(),
                "even tiny pages should return Some so v4 router can decide"
            );
        }
    }
}
