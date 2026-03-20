use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::auth::AuthAgent;
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::oauth;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Core helper: all balance mutations go through here
// ---------------------------------------------------------------------------

/// Transaction kind — determines ledger semantics.
#[derive(Debug, Clone, Copy)]
pub enum TxKind {
    Deposit,
    Transfer,
    BountyHold,
    BountyRelease,
    BountyRefund,
}

impl TxKind {
    fn as_str(&self) -> &'static str {
        match self {
            TxKind::Deposit => "deposit",
            TxKind::Transfer => "transfer",
            TxKind::BountyHold => "bounty_hold",
            TxKind::BountyRelease => "bounty_release",
            TxKind::BountyRefund => "bounty_refund",
        }
    }
}

/// Execute a credit transaction atomically.
///
/// - Deducts from `from_agent_id`'s balance (if Some).
/// - Adds to `to_agent_id`'s balance (if Some).
/// - Records the transaction in the immutable ledger.
/// - Returns 402 PAYMENT_REQUIRED if the sender has insufficient balance.
pub async fn transact(
    conn: &turso::Connection,
    from_agent_id: Option<&str>,
    to_agent_id: Option<&str>,
    amount: Decimal,
    kind: TxKind,
    reference_id: Option<&str>,
) -> Result<(), StatusCode> {
    if amount <= Decimal::ZERO {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Deduct from sender
    if let Some(from_id) = from_agent_id {
        ensure_balance_row(conn, from_id).await?;
        let balance = read_balance(conn, from_id).await?;
        if balance < amount {
            tracing::warn!(agent_id = from_id, %balance, %amount, "insufficient balance");
            return Err(StatusCode::PAYMENT_REQUIRED);
        }
        write_balance(conn, from_id, balance - amount).await?;
    }

    // Add to receiver
    if let Some(to_id) = to_agent_id {
        ensure_balance_row(conn, to_id).await?;
        let balance = read_balance(conn, to_id).await?;
        write_balance(conn, to_id, balance + amount).await?;
    }

    // Record in ledger
    let tx_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO credit_transactions (id, from_agent_id, to_agent_id, amount, kind, reference_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        turso::params![
            tx_id,
            from_agent_id.map(|s| s.to_string()),
            to_agent_id.map(|s| s.to_string()),
            amount.to_string(),
            kind.as_str().to_string(),
            reference_id.map(|s| s.to_string()),
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(())
}

async fn ensure_balance_row(conn: &turso::Connection, agent_id: &str) -> Result<(), StatusCode> {
    conn.execute(
        "INSERT OR IGNORE INTO agent_balances (agent_id, balance) VALUES (?1, '0')",
        [agent_id.to_string()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

async fn read_balance(conn: &turso::Connection, agent_id: &str) -> Result<Decimal, StatusCode> {
    let row = conn
        .query(
            "SELECT balance FROM agent_balances WHERE agent_id = ?1",
            [agent_id.to_string()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let s: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.parse().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn write_balance(conn: &turso::Connection, agent_id: &str, balance: Decimal) -> Result<(), StatusCode> {
    conn.execute(
        "UPDATE agent_balances SET balance = ?1 WHERE agent_id = ?2",
        [balance.to_string(), agent_id.to_string()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct BalanceResponse {
    pub balance: String,
}

impl TextFormat for BalanceResponse {
    fn to_text(&self) -> String {
        format!("balance: {}\n", self.balance)
    }
}

/// GET /api/credits — agent's own balance.
pub async fn get_balance(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
) -> Result<Negotiated<BalanceResponse>, StatusCode> {
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    ensure_balance_row(&conn, &agent.agent_id.to_string()).await?;
    let balance = read_balance(&conn, &agent.agent_id.to_string()).await?;

    Ok(neg.ok(BalanceResponse {
        balance: balance.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct TransferRequest {
    pub to_agent: String,
    pub amount: String,
}

#[derive(Serialize)]
pub struct TransferResponse {
    pub from_balance: String,
    pub transferred: String,
}

impl TextFormat for TransferResponse {
    fn to_text(&self) -> String {
        format!("transferred: {}\nremaining: {}\n", self.transferred, self.from_balance)
    }
}

/// POST /api/credits/transfer — agent transfers credits to another agent.
pub async fn transfer_credits(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    Json(body): Json<TransferRequest>,
) -> Result<Negotiated<TransferResponse>, StatusCode> {
    let amount: Decimal = body.amount.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Look up target agent by name
    let target_row = conn
        .query(
            "SELECT id FROM agents WHERE name = ?1",
            [body.to_agent.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let target_id: String = target_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let from_id = agent.agent_id.to_string();

    transact(&conn, Some(&from_id), Some(&target_id), amount, TxKind::Transfer, None).await?;

    let new_balance = read_balance(&conn, &from_id).await?;

    Ok(neg.ok(TransferResponse {
        from_balance: new_balance.to_string(),
        transferred: amount.to_string(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct DepositRequest {
    pub amount: String,
}

#[derive(Serialize)]
pub struct DepositResponse {
    pub agent_name: String,
    pub new_balance: String,
    pub deposited: String,
}

impl TextFormat for DepositResponse {
    fn to_text(&self) -> String {
        format!("deposited {} to {}\nnew balance: {}\n", self.deposited, self.agent_name, self.new_balance)
    }
}

/// POST /api/sponsor/agents/{name}/credits — sponsor deposits credits to agent.
pub async fn deposit_credits(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    neg: ContentNeg,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
    Json(body): Json<DepositRequest>,
) -> Result<Negotiated<DepositResponse>, StatusCode> {
    let session = oauth::get_session_sponsor(&state, &headers)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let amount: Decimal = body.amount.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify the agent belongs to this sponsor
    let row = conn
        .query(
            "SELECT id FROM agents WHERE name = ?1 AND sponsor_id = ?2",
            [agent_name.clone(), session.id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let agent_id: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    transact(&conn, None, Some(&agent_id), amount, TxKind::Deposit, None).await?;

    let new_balance = read_balance(&conn, &agent_id).await?;

    Ok(neg.ok(DepositResponse {
        agent_name,
        new_balance: new_balance.to_string(),
        deposited: amount.to_string(),
    }))
}
