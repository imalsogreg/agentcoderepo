use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::AuthAgent;
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChangesetPath {
    pub owner: String,
    pub repo: String,
    pub changeset_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoPath {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateChangeset {
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct ListChangesetsParams {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "proposed".to_string()
}

#[derive(Serialize)]
pub struct ChangesetResponse {
    pub id: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub author_name: String,
    pub description: String,
    pub ref_name: String,
    pub base_commit: String,
    pub status: String,
    pub push_url: String,
    pub created_at: String,
}

impl TextFormat for ChangesetResponse {
    fn to_text(&self) -> String {
        format!(
            "[{}] {} by {}\n  ref: {}\n  base: {}\n  push: git push {} {}\n",
            self.status,
            self.description,
            self.author_name,
            self.ref_name,
            &self.base_commit[..12.min(self.base_commit.len())],
            self.push_url,
            self.ref_name,
        )
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct ChangesetList(Vec<ChangesetResponse>);

impl TextFormat for ChangesetList {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no changesets)\n".to_string();
        }
        self.0.iter().map(|c| c.to_text()).collect()
    }
}

#[derive(Serialize)]
pub struct ChangesetDetail {
    #[serde(flatten)]
    pub changeset: ChangesetResponse,
    pub commits: Vec<crate::log::LogEntry>,
}

impl TextFormat for ChangesetDetail {
    fn to_text(&self) -> String {
        let mut s = self.changeset.to_text();
        if self.commits.is_empty() {
            s.push_str("  (no commits pushed yet)\n");
        } else {
            s.push_str(&format!("  {} commit{}:\n", self.commits.len(), if self.commits.len() == 1 { "" } else { "s" }));
            for c in &self.commits {
                s.push_str(&format!("    {} {}\n", &c.commit_id[..12.min(c.commit_id.len())], c.message));
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn lookup_repo(state: &AppState, owner: &str, repo_name: &str) -> Option<(String, String)> {
    let conn = state.db.connect().ok()?;
    let row = conn
        .query(
            "SELECT r.id, a.id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner.to_string(), repo_name.to_string()],
        )
        .await
        .ok()?
        .next()
        .await
        .ok()??;
    Some((
        row.get::<String>(0).ok()?,
        row.get::<String>(1).ok()?, // owner agent_id
    ))
}

/// Get the current HEAD sha of a bare repo, or all zeros if empty.
fn git_head_sync(repo_path: &Path) -> String {
    let zeros = "0000000000000000000000000000000000000000".to_string();
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                sha
            } else {
                zeros
            }
        }
        _ => zeros,
    }
}

/// Get commits on a ref that aren't on another ref.
fn git_log_range_sync(repo_path: &Path, base: &str, tip: &str) -> Vec<crate::log::LogEntry> {
    let range = format!("{base}..{tip}");
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--format=%H%x00%P%x00%an%x00%aI%x00%s",
            &range,
        ])
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

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
        entries.push(crate::log::LogEntry {
            commit_id: fields[0].to_string(),
            parents,
            author: fields[2].to_string(),
            date: fields[3].to_string(),
            message: fields[4].to_string(),
        });
    }
    entries
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/changesets — create a changeset.
/// Any authenticated agent can propose a changeset on any repo.
pub async fn create_changeset(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<RepoPath>,
    Json(body): Json<CreateChangeset>,
) -> Result<Negotiated<ChangesetResponse>, StatusCode> {
    if body.description.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (repo_id, _owner_agent_id) = lookup_repo(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo_path = state.repo_root.join(&path.owner).join(format!("{}.git", &path.repo));

    // Get current HEAD as base commit
    let base_commit = tokio::task::spawn_blocking({
        let repo_path = repo_path.clone();
        move || git_head_sync(&repo_path)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let id = uuid::Uuid::new_v4().to_string();
    let ref_name = format!("refs/changesets/{id}");

    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute(
        "INSERT INTO changesets (id, repo_id, author_id, description, ref_name, base_commit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        [
            id.clone(),
            repo_id,
            agent.agent_id.to_string(),
            body.description.clone(),
            ref_name.clone(),
            base_commit.clone(),
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let push_url = format!("/git/{}/{}", path.owner, path.repo);

    Ok(neg.created(ChangesetResponse {
        id,
        repo_owner: path.owner,
        repo_name: path.repo,
        author_name: agent.agent_name,
        description: body.description,
        ref_name,
        base_commit,
        status: "proposed".to_string(),
        push_url,
        created_at: String::new(),
    }))
}

/// GET /api/repos/{owner}/{repo}/changesets — list changesets.
pub async fn list_changesets(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<RepoPath>,
    Query(params): Query<ListChangesetsParams>,
) -> Result<Negotiated<ChangesetList>, StatusCode> {
    let (repo_id, _) = lookup_repo(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (where_clause, bind_values) = match params.status.as_str() {
        "all" => (
            "WHERE c.repo_id = ?1".to_string(),
            vec![repo_id],
        ),
        status @ ("proposed" | "accepted" | "rejected" | "withdrawn") => (
            "WHERE c.repo_id = ?1 AND c.status = ?2".to_string(),
            vec![repo_id, status.to_string()],
        ),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let query = format!(
        "SELECT c.id, a.name, c.description, c.ref_name, c.base_commit, c.status, c.created_at
         FROM changesets c
         JOIN agents a ON c.author_id = a.id
         {where_clause}
         ORDER BY c.created_at DESC"
    );

    let mut rows = if bind_values.len() == 1 {
        conn.query(&query, [bind_values[0].clone()])
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        conn.query(&query, [bind_values[0].clone(), bind_values[1].clone()])
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    let push_url = format!("/git/{}/{}", path.owner, path.repo);
    let mut changesets = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        changesets.push(ChangesetResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            repo_owner: path.owner.clone(),
            repo_name: path.repo.clone(),
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            description: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            ref_name: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            base_commit: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            status: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            push_url: push_url.clone(),
            created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(ChangesetList(changesets)))
}

/// GET /api/repos/{owner}/{repo}/changesets/{changeset_id} — get changeset with diff.
pub async fn get_changeset(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<ChangesetPath>,
) -> Result<Negotiated<ChangesetDetail>, StatusCode> {
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT c.id, a.name, c.description, c.ref_name, c.base_commit, c.status, c.created_at,
                    r.name as repo_name, owner.name as owner_name
             FROM changesets c
             JOIN agents a ON c.author_id = a.id
             JOIN repos r ON c.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE c.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let ref_name: String = row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let base_commit: String = row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let push_url = format!("/git/{}/{}", path.owner, path.repo);

    // Get commits in the changeset (commits on ref not on base)
    let repo_path = state.repo_root.join(&path.owner).join(format!("{}.git", &path.repo));
    let ref_clone = ref_name.clone();
    let base_clone = base_commit.clone();
    let commits = tokio::task::spawn_blocking(move || {
        git_log_range_sync(&repo_path, &base_clone, &ref_clone)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(ChangesetDetail {
        changeset: ChangesetResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            repo_owner: path.owner,
            repo_name: path.repo,
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            description: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            ref_name,
            base_commit,
            status: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            push_url,
            created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        },
        commits,
    }))
}

// ---------------------------------------------------------------------------
// Accept / Reject / Withdraw
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AcceptResponse {
    pub status: String,
    pub new_head: String,
}

impl TextFormat for AcceptResponse {
    fn to_text(&self) -> String {
        format!("accepted, new head: {}\n", self.new_head)
    }
}

/// POST /api/repos/{owner}/{repo}/changesets/{changeset_id}/accept
/// Repo owner cherry-picks the changeset commits onto main.
pub async fn accept_changeset(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<ChangesetPath>,
) -> Result<Negotiated<AcceptResponse>, StatusCode> {
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Load changeset + verify repo ownership
    let row = conn
        .query(
            "SELECT c.id, c.ref_name, c.base_commit, r.id as repo_id, owner.id as owner_id
             FROM changesets c
             JOIN repos r ON c.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE c.id = ?1 AND owner.name = ?2 AND r.name = ?3 AND c.status = 'proposed'",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let owner_id: String = row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if owner_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    let ref_name: String = row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let base_commit: String = row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let repo_path = state.repo_root.join(&path.owner).join(format!("{}.git", &path.repo));

    // Get the commits to cherry-pick (in chronological order — oldest first)
    let ref_clone = ref_name.clone();
    let base_clone = base_commit.clone();
    let rp = repo_path.clone();
    let commits = tokio::task::spawn_blocking(move || {
        git_log_range_sync(&rp, &base_clone, &ref_clone)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if commits.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Cherry-pick onto main (oldest first)
    let commit_shas: Vec<String> = commits.iter().rev().map(|c| c.commit_id.clone()).collect();
    let rp = repo_path.clone();
    let cherry_pick_result = tokio::task::spawn_blocking(move || {
        cherry_pick_onto_main(&rp, &commit_shas)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let new_head = match cherry_pick_result {
        Ok(sha) => sha,
        Err(msg) => {
            tracing::warn!(error = %msg, "cherry-pick failed");
            return Err(StatusCode::CONFLICT);
        }
    };

    // Clean up the changeset ref
    let rp = repo_path.clone();
    let ref_to_delete = ref_name.clone();
    let _ = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["update-ref", "-d", &ref_to_delete])
            .current_dir(&rp)
            .output()
    })
    .await;

    // Update changeset status
    conn.execute(
        "UPDATE changesets SET status = 'accepted', updated_at = datetime('now') WHERE id = ?1",
        [path.changeset_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(AcceptResponse {
        status: "accepted".to_string(),
        new_head,
    }))
}

/// POST /api/repos/{owner}/{repo}/changesets/{changeset_id}/reject (owner only)
pub async fn reject_changeset(
    State(state): State<Arc<AppState>>,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<ChangesetPath>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT owner.id as owner_id, c.ref_name
             FROM changesets c
             JOIN repos r ON c.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE c.id = ?1 AND owner.name = ?2 AND r.name = ?3 AND c.status = 'proposed'",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let owner_id: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if owner_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    // Clean up ref
    let ref_name: String = row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let repo_path = state.repo_root.join(&path.owner).join(format!("{}.git", &path.repo));
    let _ = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["update-ref", "-d", &ref_name])
            .current_dir(&repo_path)
            .output()
    })
    .await;

    conn.execute(
        "UPDATE changesets SET status = 'rejected', updated_at = datetime('now') WHERE id = ?1",
        [path.changeset_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/repos/{owner}/{repo}/changesets/{changeset_id}/withdraw (author only)
pub async fn withdraw_changeset(
    State(state): State<Arc<AppState>>,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<ChangesetPath>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT c.author_id, c.ref_name
             FROM changesets c
             JOIN repos r ON c.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE c.id = ?1 AND owner.name = ?2 AND r.name = ?3 AND c.status = 'proposed'",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let author_id: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if author_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    // Clean up ref
    let ref_name: String = row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let repo_path = state.repo_root.join(&path.owner).join(format!("{}.git", &path.repo));
    let _ = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["update-ref", "-d", &ref_name])
            .current_dir(&repo_path)
            .output()
    })
    .await;

    conn.execute(
        "UPDATE changesets SET status = 'withdrawn', updated_at = datetime('now') WHERE id = ?1",
        [path.changeset_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Git operations for acceptance
// ---------------------------------------------------------------------------

/// Cherry-pick commits onto main in a bare repo.
///
/// Uses a temporary worktree to perform the cherry-pick, then
/// updates the main ref. Returns the new HEAD sha.
fn cherry_pick_onto_main(repo_path: &PathBuf, commit_shas: &[String]) -> Result<String, String> {
    // Create a temporary worktree for the cherry-pick operation
    let worktree_id = uuid::Uuid::new_v4().to_string();
    let worktree_path = std::env::temp_dir().join(format!("agentcoderepo-cherry-pick-{worktree_id}"));

    // Add worktree at current main
    let output = std::process::Command::new("git")
        .args(["worktree", "add", worktree_path.to_str().unwrap(), "main"])
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("failed to create worktree: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Cherry-pick each commit
    for sha in commit_shas {
        let output = std::process::Command::new("git")
            .args(["cherry-pick", sha])
            .current_dir(&worktree_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("failed to cherry-pick {sha}: {e}"))?;

        if !output.status.success() {
            // Abort the cherry-pick and clean up
            let _ = std::process::Command::new("git")
                .args(["cherry-pick", "--abort"])
                .current_dir(&worktree_path)
                .output();
            let _ = std::process::Command::new("git")
                .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap()])
                .current_dir(repo_path)
                .output();
            return Err(format!(
                "cherry-pick conflict on {}: {}",
                &sha[..12.min(sha.len())],
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }

    // Get new HEAD
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&worktree_path)
        .output()
        .map_err(|e| format!("failed to get new HEAD: {e}"))?;

    let new_head = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Update main in the bare repo to point to the new HEAD
    let output = std::process::Command::new("git")
        .args(["update-ref", "refs/heads/main", &new_head])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("failed to update main ref: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "git update-ref failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Clean up worktree
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap()])
        .current_dir(repo_path)
        .output();
    // Also remove the temp directory in case worktree remove didn't clean it
    let _ = std::fs::remove_dir_all(&worktree_path);

    Ok(new_head)
}
