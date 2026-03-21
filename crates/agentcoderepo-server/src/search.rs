//! Search API handlers for type-signature and semantic (vector) search.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Shared response type
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub repo_id: String,
    pub owner_name: String,
    pub repo_name: String,
    pub file_path: String,
    /// Implementation language (e.g. "rust", "python"), empty if unknown.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub language: String,
    pub function_name: String,
    pub type_signature: String,
    pub description: String,
    /// Cosine distance (only populated for semantic search; lower = more similar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

impl TextFormat for SearchResponse {
    fn to_text(&self) -> String {
        if self.results.is_empty() {
            return "(no results)\n".to_string();
        }
        let mut s = String::new();
        for (i, r) in self.results.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            // First line: name, location, language, optional distance
            s.push_str(&r.function_name);
            s.push_str("  ");
            s.push_str(&r.owner_name);
            s.push('/');
            s.push_str(&r.repo_name);
            s.push(':');
            s.push_str(&r.file_path);
            if !r.language.is_empty() {
                s.push_str(&format!("  [{}]", r.language));
            }
            if let Some(dist) = r.distance {
                s.push_str(&format!("  dist={dist:.4}"));
            }
            s.push('\n');
            // Indented type + description
            s.push_str("  ");
            s.push_str(&r.type_signature);
            s.push('\n');
            if !r.description.is_empty() {
                s.push_str("  ");
                s.push_str(&r.description);
                s.push('\n');
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Type search: exact match on canonical type text
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TypeSearchRequest {
    /// A AgentCodeRepo type signature string, e.g. "forall a. Ord a => List a -> List a".
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

/// Search for functions whose canonical type signature matches the query.
///
/// The query is parsed and re-displayed to normalize it into canonical form,
/// then matched against the stored `type_signature` column.
#[tracing::instrument(skip(state))]
pub async fn search_by_type(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    Json(body): Json<TypeSearchRequest>,
) -> Result<Negotiated<SearchResponse>, StatusCode> {
    // Parse, alpha-normalize, and re-display to get a canonical form
    // that matches regardless of variable naming.
    let normalized = match agentcoderepo_types::parse::parse_ty(&body.query) {
        Ok(ty) => ty.alpha_normalize().to_string(),
        Err(_) => body.query.clone(),
    };

    let conn = state
        .db
        .connect()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut rows = conn
        .query(
            "SELECT fs.repo_id, a.name, r.name, fs.file_path, fs.language,
                    fs.function_name, fs.type_signature, fs.description
             FROM function_signatures fs
             JOIN repos r ON fs.repo_id = r.id
             JOIN agents a ON r.owner_id = a.id
             WHERE fs.type_normalized = ?1
             LIMIT ?2",
            turso::params![normalized, body.limit as i64],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "type search query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        results.push(SearchResult {
            repo_id: row
                .get::<String>(0)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            owner_name: row
                .get::<String>(1)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            repo_name: row
                .get::<String>(2)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            file_path: row
                .get::<String>(3)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            language: row.get::<String>(4).unwrap_or_default(),
            function_name: row
                .get::<String>(5)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            type_signature: row
                .get::<String>(6)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            description: row
                .get::<String>(7)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            distance: None,
        });
    }

    Ok(neg.ok(SearchResponse { results }))
}

// ---------------------------------------------------------------------------
// Semantic search: vector similarity via embeddings
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SemanticSearchRequest {
    /// Free-text query, e.g. "sort a list of integers".
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

/// Search for functions by semantic similarity.
///
/// Embeds the query text using the LLM, then finds the nearest function
/// embeddings using cosine distance.
#[tracing::instrument(skip(state))]
pub async fn search_semantic(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    Json(body): Json<SemanticSearchRequest>,
) -> Result<Negotiated<SearchResponse>, StatusCode> {
    // Embed the query
    let embeddings = state
        .llm
        .embed(&[body.query.clone()])
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to embed search query");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let query_embedding = embeddings.into_iter().next().ok_or_else(|| {
        tracing::error!("embedding returned empty result");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let emb_json = serde_json::to_string(&query_embedding).map_err(|_| {
        tracing::error!("failed to serialize query embedding");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let conn = state
        .db
        .connect()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut rows = conn
        .query(
            "SELECT fs.repo_id, a.name, r.name, fs.file_path, fs.language,
                    fs.function_name, fs.type_signature, fs.description,
                    vector_distance_cos(fe.embedding, vector(?1)) as distance
             FROM function_embeddings fe
             JOIN function_signatures fs ON fe.signature_id = fs.id
             JOIN repos r ON fs.repo_id = r.id
             JOIN agents a ON r.owner_id = a.id
             ORDER BY distance ASC
             LIMIT ?2",
            turso::params![emb_json, body.limit as i64],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "semantic search query failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut results = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        results.push(SearchResult {
            repo_id: row
                .get::<String>(0)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            owner_name: row
                .get::<String>(1)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            repo_name: row
                .get::<String>(2)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            file_path: row
                .get::<String>(3)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            language: row.get::<String>(4).unwrap_or_default(),
            function_name: row
                .get::<String>(5)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            type_signature: row
                .get::<String>(6)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            description: row
                .get::<String>(7)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            distance: row.get::<f64>(8).ok(),
        });
    }

    Ok(neg.ok(SearchResponse { results }))
}
