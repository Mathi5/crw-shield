pub mod content;
pub mod links;
pub mod markdown;
pub mod metadata;

pub use content::{
    extract_main_content, extract_main_content_v2, extract_main_content_v3, filter_tags,
    situation_aware_decision, strip_unwanted, ExtractionReason, ExtractionResult,
    ExtractionResultWithReason, PageType, SituationDecision,
};
pub use links::extract_links;
pub use markdown::html_to_markdown;
pub use metadata::extract_metadata;
