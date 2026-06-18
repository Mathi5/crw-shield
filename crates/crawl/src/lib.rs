//! BFS crawl engine.
//!
//! TODO: Phase 2 — implement BFS scheduling, URL deduplication, robots.txt
//! handling, and per-host concurrency control.

use crw_core::Result;
use url::Url;

pub struct CrawlEngine;

impl CrawlEngine {
    pub fn new() -> Self {
        Self
    }

    /// Returns `true` if `url` should be enqueued given a list of include /
    /// exclude glob-style patterns (case-sensitive on the path).
    pub fn should_visit(url: &Url, include: &[String], exclude: &[String]) -> bool {
        let path = url.path().trim_start_matches('/');
        for pat in exclude {
            if glob_match(pat, path) {
                return false;
            }
        }
        if include.is_empty() {
            return true;
        }
        include.iter().any(|pat| glob_match(pat, path))
    }
}

impl Default for CrawlEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    // Convert glob (`*`/`?`) to a regex. `.` is treated as a literal path
    // separator marker (i.e. not a regex special char) so that patterns like
    // `blog/.*` match `blog/foo`, `blog/bar`, etc.
    let mut re = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' => re.push('.'),
            '(' | ')' | '[' | ']' | '{' | '}' | '+' | '|' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(ch);
            }
            c => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
        .map(|r| r.is_match(text))
        .unwrap_or(false)
}

pub fn placeholder_result<T>() -> Result<T> {
    Err(crw_core::CrwError::NotImplemented(
        "crawl engine — Phase 2".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_visit_no_filters_always_true() {
        let url = Url::parse("https://example.com/x").unwrap();
        assert!(CrawlEngine::should_visit(&url, &[], &[]));
    }

    #[test]
    fn exclude_path_filters_out() {
        let url = Url::parse("https://example.com/blog/foo").unwrap();
        assert!(!CrawlEngine::should_visit(
            &url,
            &[],
            &["blog/.*".to_string()]
        ));
        let url2 = Url::parse("https://example.com/docs/x").unwrap();
        assert!(CrawlEngine::should_visit(
            &url2,
            &[],
            &["blog/.*".to_string()]
        ));
    }

    #[test]
    fn include_path_filters_in() {
        let url = Url::parse("https://example.com/blog/x").unwrap();
        assert!(!CrawlEngine::should_visit(
            &url,
            &["docs/.*".to_string()],
            &[]
        ));
        let url2 = Url::parse("https://example.com/docs/x").unwrap();
        assert!(CrawlEngine::should_visit(
            &url2,
            &["docs/.*".to_string()],
            &[]
        ));
    }

    #[test]
    fn glob_question_mark_matches_single_char() {
        let url = Url::parse("https://example.com/abc").unwrap();
        assert!(CrawlEngine::should_visit(&url, &["ab?".to_string()], &[]));
        let url2 = Url::parse("https://example.com/abcd").unwrap();
        assert!(!CrawlEngine::should_visit(&url2, &["ab?".to_string()], &[]));
    }

    #[test]
    fn placeholder_returns_not_implemented() {
        let r: crw_core::Result<()> = placeholder_result();
        assert!(matches!(r, Err(crw_core::CrwError::NotImplemented(_))));
    }
}
