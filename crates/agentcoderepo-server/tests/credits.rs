use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn agent_starts_with_zero_balance() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent.get("/api/credits").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "0");
}

#[tokio::test]
async fn sponsor_can_deposit_credits() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Deposit 100 credits
    let resp = sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "100.00" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["deposited"], "100.00");
    assert_eq!(body["new_balance"], "100.00");

    // Agent sees the balance
    let resp = agent.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "100.00");
}

#[tokio::test]
async fn deposit_requires_sponsor_ownership() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor1 = harness.login_github("alice", 11111).await.unwrap();
    let sponsor2 = harness.login_github("bob", 22222).await.unwrap();

    let mut agent = harness.agent();
    sponsor1.register_agent(&mut agent).await.unwrap();

    // sponsor2 tries to deposit to sponsor1's agent → 404
    let resp = sponsor2
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "50.00" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn agent_can_transfer_credits() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent_a = harness.agent();
    sponsor.register_agent(&mut agent_a).await.unwrap();
    let mut agent_b = harness.agent();
    sponsor.register_agent(&mut agent_b).await.unwrap();

    // Fund agent_a
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent_a.name),
            &serde_json::json!({ "amount": "100.00" }),
        )
        .await
        .unwrap();

    // Transfer 30 from A to B
    let resp = agent_a
        .post(
            "/api/credits/transfer",
            &serde_json::json!({
                "to_agent": agent_b.name,
                "amount": "30.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["transferred"], "30.00");
    assert_eq!(body["from_balance"], "70.00");

    // Verify B's balance
    let resp = agent_b.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "30.00");
}

#[tokio::test]
async fn transfer_fails_with_insufficient_balance() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent_a = harness.agent();
    sponsor.register_agent(&mut agent_a).await.unwrap();
    let mut agent_b = harness.agent();
    sponsor.register_agent(&mut agent_b).await.unwrap();

    // Fund agent_a with only 10
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent_a.name),
            &serde_json::json!({ "amount": "10.00" }),
        )
        .await
        .unwrap();

    // Try to transfer 50 → 402
    let resp = agent_a
        .post(
            "/api/credits/transfer",
            &serde_json::json!({
                "to_agent": agent_b.name,
                "amount": "50.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 402);

    // A's balance unchanged
    let resp = agent_a.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "10.00");
}

#[tokio::test]
async fn multiple_deposits_accumulate() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Deposit twice
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "25.50" }),
        )
        .await
        .unwrap();
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "14.50" }),
        )
        .await
        .unwrap();

    let resp = agent.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "40.00");
}

#[tokio::test]
async fn transfer_to_nonexistent_agent_fails() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "100.00" }),
        )
        .await
        .unwrap();

    let resp = agent
        .post(
            "/api/credits/transfer",
            &serde_json::json!({
                "to_agent": "nonexistent-agent",
                "amount": "10.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn zero_and_negative_amounts_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Zero deposit
    let resp = sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "0" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Negative deposit
    let resp = sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "-10.00" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
