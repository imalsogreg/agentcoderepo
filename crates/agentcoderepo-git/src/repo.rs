use std::path::{Component, Path, PathBuf};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio::process::Command;

/// Error type for repo operations, convertible to HTTP responses.
#[derive(Debug)]
pub enum RepoError {
    InvalidPath,
    InitFailed(String),
}

impl IntoResponse for RepoError {
    fn into_response(self) -> Response {
        match self {
            RepoError::InvalidPath => {
                (StatusCode::BAD_REQUEST, "invalid owner or repo name").into_response()
            }
            RepoError::InitFailed(msg) => {
                tracing::error!("git init --bare failed: {msg}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Resolve the bare repo path, creating it atomically if it doesn't exist.
///
/// Owner and repo names are validated to prevent path traversal.
#[tracing::instrument(skip(root))]
pub async fn ensure_bare_repo(
    root: &Path,
    owner: &str,
    repo: &str,
) -> Result<PathBuf, RepoError> {
    validate_name(owner)?;
    validate_name(repo)?;

    let repo_path = root.join(owner).join(format!("{repo}.git"));

    if !repo_path.exists() {
        // Use a temp dir + rename for atomic creation, avoiding races
        // where two requests both see !exists() and both try to init.
        let tmp_path = root.join(format!(".tmp-{owner}-{repo}-{}", std::process::id()));
        if let Err(e) = tokio::fs::create_dir_all(&tmp_path).await {
            return Err(RepoError::InitFailed(e.to_string()));
        }

        let output = Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&tmp_path)
            .output()
            .await
            .map_err(|e| RepoError::InitFailed(e.to_string()))?;

        if !output.status.success() {
            let _ = tokio::fs::remove_dir_all(&tmp_path).await;
            return Err(RepoError::InitFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        // Ensure parent dir exists, then atomically move into place.
        let parent = repo_path.parent().unwrap();
        let _ = tokio::fs::create_dir_all(parent).await;

        // rename is atomic on the same filesystem. If another request beat us,
        // the rename fails and we just clean up our temp dir.
        if tokio::fs::rename(&tmp_path, &repo_path).await.is_err() {
            let _ = tokio::fs::remove_dir_all(&tmp_path).await;
            // The repo now exists (created by another request), which is fine.
        }
    }

    Ok(repo_path)
}

/// Reject names that could escape the repo root via path traversal.
fn validate_name(name: &str) -> Result<(), RepoError> {
    if name.is_empty() {
        return Err(RepoError::InvalidPath);
    }

    let path = Path::new(name);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(RepoError::InvalidPath),
        }
    }

    // Extra safety: reject anything with dots at the start (hidden files, .., .)
    if name.starts_with('.') {
        return Err(RepoError::InvalidPath);
    }

    Ok(())
}
