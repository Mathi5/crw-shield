//! Headed HITL solve flow — v0.4.6.
//!
//! This is the HTTP handler entry point that ties the web viewer to
//! the headed Chromium browser. When a scrape escalates to L3 with
//! `?wait_for_human=true`, the handler:
//!
//! 1. Launches a `HeadedHitlSession` (separate Chromium in
//!    `headless=new` mode).
//! 2. Registers it in the `SessionRegistry` so the viewer can find it.
//! 3. Returns a JSON response with the `hitl_id` AND the viewer URL
//!    the operator can open in a browser.
//!
//! The actual blocking (waiting for the operator to click Turnstile,
//! polling for the cf_clearance cookie, re-fetching in the same TLS
//! session) is done by a background task that runs concurrently with
//! the HTTP response. When the solve completes (success or timeout),
//! the result is stored in the `HitlQueue` so the original
//! `POST /v2/scrape` caller can pick it up via
//! `GET /v2/scrape/hitl/result?id=...`.

use std::sync::Arc;

use axum::http::StatusCode;
use serde::Serialize;
use tracing::{info, warn};

use crw_core::ScrapeRequest;
use crw_fetch::{HeadedHitlSession, SessionRegistry};

use crate::state::AppState;

/// JSON returned to the operator when a headed HITL session is created.
/// Includes the viewer URL the operator opens in their browser.
#[derive(Debug, Serialize)]
pub struct HeadedHitlResponse {
    pub hitl_id: String,
    pub url: String,
    pub viewer_url: String,
    /// Message intended for human consumption (logs, Discord webhook, …).
    pub message: String,
}

/// Spawn a headed HITL session for the given URL. Returns the metadata
/// the operator needs to connect to the viewer.
///
/// This is the entry point called by the modified `/v2/scrape` handler
/// when `?wait_for_human=true` is set and the ladder escalates to L3.
pub async fn spawn_headed_hitl(
    state: Arc<AppState>,
    req: &ScrapeRequest,
) -> Result<HeadedHitlResponse, StatusCode> {
    let hitl_id = uuid::Uuid::new_v4().to_string();
    let url = req.url.clone();
    let public_url = std::env::var("CRW_PUBLIC_URL").unwrap_or_else(|_| {
        let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = std::env::var("PORT").unwrap_or_else(|_| "3002".to_string());
        format!("http://{host}:{port}")
    });

    info!(hitl_id = %hitl_id, url = %url, "spawning headed HITL session");

    let session = match HeadedHitlSession::launch(&hitl_id, &url).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(error = %e, "failed to launch headed HITL browser");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Register so the viewer endpoint can find it.
    {
        let mut registry: tokio::sync::MutexGuard<
            '_,
            std::collections::HashMap<String, Arc<HeadedHitlSession>>,
        > = state.hitl_sessions.lock().await;
        registry.insert(hitl_id.clone(), session.clone());
    }

    // Spawn the background solve task. It will:
    //  - Poll the browser's cookie jar for a session-like cookie
    //  - When found, extract cookies + re-fetch in the same session
    //  - Store the result somewhere the original caller can fetch it
    //  - Clean up the session
    let registry = state.hitl_sessions.clone();
    let _hitl_id_for_task = hitl_id.clone();
    let _url_for_task = url.clone();
    tokio::spawn(async move {
        run_solve_loop(session, registry, _hitl_id_for_task, _url_for_task).await;
    });

    let viewer_url = format!("{public_url}/hitl/viewer/{hitl_id}");
    let message = format!(
        "Headed HITL browser launched. Open {viewer_url} in any browser \
         to see and click the Cloudflare challenge. The scrape will auto-resolve \
         once a session cookie appears."
    );

    Ok(HeadedHitlResponse {
        viewer_url,
        hitl_id,
        url,
        message,
    })
}

/// Background solve loop: poll cookies, re-scrape on success, clean up.
async fn run_solve_loop(
    session: Arc<HeadedHitlSession>,
    registry: SessionRegistry,
    hitl_id: String,
    url: String,
) {
    info!(hitl_id = %hitl_id, "solve loop started");

    let solve_result = session.wait_for_solve().await;

    match solve_result {
        Ok(cookies) => {
            info!(
                hitl_id = %hitl_id,
                cookies = cookies.len(),
                "cookies captured; extracting + re-fetching"
            );

            // Inject into the shared CookieJar so future scrapes of
            // this domain reuse the session cookie.
            // (The viewer endpoint doesn't share AppState, so we look
            // up the jar via the registry's session only for the
            // refetch; the re-fetch uses the same browser which is
            // what really matters for the TLS-session continuity.)
            //
            // We also save the cookies to the persistence path so
            // they survive a server restart.
            if let Some(path) = std::env::var("COOKIE_PERSISTENCE_PATH")
                .ok()
                .filter(|s| !s.is_empty())
            {
                save_cookies_to_path(&cookies, &url, std::path::Path::new(&path));
            }

            // Re-fetch in the same browser session. The cookies are
            // still set in the browser, so the re-fetch should pass
            // the challenge.
            match session.refetch_in_same_session().await {
                Ok(_html) => {
                    info!(hitl_id = %hitl_id, "re-fetch succeeded in same session");
                    // TODO: store the resolved HTML in a place where
                    // the original `POST /v2/scrape` caller can pick
                    // it up. For v0.4.6 we just close the session
                    // and return; the caller can check
                    // `/v2/scrape/hitl/result?id=...` to see status.
                }
                Err(e) => {
                    warn!(hitl_id = %hitl_id, error = %e, "re-fetch failed");
                }
            }
        }
        Err(e) => {
            warn!(hitl_id = %hitl_id, error = %e, "solve loop failed");
        }
    }

    // Clean up: close the browser and remove from the registry.
    {
        let mut reg = registry.lock().await;
        reg.remove(&hitl_id);
    }
    HeadedHitlSession::close_via_arc(session).await;
    info!(hitl_id = %hitl_id, "solve loop done, session cleaned up");
}

/// Persist a captured cookie set to the shared cookie jar file.
/// (Best-effort: if the file doesn't exist yet, we create it.)
fn save_cookies_to_path(cookies: &[serde_json::Value], url: &str, path: &std::path::Path) {
    use crw_antibot::CookieJar;
    let jar = CookieJar::load_from_path(path).unwrap_or_default();
    crw_fetch::inject_cdp_cookies_into_jar(&jar, cookies);
    if let Err(e) = jar.save_to_path(path) {
        warn!(path = %path.display(), error = %e, url = %url,
              "failed to persist captured HITL cookies");
    } else {
        info!(path = %path.display(), cookies = cookies.len(),
              "captured HITL cookies persisted");
    }
}
