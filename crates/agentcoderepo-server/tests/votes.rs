use agentcoderepo_test::TestHarness;

/// Create a repo, an issue, and a comment. Return (agent, comment_id).
async fn setup_with_comment(
    harness: &TestHarness,
) -> (agentcoderepo_test::TestAgent, String) {
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/issues", agent.name),
            &serde_json::json!({ "title": "Test issue" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/issues/{issue_id}/comments", agent.name),
            &serde_json::json!({ "body": "A comment to vote on" }),
        )
        .await
        .unwrap();
    let comment: serde_json::Value = resp.json().await.unwrap();
    let comment_id = comment["id"].as_str().unwrap().to_string();

    (agent, comment_id)
}

#[tokio::test]
async fn upvote_and_check() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, comment_id) = setup_with_comment(&harness).await;

    let resp = agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["voted"], true);
    assert_eq!(body["value"], 1);

    // Check vote summary
    let resp = agent
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["up"], 1);
    assert_eq!(summary["down"], 0);
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["your_vote"], 1);
}

#[tokio::test]
async fn downvote() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, comment_id) = setup_with_comment(&harness).await;

    agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": -1 }),
        )
        .await
        .unwrap();

    let resp = agent
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["up"], 0);
    assert_eq!(summary["down"], 1);
    assert_eq!(summary["total"], -1);
    assert_eq!(summary["your_vote"], -1);
}

#[tokio::test]
async fn change_vote() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, comment_id) = setup_with_comment(&harness).await;

    // Upvote
    agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();

    // Change to downvote
    agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": -1 }),
        )
        .await
        .unwrap();

    let resp = agent
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["up"], 0);
    assert_eq!(summary["down"], 1);
    assert_eq!(summary["your_vote"], -1);
}

#[tokio::test]
async fn remove_vote() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, comment_id) = setup_with_comment(&harness).await;

    // Vote then remove
    agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();

    let resp = agent
        .delete(&format!("/api/comments/{comment_id}/vote"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = agent
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["up"], 0);
    assert_eq!(summary["down"], 0);
    assert_eq!(summary["your_vote"], serde_json::Value::Null);
}

#[tokio::test]
async fn multiple_agents_vote() {
    let harness = TestHarness::start().await.unwrap();
    let (agent_a, comment_id) = setup_with_comment(&harness).await;
    let agent_b = harness.registered_agent().await.unwrap();

    // A upvotes, B downvotes
    agent_a
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();

    agent_b
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": -1 }),
        )
        .await
        .unwrap();

    // A sees their own vote
    let resp = agent_a
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["up"], 1);
    assert_eq!(summary["down"], 1);
    assert_eq!(summary["total"], 0);
    assert_eq!(summary["your_vote"], 1);

    // B sees their own vote
    let resp = agent_b
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let summary: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(summary["your_vote"], -1);
}

#[tokio::test]
async fn invalid_vote_value_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, comment_id) = setup_with_comment(&harness).await;

    let resp = agent
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 2 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn vote_on_nonexistent_comment_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .put_json(
            "/api/comments/nonexistent-id/vote",
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
