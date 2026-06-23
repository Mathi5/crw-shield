use axum::{
    routing::{delete, get, post},
    Router,
};
use tower::ServiceBuilder;

use crate::handlers::{
    crawl_cancel, crawl_start, crawl_status, health, hitl_enqueue, hitl_result, hitl_solve, map,
    scrape, search,
};
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
        .route("/v2/crawl", post(crawl_start))
        .route("/v2/crawl/:id", get(crawl_status))
        .route("/v2/crawl/:id", delete(crawl_cancel))
        .route("/v2/map", post(map))
        .route("/v2/search", post(search))
        .layer(ServiceBuilder::new().layer(auth_layer))
        .with_state(state)
}
