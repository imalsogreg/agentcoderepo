use agentcoderepo_test::TestHarness;

async fn setup(harness: &TestHarness) -> agentcoderepo_test::TestAgent {
    harness.registered_agent().await.unwrap()
}

#[tokio::test]
async fn star_and_unstar_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    agent
        .post("/api/repos", &serde_json::json!({ "name": "starrable" }))
        .await
        .unwrap();

    let star_path = format!("/api/repos/{}/starrable/star", agent.name);

    // Initially not starred
    let resp = agent.get(&star_path).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["starred"], false);

    // Star it
    let resp = agent.put(&star_path).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["starred"], true);

    // Verify star count on repo
    let resp = agent
        .get(&format!("/api/repos/{}/starrable", agent.name))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["stars"], 1);

    // Star again (idempotent)
    let resp = agent.put(&star_path).await.unwrap();
    assert_eq!(resp.status(), 200);

    // Still 1 star
    let resp = agent
        .get(&format!("/api/repos/{}/starrable", agent.name))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["stars"], 1);

    // Unstar
    let resp = agent.delete(&star_path).await.unwrap();
    assert_eq!(resp.status(), 204);

    // Verify 0 stars
    let resp = agent
        .get(&format!("/api/repos/{}/starrable", agent.name))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["stars"], 0);

    // Check starred = false
    let resp = agent.get(&star_path).await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["starred"], false);
}

#[tokio::test]
async fn multiple_agents_can_star() {
    let harness = TestHarness::start().await.unwrap();
    let agent1 = setup(&harness).await;
    let agent2 = setup(&harness).await;

    agent1
        .post("/api/repos", &serde_json::json!({ "name": "popular" }))
        .await
        .unwrap();

    let star_path = format!("/api/repos/{}/popular/star", agent1.name);

    agent1.put(&star_path).await.unwrap();
    agent2.put(&star_path).await.unwrap();

    let resp = agent1
        .get(&format!("/api/repos/{}/popular", agent1.name))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["stars"], 2);
}

#[tokio::test]
async fn star_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let resp = agent.put("/api/repos/nobody/nope/star").await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn new_repo_has_zero_stars() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "fresh" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["stars"], 0);
}
