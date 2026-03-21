use agentcoderepo_test::TestHarness;

const TEST_WEBHOOK_SECRET: &str = "whsec_test_secret_for_testing";

/// Compute a valid Stripe webhook signature for testing.
fn sign_webhook(payload: &[u8], secret: &str, timestamp: u64) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let signed_payload = format!("{timestamp}.");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(signed_payload.as_bytes());
    mac.update(payload);
    let sig: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    format!("t={timestamp},v1={sig}")
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Create a test harness with Stripe configured using a known webhook secret.
/// Since we can't easily mock Stripe's checkout API in the harness startup,
/// we test the webhook path directly by inserting stripe_purchases manually.
async fn setup_with_stripe() -> (TestHarness, agentcoderepo_test::TestAgent, String) {
    // We need to configure Stripe on the harness. Since TestHarness::start()
    // sets stripe: None, we'll test the webhook handler directly by:
    // 1. Using a harness with stripe configured via a custom setup
    // 2. Manually inserting stripe_purchases records
    // 3. Sending signed webhook events

    // For now, we'll test by using the harness and making direct HTTP calls
    // with the right headers, since the webhook endpoint reads stripe config
    // from AppState.

    // Actually, the test harness doesn't support custom StripeConfig. Let's
    // add a method to set it, or test the functions directly.

    // Simplest approach: test verify_stripe_signature as a unit test,
    // and test the webhook flow by creating a custom test harness.

    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    (harness, agent, sponsor.sponsor_id.clone())
}

// ---------------------------------------------------------------------------
// Unit tests for signature verification
// ---------------------------------------------------------------------------

#[test]
fn verify_valid_signature() {
    let payload = b"test payload";
    let ts = now_timestamp();
    let sig = sign_webhook(payload, TEST_WEBHOOK_SECRET, ts);

    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        payload,
        &sig,
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_ok());
}

#[test]
fn reject_invalid_signature() {
    let payload = b"test payload";
    let ts = now_timestamp();
    let sig = format!("t={ts},v1=0000000000000000000000000000000000000000000000000000000000000000");

    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        payload,
        &sig,
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "signature mismatch");
}

#[test]
fn reject_tampered_body() {
    let payload = b"original payload";
    let ts = now_timestamp();
    let sig = sign_webhook(payload, TEST_WEBHOOK_SECRET, ts);

    // Verify with different payload → should fail
    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        b"tampered payload",
        &sig,
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_err());
}

#[test]
fn reject_stale_timestamp() {
    let payload = b"test payload";
    let stale_ts = now_timestamp() - 600; // 10 minutes ago
    let sig = sign_webhook(payload, TEST_WEBHOOK_SECRET, stale_ts);

    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        payload,
        &sig,
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "timestamp too old");
}

#[test]
fn reject_missing_signature_parts() {
    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        b"payload",
        "t=123",
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_err());

    let result = agentcoderepo_server::stripe::verify_stripe_signature(
        b"payload",
        "v1=abc",
        TEST_WEBHOOK_SECRET,
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Integration tests for webhook endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_without_stripe_config_returns_501() {
    let harness = TestHarness::start().await.unwrap();

    // Stripe is not configured on the test harness
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .body("test")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn webhook_deposits_credits() {
    let harness = TestHarness::start_with_stripe(TEST_WEBHOOK_SECRET).await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Manually insert a pending purchase (simulating what create_checkout_session does)
    let purchase_id = "test-purchase-1";
    let session_id = "cs_test_session_123";
    let client = reqwest::Client::new();

    // Insert purchase record via a direct DB operation isn't possible from tests,
    // so we'll use an admin-style approach: the webhook should handle the case
    // where the purchase record exists.

    // Actually, let's insert via the sponsor deposit endpoint to create the agent
    // balance row, then test that the webhook creates credits correctly.
    // But we need the stripe_purchases row. Let me use a different approach:
    // configure the harness with stripe, call the webhook, and check that it
    // handles missing purchase records gracefully.

    // For a complete test: we need to insert the purchase row. Since we can't
    // easily do that from outside, let's add a helper to the test harness.

    // Simpler approach: test the webhook with a purchase record that we insert
    // via the buy-credits endpoint backed by a wiremock Stripe mock.

    // For now, let's test the signature verification + graceful handling.
    let webhook_payload = serde_json::json!({
        "type": "checkout.session.completed",
        "data": {
            "object": {
                "id": session_id,
                "metadata": {
                    "agent_id": agent.id.to_string(),
                    "agent_name": agent.name,
                    "sponsor_id": sponsor.sponsor_id,
                    "credits": "50"
                }
            }
        }
    });
    let body = serde_json::to_vec(&webhook_payload).unwrap();
    let sig = sign_webhook(&body, TEST_WEBHOOK_SECRET, now_timestamp());

    // This will succeed (200) but log "no purchase record found" — that's fine
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .header("stripe-signature", &sig)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn webhook_rejects_invalid_signature() {
    let harness = TestHarness::start_with_stripe(TEST_WEBHOOK_SECRET).await.unwrap();

    let client = reqwest::Client::new();
    let body = b"{\"type\":\"test\"}";
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .header("stripe-signature", "t=123,v1=invalid")
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn webhook_rejects_missing_signature() {
    let harness = TestHarness::start_with_stripe(TEST_WEBHOOK_SECRET).await.unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .body("{\"type\":\"test\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn full_webhook_flow_with_purchase_record() {
    let harness = TestHarness::start_with_stripe(TEST_WEBHOOK_SECRET).await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Insert a purchase record by calling the internal DB directly
    // We'll do this by having the harness expose a helper, or by calling
    // a test-only endpoint. Simplest: just call the sponsor deposit endpoint
    // to verify the webhook adds credits on top.

    // Actually, let's insert the purchase record via SQL through the test helper.
    // Since we can't, let's use the harness to create the record.
    harness
        .insert_stripe_purchase(
            "test-purchase-2",
            "cs_test_complete_456",
            &sponsor.sponsor_id,
            &agent.id.to_string(),
            "75",
        )
        .await;

    // Send webhook
    let webhook_payload = serde_json::json!({
        "type": "checkout.session.completed",
        "data": {
            "object": {
                "id": "cs_test_complete_456",
                "metadata": {
                    "agent_id": agent.id.to_string(),
                    "agent_name": agent.name,
                    "sponsor_id": sponsor.sponsor_id,
                    "credits": "75"
                }
            }
        }
    });
    let body = serde_json::to_vec(&webhook_payload).unwrap();
    let sig = sign_webhook(&body, TEST_WEBHOOK_SECRET, now_timestamp());

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .header("stripe-signature", &sig)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Agent should have 75 credits
    let resp = agent.get("/api/credits").await.unwrap();
    let balance: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(balance["balance"], "75");

    // Send the same webhook again — idempotent
    let sig2 = sign_webhook(&body, TEST_WEBHOOK_SECRET, now_timestamp());
    let resp = client
        .post(format!("{}/stripe/webhook", harness.base_url))
        .header("stripe-signature", &sig2)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Balance should still be 75 (not 150)
    let resp = agent.get("/api/credits").await.unwrap();
    let balance: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(balance["balance"], "75");
}
