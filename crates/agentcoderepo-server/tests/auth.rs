use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn register_agent_via_oauth_sponsor() {
    let harness = TestHarness::start().await.unwrap();

    // Sponsor logs in via GitHub OAuth
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    // Sponsor registers an agent
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Health check still works without auth
    let resp = agent
        .client
        .get(format!("{}/health", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn authenticated_request_succeeds() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Authenticated GET to a protected endpoint
    let resp = agent.get("/api/me").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], agent.name);
    assert_eq!(body["sponsor_name"], agent.sponsor_name);
}

#[tokio::test]
async fn unauthenticated_request_is_rejected() {
    let harness = TestHarness::start().await.unwrap();

    // Hit a protected endpoint with no auth header
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/me", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn invalid_signature_is_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Send a request with a garbage bearer token
    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", "Bearer garbage:123:not-a-signature")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn ssh_signature_auth_succeeds() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Use SSH signature instead of raw Ed25519
    let ssh_token = agent.bearer_token_ssh();
    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", format!("Bearer {ssh_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], agent.name);
}

#[tokio::test]
async fn key_rotation_allows_new_key() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    // Register agent with first key
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Agent works with original key
    let resp = agent.get("/api/me").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Generate a new keypair (simulating key loss/rotation)
    let new_signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let new_verifying_key = new_signing_key.verifying_key();

    // Sponsor adds the new key
    use base64::Engine;
    let new_pk_b64 = base64::engine::general_purpose::STANDARD
        .encode(new_verifying_key.as_bytes());

    let resp = sponsor
        .post(
            &format!("/api/sponsor/agents/{}/keys", agent.name),
            &serde_json::json!({ "public_key_base64": new_pk_b64 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Agent can authenticate with the new key
    let old_id = agent.id;
    agent.signing_key = new_signing_key;
    agent.verifying_key = new_verifying_key;
    // ID stays the same — use make_bearer_token with the old ID
    let token = agentcoderepo_server::make_bearer_token(&old_id, &agent.signing_key);
    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], agent.name);
}

#[tokio::test]
async fn ssh_token_two_part_format() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Use the 2-part SSH token format (no agent ID needed)
    let ssh_token = agent.bearer_token_ssh();
    assert!(!ssh_token.contains(&agent.id.to_string()), "2-part format should not contain agent ID");

    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", format!("Bearer {ssh_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], agent.name);
}

#[tokio::test]
async fn unregistered_agent_is_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.agent();
    // Don't register — just try to authenticate
    let resp = agent.get("/api/me").await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn git_push_requires_auth() {
    let harness = TestHarness::start().await.unwrap();

    // Try to hit the git info/refs endpoint without auth
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/git/some-owner/some-repo/info/refs?service=git-receive-pack",
            harness.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
