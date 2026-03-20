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

/// A ref update parsed from the git receive-pack protocol.
#[derive(Debug, Clone)]
pub struct RefUpdate {
    pub old_sha: String,
    pub new_sha: String,
    pub ref_name: String,
}

/// Called before receive-pack to check if the agent is allowed to update
/// the given refs. Arguments: (owner, repo_name, agent_id, ref_updates).
/// Returns Ok(()) to allow, or Err(message) to reject.
pub type RefCheckHook = Arc<
    dyn Fn(
            String,
            String,
            String,
            Vec<RefUpdate>,
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
    /// Optional hook invoked before receive-pack to validate ref-level permissions.
    pub ref_check: Option<RefCheckHook>,
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
/// Buffers the request body to parse ref updates for access control,
/// then forwards to git receive-pack.
#[tracing::instrument(skip(state, request), fields(service = "receive-pack"))]
async fn receive_pack(
    State(state): State<GitState>,
    Path(RepoPath { owner, repo }): Path<RepoPath>,
    request: axum::extract::Request,
) -> Response {
    let repo_path = match repo::ensure_bare_repo(&state.repo_root, &owner, &repo).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    // Extract agent_id from request extensions (set by auth middleware)
    let agent_id = request
        .extensions()
        .get::<AgentIdExt>()
        .map(|e| e.0.clone())
        .unwrap_or_default();

    let body = request.into_body();

    // Buffer the body so we can parse ref updates before forwarding to git
    let body_bytes = match axum::body::to_bytes(body, 256 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to buffer request body");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    // Parse ref updates from the pkt-line protocol
    if let Some(ref_check) = &state.ref_check {
        let ref_updates = parse_ref_updates(&body_bytes);
        if !ref_updates.is_empty() {
            if let Err(msg) = ref_check(
                owner.clone(),
                repo.clone(),
                agent_id,
                ref_updates,
            )
            .await
            {
                tracing::warn!(error = %msg, "ref check rejected push");
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(format!("push rejected: {msg}\n")))
                    .unwrap();
            }
        }
    }

    // Snapshot the current HEAD before the push.
    let old_sha = rpc::current_head(&repo_path)
        .await
        .unwrap_or_else(|_| "0000000000000000000000000000000000000000".to_string());

    // Forward the buffered body to git receive-pack
    let body = Body::from(body_bytes);
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

// ---------------------------------------------------------------------------
// Agent ID extraction from request extensions
// ---------------------------------------------------------------------------

/// Extension type to pass agent identity from auth middleware to git handlers.
#[derive(Clone, Debug)]
pub struct AgentIdExt(pub String);

// ---------------------------------------------------------------------------
// Pkt-line ref update parsing
// ---------------------------------------------------------------------------

/// Parse ref update commands from the git receive-pack request body.
///
/// The format is pkt-line encoded:
///   <4-hex-len><old-sha> <new-sha> <refname>\0<capabilities>\n  (first line)
///   <4-hex-len><old-sha> <new-sha> <refname>\n                 (subsequent)
///   0000                                                       (flush)
fn parse_ref_updates(body: &[u8]) -> Vec<RefUpdate> {
    let mut updates = Vec::new();
    let mut pos = 0;

    while pos + 4 <= body.len() {
        let hex = &body[pos..pos + 4];
        let hex_str = match std::str::from_utf8(hex) {
            Ok(s) => s,
            Err(_) => break,
        };

        // Flush packet
        if hex_str == "0000" {
            break;
        }

        let pkt_len = match usize::from_str_radix(hex_str, 16) {
            Ok(n) => n,
            Err(_) => break,
        };

        if pkt_len < 4 || pos + pkt_len > body.len() {
            break;
        }

        let pkt_data = &body[pos + 4..pos + pkt_len];
        pos += pkt_len;

        // Parse: "<old-sha> <new-sha> <refname>[\0<capabilities>][\n]"
        let data_str = match std::str::from_utf8(pkt_data) {
            Ok(s) => s.trim_end_matches('\n'),
            Err(_) => continue,
        };

        // Strip capabilities after NUL
        let data_str = data_str.split('\0').next().unwrap_or(data_str);

        let parts: Vec<&str> = data_str.splitn(3, ' ').collect();
        if parts.len() == 3 && parts[0].len() == 40 && parts[1].len() == 40 {
            updates.push(RefUpdate {
                old_sha: parts[0].to_string(),
                new_sha: parts[1].to_string(),
                ref_name: parts[2].to_string(),
            });
        }
    }

    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_ref_update() {
        // Construct a pkt-line: "00850000...old 0000...new refs/heads/main\0 report-status\n"
        let old = "0000000000000000000000000000000000000000";
        let new = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let line = format!("{old} {new} refs/heads/main\0 report-status side-band-64k\n");
        let pkt_len = line.len() + 4;
        let pkt = format!("{pkt_len:04x}{line}");
        let body = format!("{pkt}0000");

        let updates = parse_ref_updates(body.as_bytes());
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].old_sha, old);
        assert_eq!(updates[0].new_sha, new);
        assert_eq!(updates[0].ref_name, "refs/heads/main");
    }

    #[test]
    fn parse_multiple_ref_updates() {
        let old = "0000000000000000000000000000000000000000";
        let new1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let new2 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let line1 = format!("{old} {new1} refs/heads/main\0 report-status\n");
        let pkt1_len = line1.len() + 4;
        let pkt1 = format!("{pkt1_len:04x}{line1}");

        let line2 = format!("{old} {new2} refs/changesets/abc\n");
        let pkt2_len = line2.len() + 4;
        let pkt2 = format!("{pkt2_len:04x}{line2}");

        let body = format!("{pkt1}{pkt2}0000");

        let updates = parse_ref_updates(body.as_bytes());
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].ref_name, "refs/heads/main");
        assert_eq!(updates[1].ref_name, "refs/changesets/abc");
    }
}
