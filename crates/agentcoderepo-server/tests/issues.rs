use agentcoderepo_test::TestHarness;

async fn setup_with_repo(harness: &TestHarness) -> agentcoderepo_test::TestAgent {
    let agent = harness.registered_agent().await.unwrap();
    agent
        .post("/api/repos", &serde_json::json!({ "name": "test-repo" }))
        .await
        .unwrap();
    agent
}

#[tokio::test]
async fn create_and_get_issue() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup_with_repo(&harness).await;

    let resp = agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({
                "title": "Something is broken",
                "body": "Please fix it",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["title"], "Something is broken");
    assert_eq!(issue["status"], "open");
    assert_eq!(issue["author_name"], agent.name);

    // Get it back
    let issue_id = issue["id"].as_str().unwrap();
    let resp = agent
        .get(&format!(
            "/api/repos/{}/test-repo/issues/{issue_id}",
            agent.name
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Something is broken");
    assert_eq!(body["comment_count"], 0);
}

#[tokio::test]
async fn list_issues_filters_by_status() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup_with_repo(&harness).await;

    // Create two issues
    let resp = agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({ "title": "Open issue" }),
        )
        .await
        .unwrap();
    let _open_issue: serde_json::Value = resp.json().await.unwrap();

    agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({ "title": "Will close" }),
        )
        .await
        .unwrap();

    // Close the second issue
    let resp = agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({ "title": "Will close" }),
        )
        .await
        .unwrap();
    let closed_issue: serde_json::Value = resp.json().await.unwrap();
    let closed_id = closed_issue["id"].as_str().unwrap();

    agent
        .patch(
            &format!("/api/repos/{}/test-repo/issues/{closed_id}", agent.name),
            &serde_json::json!({ "status": "closed" }),
        )
        .await
        .unwrap();

    // Default (open) — should have 2 (the first "Will close" is still open, plus "Open issue")
    let resp = agent
        .get(&format!("/api/repos/{}/test-repo/issues", agent.name))
        .await
        .unwrap();
    let issues: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(issues.len(), 2); // "Open issue" + first "Will close"

    // status=closed
    let resp = agent
        .get(&format!(
            "/api/repos/{}/test-repo/issues?status=closed",
            agent.name
        ))
        .await
        .unwrap();
    let issues: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["id"], closed_id);

    // status=all
    let resp = agent
        .get(&format!(
            "/api/repos/{}/test-repo/issues?status=all",
            agent.name
        ))
        .await
        .unwrap();
    let issues: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(issues.len(), 3);
}

#[tokio::test]
async fn only_author_can_close_issue() {
    let harness = TestHarness::start().await.unwrap();
    let agent_a = setup_with_repo(&harness).await;
    let agent_b = harness.registered_agent().await.unwrap();

    // A creates issue
    let resp = agent_a
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent_a.name),
            &serde_json::json!({ "title": "My issue" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // B tries to close → 403
    let resp = agent_b
        .patch(
            &format!("/api/repos/{}/test-repo/issues/{issue_id}", agent_a.name),
            &serde_json::json!({ "status": "closed" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // A can close it
    let resp = agent_a
        .patch(
            &format!("/api/repos/{}/test-repo/issues/{issue_id}", agent_a.name),
            &serde_json::json!({ "status": "closed" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "closed");
}

#[tokio::test]
async fn issue_on_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/no-such-repo/issues", agent.name),
            &serde_json::json!({ "title": "This should fail" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn comment_on_issue() {
    let harness = TestHarness::start().await.unwrap();
    let agent_a = setup_with_repo(&harness).await;
    let agent_b = harness.registered_agent().await.unwrap();

    // Create issue
    let resp = agent_a
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent_a.name),
            &serde_json::json!({ "title": "Discussion" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // Both agents can comment
    let resp = agent_a
        .post(
            &format!(
                "/api/repos/{}/test-repo/issues/{issue_id}/comments",
                agent_a.name
            ),
            &serde_json::json!({ "body": "First comment" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let resp = agent_b
        .post(
            &format!(
                "/api/repos/{}/test-repo/issues/{issue_id}/comments",
                agent_a.name
            ),
            &serde_json::json!({ "body": "Second comment" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // List comments
    let resp = agent_a
        .get(&format!(
            "/api/repos/{}/test-repo/issues/{issue_id}/comments",
            agent_a.name
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["body"], "First comment");
    assert_eq!(comments[1]["body"], "Second comment");

    // Issue now shows comment count
    let resp = agent_a
        .get(&format!(
            "/api/repos/{}/test-repo/issues/{issue_id}",
            agent_a.name
        ))
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["comment_count"], 2);
}

#[tokio::test]
async fn empty_title_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup_with_repo(&harness).await;

    let resp = agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({ "title": "", "body": "no title" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn empty_comment_body_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup_with_repo(&harness).await;

    let resp = agent
        .post(
            &format!("/api/repos/{}/test-repo/issues", agent.name),
            &serde_json::json!({ "title": "Test" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    let resp = agent
        .post(
            &format!(
                "/api/repos/{}/test-repo/issues/{issue_id}/comments",
                agent.name
            ),
            &serde_json::json!({ "body": "" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
