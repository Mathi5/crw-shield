use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};

use crate::state::AppState;

/// Bearer-token auth middleware. When `AUTH_TOKEN` is unset, requests pass
/// through. When set, requests must include `Authorization: Bearer <token>`.
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = match state.config.auth_token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(next.run(req).await),
    };
    let provided = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    match provided {
        Some(token) if token == expected => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}
