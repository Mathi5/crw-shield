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
