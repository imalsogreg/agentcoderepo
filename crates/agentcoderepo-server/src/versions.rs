use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::auth::AuthAgent;
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

#[derive(Serialize)]
pub struct VersionEntry {
    pub version: String,
    pub commit_sha: String,
    pub yanked: bool,
    pub created_at: String,
}

impl TextFormat for VersionEntry {
    fn to_text(&self) -> String {
        let yanked = if self.yanked { " [yanked]" } else { "" };
        format!(
            "{}  {}  {}{yanked}\n",
            self.version,
            &self.commit_sha[..12.min(self.commit_sha.len())],
            self.created_at,
        )
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct VersionList(Vec<VersionEntry>);

impl TextFormat for VersionList {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no versions)\n".to_string();
        }
        self.0.iter().map(|v| v.to_text()).collect()
    }
}

/// GET /api/repos/{owner}/{repo}/versions
pub async fn list_versions(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
) -> Result<Negotiated<VersionList>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let repo_row = conn
        .query(
            "SELECT r.id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner, repo],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo_id: String = repo_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut rows = conn
        .query(
            "SELECT version, commit_sha, yanked, created_at FROM repo_versions
             WHERE repo_id = ?1
             ORDER BY created_at DESC",
            [repo_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut versions = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        versions.push(VersionEntry {
            version: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            commit_sha: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            yanked: row.get::<i64>(2).unwrap_or(0) != 0,
            created_at: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(VersionList(versions)))
}

/// POST /api/repos/{owner}/{repo}/versions/{version}/yank  (repo owner only)
pub async fn yank_version(
    State(state): State<Arc<AppState>>,
    agent: AuthAgent,
    axum::extract::Path((owner, repo, version)): axum::extract::Path<(String, String, String)>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify ownership
    let row = conn
        .query(
            "SELECT r.id, a.id as owner_id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner, repo],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let owner_id: String = row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if owner_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    let repo_id: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let affected = conn
        .execute(
            "UPDATE repo_versions SET yanked = 1 WHERE repo_id = ?1 AND version = ?2",
            [repo_id, version],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if affected == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::NO_CONTENT)
}
