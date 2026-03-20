use agentcoderepo_test::TestHarness;

/// Set up a funded agent with a repo and an issue.
async fn setup_funded_agent(
    harness: &TestHarness,
) -> (
    agentcoderepo_test::TestSponsorSession,
    agentcoderepo_test::TestAgent,
    String, // issue_id
) {
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Fund the agent
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", agent.name),
            &serde_json::json!({ "amount": "500.00" }),
        )
        .await
        .unwrap();

    // Create repo + issue
    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/issues", agent.name),
            &serde_json::json!({ "title": "Fix the bug" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap().to_string();

    (sponsor, agent, issue_id)
}

#[tokio::test]
async fn post_bounty_holds_credits() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, agent, issue_id) = setup_funded_agent(&harness).await;

    // Post a 100-credit bounty
    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({
                "issue_id": issue_id,
                "amount": "100.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let bounty: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(bounty["amount"], "100.00");
    assert_eq!(bounty["status"], "open");
    assert_eq!(bounty["funder_name"], agent.name);

    // Balance should be 400 (500 - 100 held)
    let resp = agent.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "400.00");
}

#[tokio::test]
async fn bounty_insufficient_funds() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, agent, issue_id) = setup_funded_agent(&harness).await;

    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({
                "issue_id": issue_id,
                "amount": "999.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 402);
}

#[tokio::test]
async fn claim_and_approve_bounty() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, funder, issue_id) = setup_funded_agent(&harness).await;
    let claimant = harness.registered_agent().await.unwrap();

    // Funder posts bounty
    let resp = funder
        .post(
            "/api/bounties",
            &serde_json::json!({
                "issue_id": issue_id,
                "amount": "100.00",
            }),
        )
        .await
        .unwrap();
    let bounty: serde_json::Value = resp.json().await.unwrap();
    let bounty_id = bounty["id"].as_str().unwrap();

    // Claimant claims
    let resp = claimant
        .post(
            &format!("/api/bounties/{bounty_id}/claim"),
            &serde_json::json!({ "evidence": "See my PR at owner/repo#1" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let claim: serde_json::Value = resp.json().await.unwrap();
    let claim_id = claim["id"].as_str().unwrap();

    // Funder approves
    let resp = funder
        .post(
            &format!("/api/bounties/{bounty_id}/approve"),
            &serde_json::json!({ "claim_id": claim_id }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "completed");

    // Claimant received the credits
    let resp = claimant.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "100.00");

    // Funder's balance unchanged (already deducted at posting)
    let resp = funder.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "400.00");
}

#[tokio::test]
async fn cancel_bounty_refunds() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, agent, issue_id) = setup_funded_agent(&harness).await;

    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({
                "issue_id": issue_id,
                "amount": "100.00",
            }),
        )
        .await
        .unwrap();
    let bounty: serde_json::Value = resp.json().await.unwrap();
    let bounty_id = bounty["id"].as_str().unwrap();

    // Cancel
    let resp = agent
        .post(&format!("/api/bounties/{bounty_id}/cancel"), &serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Credits refunded
    let resp = agent.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "500.00");
}

#[tokio::test]
async fn only_funder_can_approve() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, funder, issue_id) = setup_funded_agent(&harness).await;
    let claimant = harness.registered_agent().await.unwrap();

    let resp = funder
        .post(
            "/api/bounties",
            &serde_json::json!({ "issue_id": issue_id, "amount": "50.00" }),
        )
        .await
        .unwrap();
    let bounty: serde_json::Value = resp.json().await.unwrap();
    let bounty_id = bounty["id"].as_str().unwrap();

    let resp = claimant
        .post(
            &format!("/api/bounties/{bounty_id}/claim"),
            &serde_json::json!({ "evidence": "done" }),
        )
        .await
        .unwrap();
    let claim: serde_json::Value = resp.json().await.unwrap();
    let claim_id = claim["id"].as_str().unwrap();

    // Claimant tries to approve their own claim → 403
    let resp = claimant
        .post(
            &format!("/api/bounties/{bounty_id}/approve"),
            &serde_json::json!({ "claim_id": claim_id }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn bounty_on_request() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, agent, _issue_id) = setup_funded_agent(&harness).await;

    // Create a request
    let resp = agent
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "Build me something" }),
        )
        .await
        .unwrap();
    let req: serde_json::Value = resp.json().await.unwrap();
    let request_id = req["id"].as_str().unwrap();

    // Post bounty on the request
    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({ "request_id": request_id, "amount": "75.00" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let bounty: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(bounty["request_id"], request_id);
    assert!(bounty["issue_id"].is_null());
}

#[tokio::test]
async fn must_specify_exactly_one_target() {
    let harness = TestHarness::start().await.unwrap();
    let (_sponsor, agent, issue_id) = setup_funded_agent(&harness).await;

    // Neither
    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({ "amount": "10.00" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Both (create a request first)
    let resp = agent
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "something" }),
        )
        .await
        .unwrap();
    let req: serde_json::Value = resp.json().await.unwrap();
    let request_id = req["id"].as_str().unwrap();

    let resp = agent
        .post(
            "/api/bounties",
            &serde_json::json!({
                "issue_id": issue_id,
                "request_id": request_id,
                "amount": "10.00",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
