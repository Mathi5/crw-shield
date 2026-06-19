use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use base64::Engine;
use chrono::Utc;
use crw_antibot::detect_challenge;
use crw_core::{
    CrawlRequest, CrawlResponse, CrawlStatus, CrawlStatusResponse, CrwError, ErrorResponse, Format,
    MapRequest, MapResponse, ScrapeData, ScrapeMetadata, ScrapeRequest, ScrapeResponse,
    SearchRequest, SearchResponse,
};
use crw_crawl::{crw_crawl, FetcherScrapeRunner};
use crw_extract::{
    extract_links, extract_main_content_v2, extract_metadata, filter_tags, html_to_markdown,
};
use crw_map::discover as map_discover;
use crw_search::SearchClient;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

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

    let ladder_result = state
        .ladder
        .fetch(&req)
        .await
        .map_err(|e| ErrorResponse::new(e.code(), e.to_string()))?;

    let fetch_result = ladder_result.fetch;

    if !state.config.cdp_enabled {
        if let Some(challenge) = detect_challenge(&fetch_result.html) {
            warn!(url = %url, challenge = %challenge, "challenge detected");
            return Err(ErrorResponse::new(
                "CHALLENGE_DETECTED",
                format!("anti-bot challenge detected: {challenge}"),
            ));
        }
    }

    let formats: HashSet<Format> = req.formats.iter().copied().collect();
    let wants = |f: Format| formats.contains(&f);

    let mut html_for_extraction = fetch_result.html.clone();
    let mut extraction_quality: Option<f32> = None;
    let mut page_type: Option<String> = None;

    if req.only_main_content {
        let result = extract_main_content_v2(&html_for_extraction);
        html_for_extraction = result.markdown;
        extraction_quality = Some(result.quality);
        page_type = Some(format!("{:?}", result.page_type).to_lowercase());
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
    let metadata = ScrapeMetadata {
        extraction_quality,
        page_type,
        ..metadata
    };

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
        ladder_result.screenshot.as_ref().map(|bytes| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        })
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
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "HTTP_ERROR" => StatusCode::BAD_GATEWAY,
        "FETCH_ERROR" => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn crw_error_to_response(err: CrwError) -> ErrorResponse {
    ErrorResponse::new(err.code(), err.to_string())
}

// ---------------------------------------------------------------------------------------
// Crawl handlers
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CrawlJob {
    pub id: String,
    pub url: String,
    pub status: CrawlStatus,
    pub created_at: chrono::DateTime<Utc>,
    pub completed_at: Option<chrono::DateTime<Utc>>,
    pub data: Vec<ScrapeData>,
    pub error: Option<String>,
}

pub async fn crawl_start(
    State(state): State<AppState>,
    Json(req): Json<CrawlRequest>,
) -> impl IntoResponse {
    match handle_crawl_start(&state, req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

async fn handle_crawl_start(
    state: &AppState,
    req: CrawlRequest,
) -> Result<CrawlResponse, ErrorResponse> {
    if url::Url::parse(&req.url).is_err() {
        return Err(ErrorResponse::new("INVALID_URL", "url is not valid"));
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let job = CrawlJob {
        id: id.clone(),
        url: req.url.clone(),
        status: CrawlStatus::Queued,
        created_at: now,
        completed_at: None,
        data: Vec::new(),
        error: None,
    };
    state
        .jobs
        .lock()
        .expect("jobs mutex poisoned")
        .insert(id.clone(), job);
    info!(job_id = %id, url = %req.url, "crawl job enqueued");

    // Spawn the actual crawl so the API responds immediately. The job is
    // mutated in place via the shared `jobs` HashMap.
    let jobs = state.jobs.clone();
    let ladder = state.ladder.clone();
    let job_id = id.clone();
    let crawl_req = req.clone();
    tokio::spawn(async move {
        run_crawl_job(jobs, ladder, job_id, crawl_req).await;
    });

    Ok(CrawlResponse {
        success: true,
        id,
        url: req.url,
        credits_used: 1,
    })
}

/// Background task that drives the BFS crawl for `job_id`. Mutates the
/// in-memory job record as pages come in, transitioning it through
/// `scraping` → `completed` (or `failed`).
async fn run_crawl_job(
    jobs: Arc<std::sync::Mutex<std::collections::HashMap<String, CrawlJob>>>,
    ladder: Arc<crate::state::FetchLadderType>,
    job_id: String,
    req: CrawlRequest,
) {
    // Flip to scraping before we start hitting the network.
    {
        let mut guard = jobs.lock().expect("jobs mutex poisoned");
        if let Some(job) = guard.get_mut(&job_id) {
            if matches!(job.status, CrawlStatus::Cancelled) {
                return;
            }
            job.status = CrawlStatus::Scraping;
        } else {
            return;
        }
    }

    let runner = Arc::new(FetcherScrapeRunner { fetcher: ladder });
    let result = crw_crawl(runner, &req).await;

    let mut guard = jobs.lock().expect("jobs mutex poisoned");
    let Some(job) = guard.get_mut(&job_id) else {
        return;
    };
    // Respect a concurrent cancel.
    if matches!(job.status, CrawlStatus::Cancelled) {
        return;
    }
    match result {
        Ok(data) => {
            info!(job_id = %job_id, pages = data.len(), "crawl job completed");
            job.data = data;
            job.status = CrawlStatus::Completed;
            job.completed_at = Some(Utc::now());
        }
        Err(e) => {
            error!(job_id = %job_id, error = %e, "crawl job failed");
            job.status = CrawlStatus::Failed;
            job.error = Some(e.to_string());
            job.completed_at = Some(Utc::now());
        }
    }
}

pub async fn crawl_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match handle_crawl_status(&state, &id) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

fn handle_crawl_status(state: &AppState, id: &str) -> Result<CrawlStatusResponse, ErrorResponse> {
    let jobs = state.jobs.lock().expect("jobs mutex poisoned");
    let job = jobs
        .get(id)
        .ok_or_else(|| ErrorResponse::new("NOT_FOUND", "crawl job not found"))?;
    let completed_at = job.completed_at;
    let created_at = job.created_at;
    let duration = completed_at.map(|c| (c - created_at).num_milliseconds() as f64 / 1000.0);
    Ok(CrawlStatusResponse {
        status: job.status,
        total: job.data.len() as u32,
        completed: job.data.len() as u32,
        credits_used: job.data.len() as u32,
        expires_at: None,
        created_at: Some(created_at),
        completed_at,
        duration,
        next: None,
        data: job.data.clone(),
        error: job.error.clone(),
    })
}

pub async fn crawl_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match handle_crawl_cancel(&state, &id) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

fn handle_crawl_cancel(state: &AppState, id: &str) -> Result<serde_json::Value, ErrorResponse> {
    let mut jobs = state.jobs.lock().expect("jobs mutex poisoned");
    let job = jobs
        .get_mut(id)
        .ok_or_else(|| ErrorResponse::new("NOT_FOUND", "crawl job not found"))?;
    job.status = CrawlStatus::Cancelled;
    job.completed_at = Some(Utc::now());
    info!(job_id = %id, "crawl job cancelled");
    Ok(json!({ "success": true, "id": id, "status": "cancelled" }))
}

// ---------------------------------------------------------------------------------------
// Map handler
// ---------------------------------------------------------------------------------------

pub async fn map(State(state): State<AppState>, Json(req): Json<MapRequest>) -> impl IntoResponse {
    match handle_map(&state, req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

async fn handle_map(state: &AppState, req: MapRequest) -> Result<MapResponse, ErrorResponse> {
    let _ = state;
    if url::Url::parse(&req.url).is_err() {
        return Err(ErrorResponse::new("INVALID_URL", "url is not valid"));
    }
    let result = map_discover(&req).await.map_err(crw_error_to_response)?;
    Ok(result)
}

// ---------------------------------------------------------------------------------------
// Search handler
// ---------------------------------------------------------------------------------------

pub async fn search(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> impl IntoResponse {
    match handle_search(&state, req).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(err_resp) => {
            let status = status_for_code(&err_resp.error);
            (status, Json(err_resp)).into_response()
        }
    }
}

async fn handle_search(
    state: &AppState,
    req: SearchRequest,
) -> Result<SearchResponse, ErrorResponse> {
    if req.query.trim().is_empty() {
        return Err(ErrorResponse::new(
            "INVALID_QUERY",
            "query must not be empty",
        ));
    }
    let client = match state.config.searxng_url.as_deref() {
        Some(url) if !url.is_empty() => SearchClient::new(url, state.config.searxng_token.clone()),
        _ => return Ok(SearchClient::new("", None).empty_response(&req)),
    };
    let result = client.search(&req).await.map_err(crw_error_to_response)?;
    Ok(result)
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
