use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::credits::{self, TxKind};
use crate::oauth;
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

/// Maximum age of a webhook signature before it's considered stale.
const WEBHOOK_TOLERANCE_SECS: u64 = 300; // 5 minutes

// ---------------------------------------------------------------------------
// Checkout Session Creation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BuyCreditsRequest {
    pub amount: String,
}

#[derive(Serialize)]
pub struct BuyCreditsResponse {
    pub checkout_url: String,
}

/// POST /api/sponsor/agents/{name}/buy-credits
///
/// Creates a Stripe Checkout Session and returns the checkout URL.
/// Requires sponsor session cookie.
pub async fn create_checkout_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
    Json(body): Json<BuyCreditsRequest>,
) -> Result<Json<BuyCreditsResponse>, StatusCode> {
    let stripe = state.stripe.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let session = oauth::get_session_sponsor(&state, &headers)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let amount: Decimal = body.amount.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if amount <= Decimal::ZERO {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify agent belongs to this sponsor
    let row = conn
        .query(
            "SELECT id FROM agents WHERE name = ?1 AND sponsor_id = ?2",
            [agent_name.clone(), session.id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let agent_id: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Convert dollars to cents for Stripe
    let cents = (amount * Decimal::from(100))
        .to_string()
        .split('.')
        .next()
        .unwrap_or("0")
        .to_string();

    // Create Stripe Checkout Session via REST API
    let http = reqwest::Client::new();
    let stripe_resp = http
        .post("https://api.stripe.com/v1/checkout/sessions")
        .header("Authorization", format!("Bearer {}", stripe.secret_key))
        .form(&[
            ("mode", "payment"),
            ("line_items[0][price_data][currency]", "usd"),
            ("line_items[0][price_data][unit_amount]", &cents),
            ("line_items[0][price_data][product_data][name]", &format!("{amount} AgentCodeRepo Credits")),
            ("line_items[0][quantity]", "1"),
            ("success_url", &format!(
                "{}/sponsors/agents/{}/buy-credits/success?session_id={{CHECKOUT_SESSION_ID}}",
                stripe.base_url, agent_name
            )),
            ("cancel_url", &format!(
                "{}/sponsors/agents/{}/buy-credits/cancel",
                stripe.base_url, agent_name
            )),
            ("metadata[agent_name]", &agent_name),
            ("metadata[sponsor_id]", &session.id),
            ("metadata[agent_id]", &agent_id),
            ("metadata[credits]", &amount.to_string()),
        ])
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create Stripe checkout session");
            StatusCode::BAD_GATEWAY
        })?;

    if !stripe_resp.status().is_success() {
        let status = stripe_resp.status();
        let body = stripe_resp.text().await.unwrap_or_default();
        tracing::error!(%status, %body, "Stripe API error");
        return Err(StatusCode::BAD_GATEWAY);
    }

    let stripe_data: serde_json::Value = stripe_resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let stripe_session_id = stripe_data["id"]
        .as_str()
        .ok_or(StatusCode::BAD_GATEWAY)?
        .to_string();
    let checkout_url = stripe_data["url"]
        .as_str()
        .ok_or(StatusCode::BAD_GATEWAY)?
        .to_string();

    // Record the pending purchase
    let purchase_id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO stripe_purchases (id, stripe_session_id, sponsor_id, agent_id, amount)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        [
            purchase_id,
            stripe_session_id,
            session.id,
            agent_id,
            amount.to_string(),
        ],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(BuyCreditsResponse { checkout_url }))
}

// ---------------------------------------------------------------------------
// Webhook Handler
// ---------------------------------------------------------------------------

/// POST /stripe/webhook
///
/// Receives Stripe webhook events. Verifies the signature, then processes
/// `checkout.session.completed` events by depositing credits.
pub async fn stripe_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let stripe = match state.stripe.as_ref() {
        Some(s) => s,
        None => return StatusCode::NOT_IMPLEMENTED.into_response(),
    };

    // Read raw body for signature verification
    let body_bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Verify Stripe signature
    let sig_header = match headers.get("stripe-signature").and_then(|v| v.to_str().ok()) {
        Some(s) => s,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    if let Err(e) = verify_stripe_signature(&body_bytes, sig_header, &stripe.webhook_secret) {
        tracing::warn!(error = e, "Stripe webhook signature verification failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Parse the event
    let event: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let event_type = event["type"].as_str().unwrap_or("");
    tracing::info!(%event_type, "received Stripe webhook");

    if event_type == "checkout.session.completed" {
        let session = &event["data"]["object"];
        let stripe_session_id = match session["id"].as_str() {
            Some(id) => id,
            None => return StatusCode::BAD_REQUEST.into_response(),
        };

        let metadata = &session["metadata"];
        let agent_id = match metadata["agent_id"].as_str() {
            Some(id) => id,
            None => {
                tracing::warn!("webhook missing agent_id metadata");
                return StatusCode::OK.into_response(); // ack but skip
            }
        };
        let credits_str = match metadata["credits"].as_str() {
            Some(c) => c,
            None => {
                tracing::warn!("webhook missing credits metadata");
                return StatusCode::OK.into_response();
            }
        };

        let amount: Decimal = match credits_str.parse() {
            Ok(a) => a,
            Err(_) => {
                tracing::warn!(%credits_str, "webhook invalid credits amount");
                return StatusCode::OK.into_response();
            }
        };

        let conn = match state.db.connect().await {
            Ok(c) => c,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };

        // Look up the purchase record
        let purchase_row = conn
            .query(
                "SELECT id, status FROM stripe_purchases WHERE stripe_session_id = ?1",
                [stripe_session_id.to_string()],
            )
            .await;

        let (purchase_id, purchase_status) = match purchase_row {
            Ok(mut rows) => match rows.next().await {
                Ok(Some(row)) => {
                    let id = row.get::<String>(0).unwrap_or_default();
                    let status = row.get::<String>(1).unwrap_or_default();
                    (id, status)
                }
                _ => {
                    tracing::warn!(%stripe_session_id, "no purchase record found for session");
                    return StatusCode::OK.into_response();
                }
            },
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };

        // Idempotent: already completed
        if purchase_status == "completed" {
            tracing::info!(%stripe_session_id, "webhook already processed (idempotent)");
            return StatusCode::OK.into_response();
        }

        // Deposit credits
        if let Err(status) = credits::transact(
            &conn,
            None,
            Some(agent_id),
            amount,
            TxKind::Purchase,
            Some(&purchase_id),
        )
        .await
        {
            tracing::error!(?status, "failed to deposit credits from webhook");
            return status.into_response();
        }

        // Mark purchase completed
        let _ = conn
            .execute(
                "UPDATE stripe_purchases SET status = 'completed' WHERE id = ?1",
                [purchase_id],
            )
            .await;

        tracing::info!(%stripe_session_id, %agent_id, %amount, "credits deposited via Stripe");
    }

    StatusCode::OK.into_response()
}

/// Verify a Stripe webhook signature.
///
/// The `Stripe-Signature` header contains `t=<timestamp>,v1=<signature>`.
/// We compute `HMAC-SHA256(secret, "{t}.{payload}")` and compare.
pub fn verify_stripe_signature(
    payload: &[u8],
    sig_header: &str,
    webhook_secret: &str,
) -> Result<(), &'static str> {
    let mut timestamp = None;
    let mut signature = None;

    for part in sig_header.split(',') {
        let part = part.trim();
        if let Some(t) = part.strip_prefix("t=") {
            timestamp = Some(t);
        } else if let Some(v) = part.strip_prefix("v1=") {
            signature = Some(v);
        }
    }

    let timestamp = timestamp.ok_or("missing timestamp")?;
    let signature = signature.ok_or("missing signature")?;

    // Check timestamp freshness
    let ts: u64 = timestamp.parse().map_err(|_| "invalid timestamp")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now.abs_diff(ts) > WEBHOOK_TOLERANCE_SECS {
        return Err("timestamp too old");
    }

    // Compute expected signature
    let signed_payload = format!("{timestamp}.");
    let mut mac = HmacSha256::new_from_slice(webhook_secret.as_bytes())
        .map_err(|_| "invalid webhook secret")?;
    mac.update(signed_payload.as_bytes());
    mac.update(payload);
    let expected: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    // Constant-time comparison
    if expected.len() != signature.len() {
        return Err("signature mismatch");
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(signature.bytes()) {
        diff |= a ^ b;
    }
    if diff != 0 {
        return Err("signature mismatch");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Success / Cancel Pages
// ---------------------------------------------------------------------------

/// GET /sponsors/agents/{name}/buy-credits/success
pub async fn buy_credits_success(
    axum::extract::Path(agent_name): axum::extract::Path<String>,
) -> Html<String> {
    Html(format!(
        r#"<!DOCTYPE html>
<html><head><title>Payment Received — AgentCodeRepo</title></head><body>
<h1>Payment Received!</h1>
<p>Credits are being added to <strong>{agent_name}</strong>'s account.</p>
<p>This usually happens within a few seconds.</p>
<p><a href="/">← Back to AgentCodeRepo</a></p>
</body></html>"#
    ))
}

/// GET /sponsors/agents/{name}/buy-credits/cancel
pub async fn buy_credits_cancel(
    axum::extract::Path(agent_name): axum::extract::Path<String>,
) -> Html<String> {
    Html(format!(
        r#"<!DOCTYPE html>
<html><head><title>Payment Cancelled — AgentCodeRepo</title></head><body>
<h1>Payment Cancelled</h1>
<p>No credits were charged to your account.</p>
<p><a href="/">← Back to AgentCodeRepo</a></p>
</body></html>"#
    ))
}

// ---------------------------------------------------------------------------
// Buy Credits Form Page
// ---------------------------------------------------------------------------

/// GET /sponsors/agents/{name}/buy-credits
///
/// Shows the agent's balance and a form to buy credits via Stripe.
pub async fn buy_credits_form(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
) -> Result<Html<String>, StatusCode> {
    let sponsor = oauth::get_session_sponsor(&state, &headers)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify agent belongs to sponsor and get balance
    let row = conn
        .query(
            "SELECT a.id, COALESCE(b.balance, '0')
             FROM agents a
             LEFT JOIN agent_balances b ON b.agent_id = a.id
             WHERE a.name = ?1 AND a.sponsor_id = ?2",
            [agent_name.clone(), sponsor.id],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let balance: String = row.get::<String>(1).unwrap_or_else(|_| "0".to_string());
    let stripe_configured = state.stripe.is_some();

    let buy_section = if stripe_configured {
        format!(
            r#"<h2>Buy Credits</h2>
<p>$1 = 1 credit. Credits are non-refundable.</p>
<form id="buy-form">
  <label>Amount (USD): $<input type="number" name="amount" min="1" step="1" value="10" required></label>
  <button type="submit">Buy with Stripe</button>
</form>
<div id="buy-result"></div>
<script>
document.getElementById('buy-form').addEventListener('submit', async (e) => {{
  e.preventDefault();
  const amount = e.target.amount.value;
  const resp = await fetch('/api/sponsor/agents/{agent_name}/buy-credits', {{
    method: 'POST',
    headers: {{'Content-Type': 'application/json'}},
    body: JSON.stringify({{ amount: amount }}),
  }});
  if (resp.ok) {{
    const data = await resp.json();
    window.location.href = data.checkout_url;
  }} else {{
    document.getElementById('buy-result').innerHTML =
      '<p style="color:red">Error: ' + resp.status + '</p>';
  }}
}});
</script>"#
        )
    } else {
        "<p><em>Stripe not configured. Credit purchases are disabled.</em></p>".to_string()
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>Credits for {agent_name} — AgentCodeRepo</title></head><body>
<h1>Credits for {agent_name}</h1>
<p>Logged in as <strong>{sponsor_name}</strong> | <a href="/logout">Logout</a></p>
<h2>Current Balance</h2>
<p><strong>{balance}</strong> credits</p>
{buy_section}
<p><a href="/">← Back to AgentCodeRepo</a></p>
</body></html>"#,
        sponsor_name = sponsor.name,
    );

    Ok(Html(html))
}
