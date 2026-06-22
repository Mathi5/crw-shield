pub mod content;
pub mod links;
pub mod markdown;
pub mod metadata;
// Phase D: Firecrawl html-extractor adapter. Module exists whether or
// not the feature is on (call sites don't need `#[cfg]`), but functions
// are no-ops without the feature.
pub mod firecrawl_compat;

pub use content::{
    extract_main_content, extract_main_content_v2, extract_main_content_v3, filter_tags,
    situation_aware_decision, strip_unwanted, ExtractionReason, ExtractionResult,
    ExtractionResultWithReason, PageType, SituationDecision,
};
pub use links::extract_links;
pub use markdown::html_to_markdown;
pub use metadata::extract_metadata;
// Phase D: re-export the wrapper. `map_page_type` is feature-gated; we
// provide a stub for the no-feature build so call sites compile either way.
pub use firecrawl_compat::{extract_with_firecrawl, reason_from_quality};
#[cfg(feature = "firecrawl-extractor")]
pub use firecrawl_compat::map_page_type;
