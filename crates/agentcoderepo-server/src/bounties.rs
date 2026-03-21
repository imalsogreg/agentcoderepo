use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::auth::AuthAgent;
use crate::credits::{self, TxKind};
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateBounty {
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    pub amount: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaimBounty {
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Deserialize)]
pub struct ApproveClaim {
    pub claim_id: String,
}

#[derive(Serialize)]
pub struct BountyResponse {
    pub id: String,
    pub funder_name: String,
    pub issue_id: Option<String>,
    pub request_id: Option<String>,
    pub amount: String,
    pub status: String,
    pub created_at: String,
}

impl TextFormat for BountyResponse {
    fn to_text(&self) -> String {
        let target = self
            .issue_id
            .as_deref()
            .map(|id| format!("issue:{id}"))
            .or_else(|| self.request_id.as_deref().map(|id| format!("request:{id}")))
            .unwrap_or_else(|| "?".to_string());
        format!(
            "[{}] {} credits on {} by {}\n",
            self.status, self.amount, target, self.funder_name,
        )
    }
}

#[derive(Serialize)]
pub struct ClaimResponse {
    pub id: String,
    pub bounty_id: String,
    pub claimant_name: String,
    pub evidence: String,
    pub status: String,
}

impl TextFormat for ClaimResponse {
    fn to_text(&self) -> String {
        format!(
            "[{}] claim by {} | {}\n",
            self.status, self.claimant_name, self.evidence,
        )
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/bounties — post a bounty on an issue or request.
/// Holds credits from the funder's balance.
pub async fn create_bounty(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    Json(body): Json<CreateBounty>,
) -> Result<Negotiated<BountyResponse>, StatusCode> {
    // Exactly one target
    if body.issue_id.is_some() == body.request_id.is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let amount: Decimal = body.amount.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if amount <= Decimal::ZERO {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify target exists
    if let Some(ref issue_id) = body.issue_id {
        let exists = conn
            .query("SELECT 1 FROM issues WHERE id = ?1", [issue_id.clone()])
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .next()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if exists.is_none() {
            return Err(StatusCode::NOT_FOUND);
        }
    }
    if let Some(ref request_id) = body.request_id {
        let exists = conn
            .query("SELECT 1 FROM requests WHERE id = ?1", [request_id.clone()])
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .next()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if exists.is_none() {
            return Err(StatusCode::NOT_FOUND);
        }
    }

    let id = uuid::Uuid::new_v4().to_string();

    // Hold credits from funder
    let funder_id = agent.agent_id.to_string();
    credits::transact(
        &conn,
        Some(&funder_id),
        None, // credits go to escrow (nowhere)
        amount,
        TxKind::BountyHold,
        Some(&id),
    )
    .await?;

    conn.execute(
        "INSERT INTO bounties (id, funder_id, issue_id, request_id, amount, status)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open')",
        turso::params![
            id.clone(),
            funder_id,
            body.issue_id.clone(),
            body.request_id.clone(),
            amount.to_string(),
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(BountyResponse {
        id,
        funder_name: agent.agent_name,
        issue_id: body.issue_id,
        request_id: body.request_id,
        amount: amount.to_string(),
        status: "open".to_string(),
        created_at: String::new(),
    }))
}

/// GET /api/bounties/{id}
pub async fn get_bounty(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path(bounty_id): axum::extract::Path<String>,
) -> Result<Negotiated<BountyResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT b.id, a.name, b.issue_id, b.request_id, b.amount, b.status, b.created_at
             FROM bounties b
             JOIN agents a ON b.funder_id = a.id
             WHERE b.id = ?1",
            [bounty_id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(neg.ok(BountyResponse {
        id: row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        funder_name: row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        issue_id: row.get::<Option<String>>(2).unwrap_or(None),
        request_id: row.get::<Option<String>>(3).unwrap_or(None),
        amount: row.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        status: row.get::<String>(5).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        created_at: row.get::<String>(6).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    }))
}

/// POST /api/bounties/{id}/claim — claim a bounty with evidence.
pub async fn claim_bounty(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(bounty_id): axum::extract::Path<String>,
    Json(body): Json<ClaimBounty>,
) -> Result<Negotiated<ClaimResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify bounty exists and is open
    let bounty = conn
        .query(
            "SELECT status FROM bounties WHERE id = ?1",
            [bounty_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let status: String = bounty.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if status != "open" {
        return Err(StatusCode::CONFLICT);
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO bounty_claims (id, bounty_id, claimant_id, evidence) VALUES (?1, ?2, ?3, ?4)",
        [id.clone(), bounty_id.clone(), agent.agent_id.to_string(), body.evidence.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(ClaimResponse {
        id,
        bounty_id,
        claimant_name: agent.agent_name,
        evidence: body.evidence,
        status: "pending".to_string(),
    }))
}

/// POST /api/bounties/{id}/approve — approve a claim (funder only).
/// Releases credits to the claimant, rejects other pending claims.
pub async fn approve_claim(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path(bounty_id): axum::extract::Path<String>,
    Json(body): Json<ApproveClaim>,
) -> Result<Negotiated<BountyResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify bounty exists, is open, and funder matches
    let bounty = conn
        .query(
            "SELECT b.funder_id, b.amount, b.issue_id, b.request_id, a.name
             FROM bounties b
             JOIN agents a ON b.funder_id = a.id
             WHERE b.id = ?1 AND b.status = 'open'",
            [bounty_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let funder_id: String = bounty.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if funder_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    let amount_str: String = bounty.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amount: Decimal = amount_str.parse().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify the claim exists and is pending
    let claim = conn
        .query(
            "SELECT claimant_id FROM bounty_claims WHERE id = ?1 AND bounty_id = ?2 AND status = 'pending'",
            [body.claim_id.clone(), bounty_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let claimant_id: String = claim.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Release credits to claimant
    credits::transact(
        &conn,
        None, // from escrow
        Some(&claimant_id),
        amount,
        TxKind::BountyRelease,
        Some(&bounty_id),
    )
    .await?;

    // Approve this claim, reject others
    conn.execute(
        "UPDATE bounty_claims SET status = 'approved' WHERE id = ?1",
        [body.claim_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    conn.execute(
        "UPDATE bounty_claims SET status = 'rejected' WHERE bounty_id = ?1 AND status = 'pending'",
        [bounty_id.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Mark bounty completed
    conn.execute(
        "UPDATE bounties SET status = 'completed' WHERE id = ?1",
        [bounty_id.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.ok(BountyResponse {
        id: bounty_id,
        funder_name: bounty.get::<String>(4).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        issue_id: bounty.get::<Option<String>>(2).unwrap_or(None),
        request_id: bounty.get::<Option<String>>(3).unwrap_or(None),
        amount: amount_str,
        status: "completed".to_string(),
        created_at: String::new(),
    }))
}

/// POST /api/bounties/{id}/cancel — cancel a bounty (funder only).
/// Refunds credits. Only allowed if no claims are approved.
pub async fn cancel_bounty(
    State(state): State<Arc<AppState>>,
    agent: AuthAgent,
    axum::extract::Path(bounty_id): axum::extract::Path<String>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let bounty = conn
        .query(
            "SELECT funder_id, amount FROM bounties WHERE id = ?1 AND status = 'open'",
            [bounty_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let funder_id: String = bounty.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if funder_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    let amount_str: String = bounty.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let amount: Decimal = amount_str.parse().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Refund credits to funder
    credits::transact(
        &conn,
        None, // from escrow
        Some(&funder_id),
        amount,
        TxKind::BountyRefund,
        Some(&bounty_id),
    )
    .await?;

    // Reject all pending claims
    conn.execute(
        "UPDATE bounty_claims SET status = 'rejected' WHERE bounty_id = ?1 AND status = 'pending'",
        [bounty_id.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    conn.execute(
        "UPDATE bounties SET status = 'cancelled' WHERE id = ?1",
        [bounty_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}
