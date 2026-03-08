mod pktline;
pub mod repo;
mod rpc;

pub use repo::ensure_bare_repo;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

/// Called after receive-pack completes successfully.
/// Arguments: (repo_path, owner, repo_name, old_sha, new_sha).
/// Returns Ok(()) to accept the push, or Err(message) to reject it
/// (e.g. semver violation).
pub type PostReceiveHook = Arc<
    dyn Fn(
            PathBuf,
            String,
            String,
            String,
            String,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// Shared state for git routes.
#[derive(Clone)]
pub struct GitState {
    pub repo_root: Arc<PathBuf>,
    /// Optional hook invoked after a successful receive-pack, before returning
    /// the response. Used to trigger indexing.
    pub post_receive: Option<PostReceiveHook>,
}

#[derive(Deserialize)]
struct InfoRefsQuery {
    service: String,
}

#[derive(Deserialize)]
struct RepoPath {
    owner: String,
    repo: String,
}

pub fn routes(state: GitState) -> Router {
    Router::new()
        .route("/{owner}/{repo}/info/refs", get(info_refs))
        .route("/{owner}/{repo}/git-upload-pack", post(upload_pack))
        .route("/{owner}/{repo}/git-receive-pack", post(receive_pack))
        .with_state(state)
}

/// GET /:owner/:repo/info/refs?service=git-upload-pack|git-receive-pack
#[tracing::instrument(skip(state, query), fields(service = %query.service))]
async fn info_refs(
    State(state): State<GitState>,
    Path(RepoPath { owner, repo }): Path<RepoPath>,
    Query(query): Query<InfoRefsQuery>,
) -> Response {
    let service = &query.service;
    if service != "git-upload-pack" && service != "git-receive-pack" {
        tracing::warn!("rejected invalid service");
        return StatusCode::BAD_REQUEST.into_response();
    }

    let repo_path = match repo::ensure_bare_repo(&state.repo_root, &owner, &repo).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let git_cmd = service.strip_prefix("git-").unwrap();
    let output = match rpc::advertise_refs(&repo_path, git_cmd).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "advertise_refs failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    tracing::debug!(response_bytes = output.len(), "advertise_refs complete");

    let body = pktline::wrap_advertisement(service, &output);
    let content_type = format!("application/x-{service}-advertisement");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap()
}

/// POST /:owner/:repo/git-receive-pack
///
/// Buffers the git response (small status lines), runs the post-receive
/// hook (indexing) if configured, then returns the response.
#[tracing::instrument(skip(state, body), fields(service = "receive-pack"))]
async fn receive_pack(
    State(state): State<GitState>,
    Path(RepoPath { owner, repo }): Path<RepoPath>,
    body: Body,
) -> Response {
    let repo_path = match repo::ensure_bare_repo(&state.repo_root, &owner, &repo).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    // Snapshot the current HEAD before the push.
    let old_sha = rpc::current_head(&repo_path)
        .await
        .unwrap_or_else(|_| "0000000000000000000000000000000000000000".to_string());

    // Buffer the receive-pack response (it's small — just status lines).
    let output = match rpc::stateless_rpc_buffered(&repo_path, "receive-pack", body).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "receive-pack failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Get new HEAD after the push.
    let new_sha = rpc::current_head(&repo_path)
        .await
        .unwrap_or_default();

    // Run post-receive hook (indexing + semver validation) before returning.
    if let Some(hook) = &state.post_receive {
        if !new_sha.is_empty() && new_sha != old_sha {
            if let Err(msg) = hook(
                repo_path,
                owner,
                repo,
                old_sha,
                new_sha,
            )
            .await
            {
                tracing::warn!(error = %msg, "post-receive hook rejected push");
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(format!("push rejected: {msg}\n")))
                    .unwrap();
            }
        }
    }

    let content_type = "application/x-git-receive-pack-result";
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(output))
        .unwrap()
}

/// POST /:owner/:repo/git-upload-pack
///
/// Streams the response (packfiles can be large).
#[tracing::instrument(skip(state, body), fields(service = "upload-pack"))]
async fn upload_pack(
    State(state): State<GitState>,
    Path(RepoPath { owner, repo }): Path<RepoPath>,
    body: Body,
) -> Response {
    let repo_path = match repo::ensure_bare_repo(&state.repo_root, &owner, &repo).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let response_body = match rpc::stateless_rpc_streaming(&repo_path, "upload-pack", body).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "upload-pack failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let content_type = "application/x-git-upload-pack-result";
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache")
        .body(response_body)
        .unwrap()
}
