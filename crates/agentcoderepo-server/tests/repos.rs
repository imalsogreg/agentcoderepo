use agentcoderepo_test::TestHarness;

async fn setup(harness: &TestHarness) -> agentcoderepo_test::TestAgent {
    harness.registered_agent().await.unwrap()
}

#[tokio::test]
async fn create_and_get_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // Create a repo
    let resp = agent
        .post(
            "/api/repos",
            &serde_json::json!({
                "name": "my-repo",
                "description": "A test repository"
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "my-repo");
    assert_eq!(body["description"], "A test repository");
    assert_eq!(body["owner_name"], agent.name);

    // Get the repo by owner/name (public, no auth needed — but our get endpoint
    // is on the same router with state, so we use the agent client for convenience)
    let resp = agent
        .get(&format!("/api/repos/{}/my-repo", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "my-repo");
    assert_eq!(body["description"], "A test repository");
}

#[tokio::test]
async fn list_repos() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // Create two repos
    agent
        .post("/api/repos", &serde_json::json!({ "name": "repo-a" }))
        .await
        .unwrap();
    agent
        .post("/api/repos", &serde_json::json!({ "name": "repo-b" }))
        .await
        .unwrap();

    let resp = agent.get("/api/repos").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 2);

    let names: Vec<&str> = body.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"repo-a"));
    assert!(names.contains(&"repo-b"));
}

#[tokio::test]
async fn duplicate_repo_name_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "dup" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "dup" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

#[tokio::test]
async fn update_repo_description() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    agent
        .post(
            "/api/repos",
            &serde_json::json!({ "name": "updatable", "description": "old" }),
        )
        .await
        .unwrap();

    let resp = agent
        .patch(
            &format!("/api/repos/{}/updatable", agent.name),
            &serde_json::json!({ "description": "new description" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["description"], "new description");

    // Verify it persisted
    let resp = agent
        .get(&format!("/api/repos/{}/updatable", agent.name))
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["description"], "new description");
}

#[tokio::test]
async fn delete_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    agent
        .post("/api/repos", &serde_json::json!({ "name": "doomed" }))
        .await
        .unwrap();

    let resp = agent
        .delete(&format!("/api/repos/{}/doomed", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify it's gone
    let resp = agent
        .get(&format!("/api/repos/{}/doomed", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn cannot_update_other_agents_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent1 = setup(&harness).await;
    let agent2 = setup(&harness).await;

    // Agent 1 creates a repo
    agent1
        .post("/api/repos", &serde_json::json!({ "name": "private" }))
        .await
        .unwrap();

    // Agent 2 tries to update it
    let resp = agent2
        .patch(
            &format!("/api/repos/{}/private", agent1.name),
            &serde_json::json!({ "description": "hacked" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn cannot_delete_other_agents_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent1 = setup(&harness).await;
    let agent2 = setup(&harness).await;

    agent1
        .post("/api/repos", &serde_json::json!({ "name": "safe" }))
        .await
        .unwrap();

    let resp = agent2
        .delete(&format!("/api/repos/{}/safe", agent1.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // Verify it still exists
    let resp = agent1
        .get(&format!("/api/repos/{}/safe", agent1.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn get_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let resp = agent
        .get(&format!("/api/repos/{}/nope", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn invalid_repo_name_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // Dot-prefixed name
    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": ".hidden" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Name with slash
    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "a/b" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Empty name
    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
