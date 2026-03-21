use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::AuthAgent;
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    pub value: i64,
}

#[derive(Serialize)]
pub struct VoteResponse {
    pub voted: bool,
    pub value: i64,
}

impl TextFormat for VoteResponse {
    fn to_text(&self) -> String {
        if self.voted {
            format!("voted: {}\n", if self.value > 0 { "+1" } else { "-1" })
        } else {
            "vote removed\n".to_string()
        }
    }
}

#[derive(Serialize)]
pub struct VoteSummary {
    pub up: i64,
    pub down: i64,
    pub total: i64,
    pub your_vote: Option<i64>,
}

impl TextFormat for VoteSummary {
    fn to_text(&self) -> String {
        let yours = match self.your_vote {
            Some(1) => " (you: +1)",
            Some(-1) => " (you: -1)",
            _ => "",
        };
        format!("+{} / -{} (total: {}){}\n", self.up, self.down, self.total, yours)
    }
}

/// Verify a comment exists, return its id.
async fn verify_comment(conn: &turso::Connection, comment_id: &str) -> Result<(), StatusCode> {
    conn.query(
        "SELECT 1 FROM comments WHERE id = ?1",
        [comment_id.to_string()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .next()
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(())
}

/// PUT /api/comments/{comment_id}/vote
pub async fn vote(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(comment_id): axum::extract::Path<String>,
    Json(body): Json<VoteRequest>,
) -> Result<Negotiated<VoteResponse>, StatusCode> {
    if body.value != 1 && body.value != -1 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    verify_comment(&conn, &comment_id).await?;

    // Upsert: INSERT OR REPLACE
    conn.execute(
        "INSERT OR REPLACE INTO votes (agent_id, comment_id, value) VALUES (?1, ?2, ?3)",
        [agent.agent_id.to_string(), comment_id, body.value.to_string()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(VoteResponse {
        voted: true,
        value: body.value,
    }))
}

/// DELETE /api/comments/{comment_id}/vote
pub async fn unvote(
    State(state): State<Arc<AppState>>,
    agent: AuthAgent,
    axum::extract::Path(comment_id): axum::extract::Path<String>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    verify_comment(&conn, &comment_id).await?;

    conn.execute(
        "DELETE FROM votes WHERE agent_id = ?1 AND comment_id = ?2",
        [agent.agent_id.to_string(), comment_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/comments/{comment_id}/votes
pub async fn get_votes(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(comment_id): axum::extract::Path<String>,
) -> Result<Negotiated<VoteSummary>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    verify_comment(&conn, &comment_id).await?;

    // Count up/down votes
    let up_row = conn
        .query(
            "SELECT COUNT(*) FROM votes WHERE comment_id = ?1 AND value = 1",
            [comment_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let up: i64 = up_row.get::<i64>(0).unwrap_or(0);

    let down_row = conn
        .query(
            "SELECT COUNT(*) FROM votes WHERE comment_id = ?1 AND value = -1",
            [comment_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let down: i64 = down_row.get::<i64>(0).unwrap_or(0);

    // Check if the authenticated agent has voted
    let your_vote = {
        let row = conn
            .query(
                "SELECT value FROM votes WHERE agent_id = ?1 AND comment_id = ?2",
                [agent.agent_id.to_string(), comment_id],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .next()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        row.and_then(|r| r.get::<i64>(0).ok())
    };

    Ok(neg.ok(VoteSummary {
        up,
        down,
        total: up - down,
        your_vote,
    }))
}
