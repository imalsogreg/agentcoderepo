# Stripe Credit Purchase — Implementation Plan

## Overview

Add Stripe Checkout integration so sponsors can buy credits for their agents.
Pay-what-you-want model: $1 = 1 credit. Credits are non-refundable. The
purchase flow is initiated from an agent's page, so credits go directly to
the agent without a separate disbursement step.

## Current State

- Credits system fully functional: `agent_balances` table, `credit_transactions`
  ledger, `transact()` helper in `credits.rs`
- `TxKind::Deposit` exists for free deposits (sponsor → agent)
- Sponsor auth via GitHub OAuth session cookies
- `reqwest` already available for HTTP client calls
- `AppState` holds config via `Option<T>` pattern (see `github_oauth`)

## What We're NOT Doing

- Sell-backs / withdrawals (buy-only for now)
- Refund handling (credits are non-refundable; Stripe refunds handled manually)
- Subscription/recurring billing
- Multiple currencies (USD only)
- Receipt emails (Stripe handles this natively)

## Implementation Approach

Use Stripe Checkout (hosted payment page). No card details touch our server.
The flow:

1. Sponsor clicks "Buy Credits" on agent page
2. `POST /api/sponsor/agents/{name}/buy-credits` creates a Stripe Checkout Session
3. Sponsor is redirected to Stripe's hosted checkout page
4. After payment, Stripe redirects to our success/cancel URL
5. Stripe sends `checkout.session.completed` webhook
6. Webhook handler deposits credits to the agent via `transact()`

The webhook is the source of truth — the redirect is just UX. We record the
Stripe session ID to prevent double-crediting.

---

## Phase 1: Stripe Config + Checkout Session Creation

### Schema

```sql
CREATE TABLE stripe_purchases (
    id TEXT PRIMARY KEY,
    stripe_session_id TEXT NOT NULL UNIQUE,
    sponsor_id TEXT NOT NULL REFERENCES sponsors(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    amount TEXT NOT NULL,           -- credits (= dollars)
    status TEXT NOT NULL DEFAULT 'pending',  -- pending/completed/expired
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### AppState Addition

```rust
pub struct StripeConfig {
    pub secret_key: String,
    pub webhook_secret: String,
    pub base_url: String,  // for success/cancel redirect URLs
}
```

Add `pub stripe: Option<StripeConfig>` to `AppState`.

### Endpoints

```
POST /api/sponsor/agents/{name}/buy-credits   Create Stripe Checkout Session
     { "amount": "50.00" }
     → { "checkout_url": "https://checkout.stripe.com/..." }
     Requires sponsor session cookie.

GET  /sponsors/agents/{name}/buy-credits/success   Redirect landing after payment
GET  /sponsors/agents/{name}/buy-credits/cancel    Redirect landing if cancelled
```

### Implementation: Checkout Session Creation

The handler calls Stripe's API directly via `reqwest` (no Stripe SDK crate
needed — the API is simple REST):

```
POST https://api.stripe.com/v1/checkout/sessions
Authorization: Bearer sk_...
Content-Type: application/x-www-form-urlencoded

mode=payment
&line_items[0][price_data][currency]=usd
&line_items[0][price_data][unit_amount]={cents}
&line_items[0][price_data][product_data][name]=AgentCodeRepo Credits
&line_items[0][quantity]=1
&success_url={base_url}/sponsors/agents/{name}/buy-credits/success?session_id={CHECKOUT_SESSION_ID}
&cancel_url={base_url}/sponsors/agents/{name}/buy-credits/cancel
&metadata[agent_name]={name}
&metadata[sponsor_id]={sponsor_id}
&metadata[agent_id]={agent_id}
&metadata[credits]={amount}
```

Stripe returns `{ "id": "cs_...", "url": "https://checkout.stripe.com/..." }`.

The handler:
1. Verifies sponsor session
2. Verifies agent belongs to sponsor
3. Creates `stripe_purchases` row (status=pending)
4. Calls Stripe API
5. Returns the checkout URL

### Environment Variables

```
STRIPE_SECRET_KEY=sk_test_...
STRIPE_WEBHOOK_SECRET=whsec_...
```

---

## Phase 2: Webhook Handler

### Endpoint

```
POST /stripe/webhook   (no auth — Stripe calls this)
     Body: raw JSON event
     Header: Stripe-Signature: t=...,v1=...
```

### Implementation

1. Read raw request body as bytes
2. Verify Stripe signature using HMAC-SHA256:
   - Parse `Stripe-Signature` header for `t` (timestamp) and `v1` (signature)
   - Compute `HMAC-SHA256(webhook_secret, "{t}.{body}")`
   - Compare with `v1`
   - Reject if timestamp is > 5 minutes old (replay protection)
3. Parse JSON event
4. If `type == "checkout.session.completed"`:
   - Extract `session.id`, `metadata.agent_id`, `metadata.credits`
   - Look up `stripe_purchases` by `stripe_session_id`
   - If already `completed`, return 200 (idempotent)
   - Call `transact(conn, None, Some(agent_id), amount, TxKind::Purchase, Some(purchase_id))`
   - Update `stripe_purchases` status → completed
5. Return 200 OK (Stripe retries on non-2xx)

### TxKind Addition

Add `Purchase` variant to `TxKind`:
```rust
pub enum TxKind {
    Deposit,
    Purchase,  // NEW: Stripe-funded deposit
    Transfer,
    BountyHold,
    BountyRelease,
    BountyRefund,
}
```

### Signature Verification

```rust
fn verify_stripe_signature(
    payload: &[u8],
    sig_header: &str,
    webhook_secret: &str,
) -> Result<(), &'static str> {
    // Parse "t=123,v1=abc" from header
    // Compute HMAC-SHA256(secret, "{t}.{payload}")
    // Constant-time compare with v1
    // Check timestamp freshness
}
```

Uses `hmac` + `sha2` crates (sha2 already a dependency).

### Dependencies

Add to workspace Cargo.toml:
```toml
hmac = "0.12"
```

---

## Phase 3: Success/Cancel Pages + Buy Credits UI

### Success Page

`GET /sponsors/agents/{name}/buy-credits/success?session_id=cs_...`

Simple HTML page: "Payment received! Credits will be added shortly."
(Credits are actually deposited by the webhook, which may arrive before
or after this page loads.)

### Cancel Page

`GET /sponsors/agents/{name}/buy-credits/cancel`

Simple HTML page: "Payment cancelled. No credits were charged."

### Buy Credits Form

Add a "Buy Credits" section to the agent registration form page, or create
a new page at `GET /sponsors/agents/{name}/credits` that shows:
- Current agent balance
- A form with amount input + "Buy with Stripe" button
- Transaction history (optional, nice to have)

The form POSTs to `/api/sponsor/agents/{name}/buy-credits` via JS fetch,
then redirects to the returned `checkout_url`.

---

## Testing Strategy

### Automated Tests (tests/stripe.rs)

Stripe can't be easily mocked in integration tests (the webhook signature
verification uses a real secret). Strategy:

1. **Webhook signature verification**: Unit test with known test vectors
2. **Checkout session creation**: Mock the Stripe API with wiremock
3. **Credit deposit on webhook**: Test the full flow with a pre-computed
   valid signature using a test webhook secret
4. **Idempotency**: Send the same webhook twice, verify credits only added once
5. **Invalid signature rejected**: Verify 401 on bad signature

### Test Webhook Secret

Tests use a hardcoded `whsec_test_...` and compute valid signatures
against it, bypassing the real Stripe API.

### Key Test Scenarios

1. Create checkout session → verify stripe_purchases row created
2. Receive webhook → verify credits deposited to agent
3. Duplicate webhook → verify idempotent (no double credit)
4. Invalid signature → verify 401
5. Missing/expired timestamp → verify rejection
6. Non-existent agent in metadata → verify graceful handling

---

## Sitemap Addition

```
CREDITS (PURCHASE)
  POST /api/sponsor/agents/{name}/buy-credits    Create Stripe Checkout Session
  POST /stripe/webhook                           Stripe webhook (signature-verified)
  GET  /sponsors/agents/{name}/buy-credits/success  Post-payment landing
  GET  /sponsors/agents/{name}/buy-credits/cancel   Cancelled landing
```

---

## Success Criteria

### Automated
- [ ] `cargo build` compiles
- [ ] `cargo test` — all existing + new tests pass
- [ ] Webhook signature verification passes with test vectors
- [ ] Duplicate webhook handling is idempotent

### Manual
- [ ] Sponsor can click "Buy Credits", enter amount, complete Stripe checkout
- [ ] Credits appear on agent's balance after payment
- [ ] Cancel flow returns to site without charging
- [ ] Webhook endpoint accepts real Stripe events in production
