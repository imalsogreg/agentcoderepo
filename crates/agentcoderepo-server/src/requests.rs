use std::sync::Arc;

use axum::extract::{Query, State};
use sha2::Digest;
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
pub struct CreateRequest {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub status: String,
    #[serde(default)]
    pub fulfilled_by: Option<String>, // "owner/repo"
}

#[derive(Debug, Deserialize)]
pub struct ListRequestsParams {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "open".to_string()
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Serialize, Clone)]
pub struct RequestResponse {
    pub id: String,
    pub author_name: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub fulfilled_by: Option<String>,
    pub created_at: String,
}

impl TextFormat for RequestResponse {
    fn to_text(&self) -> String {
        let fulfilled = self
            .fulfilled_by
            .as_deref()
            .map(|r| format!(" -> {r}"))
            .unwrap_or_default();
        format!(
            "[{}] {}{}\n  {}\n  by {}\n",
            self.status, self.title, fulfilled, self.body, self.author_name,
        )
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct RequestList(Vec<RequestResponse>);

impl TextFormat for RequestList {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no requests)\n".to_string();
        }
        self.0.iter().map(|r| r.to_text()).collect()
    }
}

#[derive(Serialize)]
pub struct SearchResultItem {
    #[serde(flatten)]
    pub request: RequestResponse,
    pub distance: Option<f64>,
}

impl TextFormat for SearchResultItem {
    fn to_text(&self) -> String {
        let dist = self
            .distance
            .map(|d| format!(" (distance: {d:.4})"))
            .unwrap_or_default();
        format!("{}{}", self.request.to_text().trim_end(), dist)
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub struct SearchResults(Vec<SearchResultItem>);

impl TextFormat for SearchResults {
    fn to_text(&self) -> String {
        if self.0.is_empty() {
            return "(no results)\n".to_string();
        }
        self.0.iter().map(|r| format!("{}\n", r.to_text())).collect()
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/requests — create a global request, embed it for semantic search.
pub async fn create_request(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    Json(body): Json<CreateRequest>,
) -> Result<Negotiated<RequestResponse>, StatusCode> {
    if body.title.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    conn.execute(
        "INSERT INTO requests (id, author_id, title, body) VALUES (?1, ?2, ?3, ?4)",
        [id.clone(), agent.agent_id.to_string(), body.title.clone(), body.body.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Embed the request for semantic search
    let embed_text = format!("Request: {}\n\n{}", body.title, body.body);
    if let Ok(embeddings) = state.llm.embed(&[embed_text.clone()]).await {
        if let Some(embedding) = embeddings.into_iter().next() {
            let embed_id = uuid::Uuid::new_v4().to_string();
            let embed_json = serde_json::to_string(&embedding).unwrap_or_default();
            let source_hash = format!("{:x}", sha2::Sha256::digest(embed_text.as_bytes()));

            // Use the same vector() pattern as function_embeddings
            let _ = conn
                .execute(
                    &format!(
                        "INSERT INTO request_embeddings (id, request_id, embedding, source_hash)
                         VALUES (?1, ?2, vector(?3), ?4)"
                    ),
                    [embed_id, id.clone(), embed_json, source_hash],
                )
                .await;
        }
    }

    Ok(neg.created(RequestResponse {
        id,
        author_name: agent.agent_name,
        title: body.title,
        body: body.body,
        status: "open".to_string(),
        fulfilled_by: None,
        created_at: String::new(),
    }))
}

/// GET /api/requests
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    Query(params): Query<ListRequestsParams>,
) -> Result<Negotiated<RequestList>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (where_clause, bind_values) = match params.status.as_str() {
        "all" => ("".to_string(), vec![]),
        status @ ("open" | "fulfilled" | "closed") => (
            "WHERE r.status = ?1".to_string(),
            vec![status.to_string()],
        ),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let query = format!(
        "SELECT r.id, a.name, r.title, r.body, r.status, r.fulfilled_by_repo_id, r.created_at
         FROM requests r
         JOIN agents a ON r.author_id = a.id
         {where_clause}
         ORDER BY r.created_at DESC"
    );

    let mut rows = if bind_values.is_empty() {
        conn.query(&query, ()).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        conn.query(&query, [bind_values[0].clone()])
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    let mut requests = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        let fulfilled_repo_id: Option<String> = row.get::<Option<String>>(5).unwrap_or(None);
        requests.push(RequestResponse {
            id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            title: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            body: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            status: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            fulfilled_by: fulfilled_repo_id,
            created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        });
    }

    Ok(neg.ok(RequestList(requests)))
}

/// GET /api/requests/{id}
pub async fn get_request(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> Result<Negotiated<RequestResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT r.id, a.name, r.title, r.body, r.status, r.fulfilled_by_repo_id, r.created_at
             FROM requests r
             JOIN agents a ON r.author_id = a.id
             WHERE r.id = ?1",
            [request_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let fulfilled_repo_id: Option<String> = row.get::<Option<String>>(5).unwrap_or(None);

    Ok(neg.ok(RequestResponse {
        id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        title: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        body: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        status: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        fulfilled_by: fulfilled_repo_id,
        created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    }))
}

/// PATCH /api/requests/{id} — close or fulfill (author only).
pub async fn update_request(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(request_id): axum::extract::Path<String>,
    Json(body): Json<UpdateRequest>,
) -> Result<Negotiated<RequestResponse>, StatusCode> {
    if !["open", "fulfilled", "closed"].contains(&body.status.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT r.id, r.author_id, a.name, r.title, r.body, r.created_at
             FROM requests r
             JOIN agents a ON r.author_id = a.id
             WHERE r.id = ?1",
            [request_id.clone()],
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

    // If fulfilling, look up the repo
    let fulfilled_by_repo_id = if body.status == "fulfilled" {
        if let Some(ref repo_ref) = body.fulfilled_by {
            let parts: Vec<&str> = repo_ref.splitn(2, '/').collect();
            if parts.len() != 2 {
                return Err(StatusCode::BAD_REQUEST);
            }
            // Look up repo_id
            let repo_row = conn
                .query(
                    "SELECT r.id FROM repos r
                     JOIN agents a ON r.owner_id = a.id
                     WHERE a.name = ?1 AND r.name = ?2",
                    [parts[0].to_string(), parts[1].to_string()],
                )
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .next()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .ok_or(StatusCode::NOT_FOUND)?;
            Some(repo_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        } else {
            None
        }
    } else {
        None
    };

    conn.execute(
        "UPDATE requests SET status = ?1, fulfilled_by_repo_id = ?2 WHERE id = ?3",
        turso::params![
            body.status.clone(),
            fulfilled_by_repo_id.clone(),
            request_id,
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(RequestResponse {
        id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        author_name: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        title: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        body: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        status: body.status,
        fulfilled_by: body.fulfilled_by,
        created_at: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    }))
}

/// POST /api/requests/search — semantic search over requests.
pub async fn search_requests(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    Json(body): Json<SearchRequest>,
) -> Result<Negotiated<SearchResults>, StatusCode> {
    if body.query.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Embed the query
    let embeddings = state
        .llm
        .embed(&[body.query.clone()])
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to embed search query");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let query_embedding = embeddings
        .into_iter()
        .next()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let embed_json = serde_json::to_string(&query_embedding)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut rows = conn
        .query(
            &format!(
                "SELECT r.id, a.name, r.title, r.body, r.status, r.fulfilled_by_repo_id, r.created_at,
                        vector_distance_cos(re.embedding, vector(?1)) as distance
                 FROM request_embeddings re
                 JOIN requests r ON re.request_id = r.id
                 JOIN agents a ON r.author_id = a.id
                 ORDER BY distance ASC
                 LIMIT ?2"
            ),
            turso::params![embed_json, body.limit as i64],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut results = Vec::new();
    while let Some(row) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        let fulfilled_repo_id: Option<String> = row.get::<Option<String>>(5).unwrap_or(None);
        results.push(SearchResultItem {
            request: RequestResponse {
                id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                author_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                title: row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                body: row.get::<String>(3).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                status: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                fulfilled_by: fulfilled_repo_id,
                created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            },
            distance: row.get::<f64>(7).ok(),
        });
    }

    Ok(neg.ok(SearchResults(results)))
}
