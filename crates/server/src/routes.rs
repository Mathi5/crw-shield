use axum::{
    routing::{delete, get, post},
    Router,
};
use tower::ServiceBuilder;

use crate::handlers::{
    crawl_cancel, crawl_start, crawl_status, health, hitl_enqueue, hitl_result, hitl_solve,
    hitl_solve_ui_get, hitl_solve_ui_post, map, scrape, search,
};
use crate::headed_viewer::{hitl_cdp_proxy, hitl_viewer_page};
use crate::middleware::auth_middleware;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let auth_layer = axum::middleware::from_fn_with_state(state.clone(), auth_middleware);
    Router::new()
        .route("/health", get(health))
        .route("/v2/scrape", post(scrape))
        .route("/v2/scrape/hitl", post(hitl_enqueue))
        .route("/v2/scrape/hitl/result", get(hitl_result))
        .route("/v2/scrape/hitl/:id/solve", post(hitl_solve))
        // Self-service solve UI: GET renders the form, POST accepts the form
        // submission. Both call the same `handle_hitl_solve` core under the
        // hood, so the JSON endpoint stays available for programmatic use.
        .route(
            "/v2/scrape/hitl/:id/solve-ui",
            get(hitl_solve_ui_get).post(hitl_solve_ui_post),
        )
        .route("/v2/crawl", post(crawl_start))
        .route("/v2/crawl/:id", get(crawl_status))
        .route("/v2/crawl/:id", delete(crawl_cancel))
        .route("/v2/map", post(map))
        .route("/v2/search", post(search))
        // Headed HITL viewer (v0.4.6). Mounted BEFORE the auth layer so
        // the operator can open the viewer URL in a browser without
        // copy-pasting a bearer token. The two endpoints are:
        //   GET  /hitl/viewer/:id  — static HTML+JS viewer
        //   GET  /hitl/cdp/:id     — WebSocket CDP proxy
        // The viewer is still safe-by-default: an unknown :id returns 404
        // (the proxy checks the SessionRegistry; the page is generic
        // HTML and reveals nothing about other sessions).
        .route("/hitl/viewer/:id", get(hitl_viewer_page))
        .route("/hitl/cdp/:id", get(hitl_cdp_proxy))
        .layer(ServiceBuilder::new().layer(auth_layer))
        .with_state(state)
}
