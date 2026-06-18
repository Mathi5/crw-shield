use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use crw_antibot::detect_challenge;
use crw_core::{ErrorResponse, Format, ScrapeData, ScrapeMetadata, ScrapeRequest, ScrapeResponse};
use crw_extract::{
    extract_links, extract_main_content, extract_metadata, filter_tags, html_to_markdown,
};
use crw_fetch::Fetcher;
use serde_json::json;
use std::collections::HashSet;
use tracing::{info, warn};

use crate::state::AppState;

pub async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

pub async fn scrape(
    State(state): State<AppState>,
    Json(req): Json<ScrapeRequest>,
) -> impl IntoResponse {
    match handle_scrape(&state, req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

async fn handle_scrape(
    state: &AppState,
    req: ScrapeRequest,
) -> Result<ScrapeResponse, ErrorResponse> {
    let url = req.url.clone();
    if url::Url::parse(&url).is_err() {
        return Err(ErrorResponse::new("INVALID_URL", "url is not valid"));
    }

    info!(url = %url, "scrape request");

    let fetch_result = state
        .fetcher
        .fetch(&req)
        .await
        .map_err(|e| ErrorResponse::new(e.code(), e.to_string()))?;

    if let Some(challenge) = detect_challenge(&fetch_result.html) {
        warn!(url = %url, challenge = %challenge, "challenge detected");
        return Err(ErrorResponse::new(
            "CHALLENGE_DETECTED",
            format!("anti-bot challenge detected: {challenge}"),
        ));
    }

    let formats: HashSet<Format> = req.formats.iter().copied().collect();
    let wants = |f: Format| formats.contains(&f);

    let mut html_for_extraction = fetch_result.html.clone();
    if req.only_main_content {
        html_for_extraction = extract_main_content(&html_for_extraction);
    }
    if !req.include_tags.is_empty() || !req.exclude_tags.is_empty() {
        html_for_extraction =
            filter_tags(&html_for_extraction, &req.include_tags, &req.exclude_tags);
    }

    let metadata = extract_metadata(
        &fetch_result.html,
        &fetch_result.final_url,
        fetch_result.status_code,
    );

    let markdown = if wants(Format::Markdown) {
        Some(html_to_markdown(&html_for_extraction))
    } else {
        None
    };
    let html_out = if wants(Format::Html) {
        Some(html_for_extraction.clone())
    } else {
        None
    };
    let raw_html = if wants(Format::RawHtml) {
        Some(fetch_result.html.clone())
    } else {
        None
    };
    let links = if wants(Format::Links) {
        Some(extract_links(&fetch_result.html, &fetch_result.final_url))
    } else {
        None
    };
    let screenshot = if wants(Format::Screenshot) {
        return Err(ErrorResponse::new(
            "NOT_IMPLEMENTED",
            "Screenshot is not available in Phase 1 (CDP required)",
        ));
    } else {
        None
    };

    let data = ScrapeData {
        markdown,
        html: html_out,
        raw_html,
        links,
        screenshot,
        metadata: ScrapeMetadata {
            error: None,
            ..metadata
        },
    };

    Ok(ScrapeResponse::ok(data))
}

fn status_for_code(code: &str) -> StatusCode {
    match code {
        "INVALID_URL" => StatusCode::BAD_REQUEST,
        "CHALLENGE_DETECTED" => StatusCode::FORBIDDEN,
        "NOT_IMPLEMENTED" => StatusCode::NOT_IMPLEMENTED,
        "HTTP_ERROR" => StatusCode::BAD_GATEWAY,
        "FETCH_ERROR" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_for_code_known() {
        assert_eq!(status_for_code("INVALID_URL"), StatusCode::BAD_REQUEST);
        assert_eq!(status_for_code("CHALLENGE_DETECTED"), StatusCode::FORBIDDEN);
        assert_eq!(
            status_for_code("NOT_IMPLEMENTED"),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(status_for_code("OTHER"), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
