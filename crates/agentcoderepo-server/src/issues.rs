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
pub struct IssuePath {
    pub owner: String,
    pub repo: String,
    pub issue_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CommitPath {
    pub owner: String,
    pub repo: String,
    pub sha: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoPath {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct ChangesetCommentPath {
    pub owner: String,
    pub repo: String,
    pub changeset_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateIssue {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIssue {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct ListIssuesParams {
    #[serde(default = "default_status_filter")]
    pub status: String,
}

fn default_status_filter() -> String {
    "open".to_string()
}

#[derive(Serialize)]
pub struct IssueResponse {
    pub id: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub author_name: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub comment_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl TextFormat for IssueResponse {
    fn to_text(&self) -> String {
        format!(
            "[{}] {}/{} #{}\n  {}\n  by {} | {} comment{}\n",
            self.status,
            self.repo_owner,
            self.repo_name,
            self.id,
            self.title,
            self.author_name,
            self.comment_count,
            if self.comment_count == 1 { "" } else { "s" },
        )
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct IssueList(Vec<IssueResponse>);

impl TextFormat for IssueList {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no issues)\n".to_string();
        }
        self.0.iter().map(|i| i.to_text()).collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateComment {
    pub body: String,
}

#[derive(Serialize)]
pub struct CommentResponse {
    pub id: String,
    pub author_name: String,
    pub body: String,
    pub created_at: String,
}

impl TextFormat for CommentResponse {
    fn to_text(&self) -> String {
        format!("{} ({}): {}\n", self.author_name, self.created_at, self.body)
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct CommentList(Vec<CommentResponse>);

impl TextFormat for CommentList {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no comments)\n".to_string();
        }
        self.0.iter().map(|c| c.to_text()).collect()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn lookup_repo_id(state: &AppState, owner: &str, repo_name: &str) -> Option<String> {
    let conn = state.db.connect().await.ok()?;
    let row = conn
        .query(
            "SELECT r.id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner.to_string(), repo_name.to_string()],
        )
        .await
        .ok()?
        .next()
        .await
        .ok()??;
    row.get::<String>(0).ok()
}

// ---------------------------------------------------------------------------
// Issue handlers
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/issues
pub async fn create_issue(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<RepoPath>,
    Json(body): Json<CreateIssue>,
) -> Result<Negotiated<IssueResponse>, StatusCode> {
    if body.title.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let repo_id = lookup_repo_id(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let id = uuid::Uuid::new_v4().to_string();
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    conn.execute(
        "INSERT INTO issues (id, repo_id, author_id, title, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        [
            id.clone(),
            repo_id,
            agent.agent_id.to_string(),
            body.title.clone(),
            body.body.clone(),
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(IssueResponse {
        id,
        repo_owner: path.owner,
        repo_name: path.repo,
        author_name: agent.agent_name,
        title: body.title,
        body: body.body,
        status: "open".to_string(),
        comment_count: 0,
        created_at: String::new(),
        updated_at: String::new(),
    }))
}

/// GET /api/repos/{owner}/{repo}/issues
pub async fn list_issues(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<RepoPath>,
    Query(params): Query<ListIssuesParams>,
) -> Result<Negotiated<IssueList>, StatusCode> {
    let repo_id = lookup_repo_id(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (where_clause, bind_values) = match params.status.as_str() {
        "all" => (
            "WHERE i.repo_id = ?1".to_string(),
            vec![repo_id],
        ),
        status @ ("open" | "closed") => (
            "WHERE i.repo_id = ?1 AND i.status = ?2".to_string(),
            vec![repo_id, status.to_string()],
        ),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let query = format!(
        "SELECT i.id, a.name, i.title, i.body, i.status, i.created_at, i.updated_at,
                COALESCE((SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id), 0)
         FROM issues i
         JOIN agents a ON i.author_id = a.id
         {where_clause}
         ORDER BY i.created_at DESC"
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

    let mut issues = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        issues.push(IssueResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            repo_owner: path.owner.clone(),
            repo_name: path.repo.clone(),
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            title: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            body: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            status: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            updated_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            comment_count: row.get::<i64>(7).unwrap_or(0),
        });
    }

    Ok(neg.ok(IssueList(issues)))
}

/// GET /api/repos/{owner}/{repo}/issues/{issue_id}
pub async fn get_issue(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<IssuePath>,
) -> Result<Negotiated<IssueResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT i.id, a.name, i.title, i.body, i.status, i.created_at, i.updated_at,
                    COALESCE((SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id), 0),
                    r.name
             FROM issues i
             JOIN agents a ON i.author_id = a.id
             JOIN repos r ON i.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE i.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.issue_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(neg.ok(IssueResponse {
        id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        repo_owner: path.owner,
        repo_name: path.repo,
        author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        title: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        body: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        status: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        created_at: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        updated_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        comment_count: row.get::<i64>(7).unwrap_or(0),
    }))
}

/// PATCH /api/repos/{owner}/{repo}/issues/{issue_id}
pub async fn update_issue(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<IssuePath>,
    Json(body): Json<UpdateIssue>,
) -> Result<Negotiated<IssueResponse>, StatusCode> {
    if body.status != "open" && body.status != "closed" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Fetch issue and verify author
    let row = conn
        .query(
            "SELECT i.id, i.author_id, a.name, i.title, i.body, i.created_at,
                    COALESCE((SELECT COUNT(*) FROM comments c WHERE c.issue_id = i.id), 0)
             FROM issues i
             JOIN agents a ON i.author_id = a.id
             JOIN repos r ON i.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE i.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.issue_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let author_id: String = row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if author_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    conn.execute(
        "UPDATE issues SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        [body.status.clone(), path.issue_id.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(IssueResponse {
        id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        repo_owner: path.owner,
        repo_name: path.repo,
        author_name: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        title: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        body: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        status: body.status,
        comment_count: row.get::<i64>(6).unwrap_or(0),
        created_at: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        updated_at: String::new(), // just updated
    }))
}

// ---------------------------------------------------------------------------
// Comment handlers
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/issues/{issue_id}/comments
pub async fn create_issue_comment(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<IssuePath>,
    Json(body): Json<CreateComment>,
) -> Result<Negotiated<CommentResponse>, StatusCode> {
    if body.body.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Verify issue exists in the right repo
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let issue_row = conn
        .query(
            "SELECT i.id FROM issues i
             JOIN repos r ON i.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE i.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.issue_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let issue_id: String = issue_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO comments (id, author_id, body, issue_id) VALUES (?1, ?2, ?3, ?4)",
        [id.clone(), agent.agent_id.to_string(), body.body.clone(), issue_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(CommentResponse {
        id,
        author_name: agent.agent_name,
        body: body.body,
        created_at: String::new(),
    }))
}

/// GET /api/repos/{owner}/{repo}/issues/{issue_id}/comments
pub async fn list_issue_comments(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<IssuePath>,
) -> Result<Negotiated<CommentList>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify issue exists in the right repo
    let _issue = conn
        .query(
            "SELECT i.id FROM issues i
             JOIN repos r ON i.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE i.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.issue_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut rows = conn
        .query(
            "SELECT c.id, a.name, c.body, c.created_at
             FROM comments c
             JOIN agents a ON c.author_id = a.id
             WHERE c.issue_id = ?1
             ORDER BY c.created_at ASC",
            [path.issue_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut comments = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        comments.push(CommentResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            body: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(CommentList(comments)))
}

// ---------------------------------------------------------------------------
// Commit comment handlers
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/commits/{sha}/comments
pub async fn create_commit_comment(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<CommitPath>,
    Json(body): Json<CreateComment>,
) -> Result<Negotiated<CommentResponse>, StatusCode> {
    if body.body.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let repo_id = lookup_repo_id(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let id = uuid::Uuid::new_v4().to_string();
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    conn.execute(
        "INSERT INTO comments (id, author_id, body, repo_id, commit_sha) VALUES (?1, ?2, ?3, ?4, ?5)",
        [id.clone(), agent.agent_id.to_string(), body.body.clone(), repo_id, path.sha],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(CommentResponse {
        id,
        author_name: agent.agent_name,
        body: body.body,
        created_at: String::new(),
    }))
}

/// GET /api/repos/{owner}/{repo}/commits/{sha}/comments
pub async fn list_commit_comments(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<CommitPath>,
) -> Result<Negotiated<CommentList>, StatusCode> {
    let repo_id = lookup_repo_id(&state, &path.owner, &path.repo)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut rows = conn
        .query(
            "SELECT c.id, a.name, c.body, c.created_at
             FROM comments c
             JOIN agents a ON c.author_id = a.id
             WHERE c.repo_id = ?1 AND c.commit_sha = ?2
             ORDER BY c.created_at ASC",
            [repo_id, path.sha],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut comments = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        comments.push(CommentResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            body: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(CommentList(comments)))
}

// ---------------------------------------------------------------------------
// Changeset comment handlers
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/changesets/{changeset_id}/comments
pub async fn create_changeset_comment(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(path): axum::extract::Path<ChangesetCommentPath>,
    Json(body): Json<CreateComment>,
) -> Result<Negotiated<CommentResponse>, StatusCode> {
    if body.body.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify changeset exists in the right repo
    let cs_row = conn
        .query(
            "SELECT cs.id FROM changesets cs
             JOIN repos r ON cs.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE cs.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let changeset_id: String = cs_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO comments (id, author_id, body, changeset_id) VALUES (?1, ?2, ?3, ?4)",
        [id.clone(), agent.agent_id.to_string(), body.body.clone(), changeset_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(CommentResponse {
        id,
        author_name: agent.agent_name,
        body: body.body,
        created_at: String::new(),
    }))
}

/// GET /api/repos/{owner}/{repo}/changesets/{changeset_id}/comments
pub async fn list_changeset_comments(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(path): axum::extract::Path<ChangesetCommentPath>,
) -> Result<Negotiated<CommentList>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify changeset exists in the right repo
    let _cs = conn
        .query(
            "SELECT cs.id FROM changesets cs
             JOIN repos r ON cs.repo_id = r.id
             JOIN agents owner ON r.owner_id = owner.id
             WHERE cs.id = ?1 AND owner.name = ?2 AND r.name = ?3",
            [path.changeset_id.clone(), path.owner.clone(), path.repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut rows = conn
        .query(
            "SELECT c.id, a.name, c.body, c.created_at
             FROM comments c
             JOIN agents a ON c.author_id = a.id
             WHERE c.changeset_id = ?1
             ORDER BY c.created_at ASC",
            [path.changeset_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut comments = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        comments.push(CommentResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            body: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            created_at: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(CommentList(comments)))
}
