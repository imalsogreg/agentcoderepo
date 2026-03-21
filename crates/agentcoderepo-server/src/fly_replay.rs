use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// Middleware that replays write requests to the primary Fly.io instance.
///
/// On non-primary instances, mutating requests (POST, PUT, PATCH, DELETE)
/// are not executed. Instead, the middleware returns a `fly-replay` header
/// that tells Fly's proxy to replay the request on the primary machine.
///
/// Read-only requests (GET, HEAD, OPTIONS) are always served locally.
///
/// This is a no-op if the instance is the primary (or if not running on Fly.io).
pub async fn replay_writes_to_primary(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // Primary instances (and local dev) always handle everything
    if state.is_primary {
        return next.run(request).await;
    }

    // Read-only methods are always served locally from the embedded replica
    let method = request.method().clone();
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return next.run(request).await;
    }

    // OAuth callbacks must be handled locally (they set cookies)
    let path = request.uri().path().to_string();
    if path.starts_with("/auth/") || path.starts_with("/login/") || path == "/logout" {
        return next.run(request).await;
    }

    // Write request on a non-primary instance → replay to primary
    let replay_target = if let Some(ref machine_id) = state.primary_machine_id {
        format!("instance={machine_id}")
    } else {
        // Fall back to region-based replay
        "region=sjc".to_string()
    };

    tracing::debug!(
        method = %method,
        path = %path,
        replay = %replay_target,
        "replaying write to primary"
    );

    Response::builder()
        .status(StatusCode::CONFLICT)
        .header("fly-replay", replay_target)
        .body(Body::empty())
        .unwrap()
        .into_response()
}
