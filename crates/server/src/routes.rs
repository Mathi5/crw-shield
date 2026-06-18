use axum::{routing::get, routing::post, Router};
use tower::ServiceBuilder;

use crate::handlers::{health, scrape};
use crate::middleware::auth_middleware;
use crate::state::AppState;

pub fn build_router(state: AppState) -> Router {
    let auth_layer = axum::middleware::from_fn_with_state(state.clone(), auth_middleware);
    Router::new()
        .route("/health", get(health))
        .route("/v2/scrape", post(scrape))
        .layer(ServiceBuilder::new().layer(auth_layer))
        .with_state(state)
}
