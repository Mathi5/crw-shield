pub mod content;
pub mod links;
pub mod markdown;
pub mod metadata;

pub use content::{
    extract_main_content, extract_main_content_v2, filter_tags, strip_unwanted, ExtractionResult,
    PageType,
};
pub use links::extract_links;
pub use markdown::html_to_markdown;
pub use metadata::extract_metadata;
