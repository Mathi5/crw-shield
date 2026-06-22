use axum::{
    extract::{Path, Query, State},
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
    extract_links, extract_main_content_v4, extract_metadata, filter_tags, html_to_markdown,
};
use crw_map::discover as map_discover;
use crw_search::SearchClient;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::state::AppState;

/// HITL (Human-In-The-Loop) request — used when automatic anti-bot
/// escalation has been exhausted (DataDome/Cloudflare IUAM/PerimeterX
/// hard-captcha) and the caller wants to hand the challenge to a human.
///
/// The container is headless so we can't open a real visible browser.
/// Instead, the handler records the URL + challenge kind in a queue file
/// at `/tmp/hitl_queue.json` so an external process (Playwright Desktop,
/// human in front of Chrome, ...) can pick it up, solve the challenge
/// outside, and write the resulting cookies to the same file. A later
/// `GET /v2/scrape/hitl/result?id=...` can read the solved cookies and
/// re-run the ladder with them.
#[derive(Debug, Clone, Deserialize)]
pub struct HitlRequest {
    pub url: String,
    /// What kind of challenge we're stuck on. Used for telemetry and to
    /// give the human a hint. Optional.
    #[serde(default)]
    pub challenge_kind: Option<String>,
    /// Free-form context. Stored verbatim alongside the queue entry.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HitlEnqueueResponse {
    pub success: bool,
    pub hitl_required: bool,
    pub id: String,
    pub queue_file: String,
    pub instructions: String,
    pub created_at: String,
}

const HITL_QUEUE_PATH: &str = "/tmp/hitl_queue.json";

/// POST /v2/scrape/hitl
///
/// Records a URL that needs a human to solve a captcha. Returns
/// immediately with a `hitl_required: true` payload pointing to the
/// queue file. Does not block waiting for a solution — the caller is
/// expected to poll `GET /v2/scrape/hitl/result?id=...` or call
/// `/v2/scrape` again with `cookies` populated from the result file.
pub async fn hitl_enqueue(
    State(state): State<AppState>,
    Json(req): Json<HitlRequest>,
) -> impl IntoResponse {
    let resp = handle_hitl_enqueue(&state, req).await;
    match resp {
        Ok(r) => (StatusCode::ACCEPTED, Json(json!(r))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        )
            .into_response(),
    }
}

async fn handle_hitl_enqueue(
    _state: &AppState,
    req: HitlRequest,
) -> Result<HitlEnqueueResponse, String> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let entry = json!({
        "id": id,
        "url": req.url,
        "challenge_kind": req.challenge_kind,
        "note": req.note,
        "status": "pending",
        "created_at": now.to_rfc3339(),
    });
    // Read existing queue (if any) and append. We use a simple file-based
    // queue because we're running in a headless container — no Redis, no
    // Postgres, just /tmp. Each entry is on its own line (NDJSON) so
    // multiple concurrent enqueues don't clobber each other.
    let queue_path = PathBuf::from(HITL_QUEUE_PATH);
    let mut line = serde_json::to_string(&entry).map_err(|e| format!("serialize entry: {e}"))?;
    line.push('\n');
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&queue_path)
        .map_err(|e| format!("open queue file {}: {e}", queue_path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("append to queue file: {e}"))?;
    info!(id = %id, url = %req.url, "HITL: challenge enqueued");
    let id_for_response = id.clone();
    Ok(HitlEnqueueResponse {
        success: true,
        hitl_required: true,
        id: id_for_response,
        queue_file: HITL_QUEUE_PATH.to_string(),
        instructions: format!(
            "Open {} in a visible browser, solve the challenge, then write \
             the resulting cookies (name=value; domain=...) to {} with id={} \
             and status='solved'. A subsequent /v2/scrape call with these \
             cookies will succeed.",
            req.url, HITL_QUEUE_PATH, id
        ),
        created_at: now.to_rfc3339(),
    })
}

/// GET /v2/scrape/hitl/result?id=<uuid>
///
/// Reads back the current state of a HITL queue entry. Returns the entry
/// (with `status`, `cookies`, etc.) when found, or 404 when the id is
/// unknown. The caller is expected to poll this until `status` flips to
/// `solved`, then take the `cookies` and retry the original /v2/scrape
/// request with those cookies attached.
pub async fn hitl_result(
    State(state): State<AppState>,
    Query(params): Query<HitlResultQuery>,
) -> impl IntoResponse {
    let resp = handle_hitl_result(&state, &params.id);
    match resp {
        Ok(entry) => (StatusCode::OK, Json(entry)).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "error": e})),
        )
            .into_response(),
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct HitlResultQuery {
    pub id: String,
}

fn handle_hitl_result(_state: &AppState, id: &str) -> Result<serde_json::Value, String> {
    let queue_path = PathBuf::from(HITL_QUEUE_PATH);
    let content = std::fs::read_to_string(&queue_path)
        .map_err(|e| format!("read queue file {}: {e}", queue_path.display()))?;
    // Find the line whose `id` field matches the requested id. NDJSON
    // means we walk line by line and parse each as JSON.
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("parse queue entry: {e}"))?;
        if entry.get("id").and_then(|v| v.as_str()) == Some(id) {
            return Ok(entry);
        }
    }
    Err(format!("HITL queue entry {id} not found"))
}

pub async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok","version": env!("CARGO_PKG_VERSION")}))
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

    // Phase 3 — per-server rate limiter. Adds min_interval + random jitter
    // between consecutive scrapes to avoid hammering upstream rate-limits
    // (Cloudflare, DataDome, etc.). First call is instant, subsequent calls
    // wait `RATE_LIMIT_MIN_MS` (default 2000) + random 0..`RATE_LIMIT_JITTER_MS`
    // (default 500). Set either to 0 to disable. See `crate::rate_limit`.
    state.rate_limiter.wait().await;

    let ladder_result = state
        .ladder
        .fetch_with_rotation(&req, &state.host_counters)
        .await
        .map_err(|e| ErrorResponse::new(e.code(), e.to_string()))?;

    let fetch_result = ladder_result.fetch;
    let situation = ladder_result.situation;

    // HITL auto-trigger: if the ladder returned a response that is still
    // an anti-bot block after exhausting L1 (ClearAndRetry) and L2
    // (Rotate), enqueue a HITL request so a human can solve the
    // challenge externally. We return an `Err` with code `HITL_REQUIRED`
    // and embed the queue id + instructions in the error metadata so
    // the caller can poll for the solution.
    if situation.is_anti_bot() {
        let challenge_kind = situation.kind.as_str().to_string();
        warn!(
            url = %url,
            kind = %challenge_kind,
            "L3 Fail: ladder exhausted; auto-enqueueing HITL request"
        );
        match handle_hitl_enqueue(
            state,
            HitlRequest {
                url: url.clone(),
                challenge_kind: Some(challenge_kind.clone()),
                note: Some(format!(
                    "auto-triggered after ladder exhausted L1+L2 (situation={challenge_kind})"
                )),
            },
        )
        .await
        {
            Ok(hitl_resp) => {
                let instructions = format!(
                    "anti-bot challenge ({challenge_kind}) not solved automatically; \
                     open the URL in a browser, solve the challenge, then call \
                     GET /v2/scrape/hitl/result?id={} to retrieve the cookies, \
                     and retry the original /v2/scrape with those cookies.",
                    hitl_resp.id
                );
                // Build a structured ErrorResponse with the HITL payload
                // embedded in the error metadata. The scrape() wrapper
                // surfaces this as a 503 to the caller.
                let mut err = ErrorResponse::new("HITL_REQUIRED", instructions);
                err.details = Some(json!({
                    "hitl_id": hitl_resp.id,
                    "queue_file": hitl_resp.queue_file,
                    "challenge_kind": challenge_kind,
                    "url": url,
                    "created_at": hitl_resp.created_at,
                    "instructions": hitl_resp.instructions,
                }));
                return Err(err);
            }
            Err(e) => {
                error!(error = %e, "failed to auto-enqueue HITL request");
                // Fall through to the regular CHALLENGE_DETECTED path.
            }
        }
    }

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
    let mut extraction_reason: Option<String> = None;

    if req.only_main_content {
        // Phase D: page-type-aware extraction router. v4 reuses v3's
        // situation-awareness (SoftNotFound / JsOnly / anti-bot blocks
        // short-circuit with a tagged reason) AND, when the page-type is
        // Article or Doc, delegates to the optional firecrawl-extractor
        // pipeline for better scoring + fallback chain.
        //
        // The URL is forwarded to the Firecrawl extractor so it can rewrite
        // relative links and use the URL for page-type scoring position
        // hints. With the feature off, the url argument is ignored.
        let v4 = extract_main_content_v4(
            &html_for_extraction,
            Some(&situation),
            &fetch_result.final_url,
        );
        html_for_extraction = v4.result.markdown;
        extraction_quality = Some(v4.result.quality);
        page_type = Some(format!("{:?}", v4.result.page_type).to_lowercase());
        extraction_reason = Some(format!("{:?}", v4.reason).to_lowercase());
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
        situation: Some(situation),
        extraction_reason: extraction_reason.clone(),
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
        "HITL_REQUIRED" => StatusCode::SERVICE_UNAVAILABLE,
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
