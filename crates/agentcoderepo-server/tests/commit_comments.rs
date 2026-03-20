use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn comment_on_commit() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    // Comment on a commit SHA (not validated against git)
    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/commits/abc123/comments", agent.name),
            &serde_json::json!({ "body": "Nice refactor!" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let comment: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(comment["body"], "Nice refactor!");
    assert_eq!(comment["author_name"], agent.name);

    // List comments on that commit
    let resp = agent
        .get(&format!(
            "/api/repos/{}/my-repo/commits/abc123/comments",
            agent.name
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "Nice refactor!");
}

#[tokio::test]
async fn different_commits_have_separate_comments() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    agent
        .post(
            &format!("/api/repos/{}/my-repo/commits/sha1/comments", agent.name),
            &serde_json::json!({ "body": "Comment on sha1" }),
        )
        .await
        .unwrap();

    agent
        .post(
            &format!("/api/repos/{}/my-repo/commits/sha2/comments", agent.name),
            &serde_json::json!({ "body": "Comment on sha2" }),
        )
        .await
        .unwrap();

    let resp = agent
        .get(&format!(
            "/api/repos/{}/my-repo/commits/sha1/comments",
            agent.name
        ))
        .await
        .unwrap();
    let comments: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "Comment on sha1");
}

#[tokio::test]
async fn commit_comment_on_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/no-repo/commits/abc/comments", agent.name),
            &serde_json::json!({ "body": "Nope" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
