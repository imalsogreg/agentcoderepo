use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

const MAX_LIMIT: usize = 500;
const DEFAULT_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
pub struct LogParams {
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Git ref to start from (default: HEAD).
    #[serde(default = "default_ref")]
    pub r#ref: String,
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

fn default_ref() -> String {
    "HEAD".to_string()
}

#[derive(Serialize, Clone)]
pub struct LogEntry {
    pub commit_id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub date: String,
    pub message: String,
}

impl TextFormat for LogEntry {
    fn to_text(&self) -> String {
        format!(
            "{} {} {}\n  {}\n",
            &self.commit_id[..12.min(self.commit_id.len())],
            self.author,
            self.date,
            self.message,
        )
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct LogResponse(Vec<LogEntry>);

impl TextFormat for LogResponse {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no commits)\n".to_string();
        }
        self.0.iter().map(|e| e.to_text()).collect()
    }
}

/// GET /api/repos/{owner}/{repo}/log
pub async fn get_log(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    Query(params): Query<LogParams>,
) -> Result<Negotiated<LogResponse>, StatusCode> {
    let limit = params.limit.min(MAX_LIMIT);
    let repo_path = state.repo_root.join(&owner).join(format!("{repo}.git"));

    if !repo_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    // Validate the ref name to prevent injection
    // Only allow alphanumeric, /, -, _, .
    if !params.r#ref.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c)) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let ref_name = params.r#ref.clone();
    let entries = tokio::task::spawn_blocking(move || {
        git_log(&repo_path, &ref_name, limit)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(LogResponse(entries)))
}

/// Shell out to `git log` and parse the output.
fn git_log(
    repo_path: &std::path::Path,
    git_ref: &str,
    limit: usize,
) -> Result<Vec<LogEntry>, String> {
    // Use NUL-separated fields to avoid escaping issues
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--format=%H%x00%P%x00%an%x00%aI%x00%s",
            &format!("--max-count={limit}"),
            git_ref,
        ])
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("failed to run git log: {e}"))?;

    if !output.status.success() {
        // If the ref doesn't exist (empty repo), return empty list
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(5, '\0').collect();
        if fields.len() < 5 {
            continue;
        }

        let parents: Vec<String> = if fields[1].is_empty() {
            Vec::new()
        } else {
            fields[1].split(' ').map(|s| s.to_string()).collect()
        };

        entries.push(LogEntry {
            commit_id: fields[0].to_string(),
            parents,
            author: fields[2].to_string(),
            date: fields[3].to_string(),
            message: fields[4].to_string(),
        });
    }

    Ok(entries)
}
