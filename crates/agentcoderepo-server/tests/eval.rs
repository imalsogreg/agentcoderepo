use agentcoderepo_test::TestHarness;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start a harness with sprites configured to point at a wiremock server.
async fn setup_with_sprites() -> (TestHarness, MockServer) {
    let mock_sprites = MockServer::start().await;
    let harness = TestHarness::start_with_sprites(&mock_sprites.uri()).await.unwrap();
    (harness, mock_sprites)
}

#[tokio::test]
async fn eval_without_sprites_returns_501() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/eval", agent.name),
            &serde_json::json!({
                "language": "python",
                "code": "print('hello')",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn eval_on_unprovisioned_repo_returns_404() {
    let (harness, _mock) = setup_with_sprites().await;
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/eval", agent.name),
            &serde_json::json!({
                "language": "python",
                "code": "print('hello')",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404); // sprite not provisioned
}

#[tokio::test]
async fn eval_invalid_language_returns_400() {
    let (harness, _mock) = setup_with_sprites().await;
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/eval", agent.name),
            &serde_json::json!({
                "language": "brainfuck",
                "code": "+++",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn eval_with_provisioned_sprite_succeeds() {
    let (harness, mock_sprites) = setup_with_sprites().await;
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    // Insert a provisioned sprite record directly
    harness
        .insert_repo_sprite(&agent.name, "my-repo", "acr-test-sprite", "v1")
        .await;

    // Mock: restore checkpoint
    Mock::given(method("POST"))
        .and(path_regex(r"/v1/sprites/.*/checkpoints/.*/restore"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("{\"type\":\"complete\",\"data\":\"restored\"}\n"),
        )
        .mount(&mock_sprites)
        .await;

    // Mock: write file (called twice — agent code + runner script)
    Mock::given(method("PUT"))
        .and(path_regex(r"/v1/sprites/.*/fs/write"))
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .mount(&mock_sprites)
        .await;

    // Mock: exec — returns NDJSON with stdout data + exit event
    Mock::given(method("POST"))
        .and(path_regex(r"/v1/sprites/.*/exec"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(
                "{\"type\":\"stdout\",\"data\":\"[1, 2, 3]\\n\"}\n{\"type\":\"exit\",\"exit_code\":0}\n",
            ),
        )
        .mount(&mock_sprites)
        .await;

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-repo/eval", agent.name),
            &serde_json::json!({
                "language": "python",
                "code": "from sort import sort\nprint(sort([3,1,2]))",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "completed");
    assert_eq!(body["exit_code"], 0);
    assert!(body["stdout"].as_str().unwrap().contains("[1, 2, 3]"));
}

#[tokio::test]
async fn sprite_status_unprovisioned() {
    let (harness, _mock) = setup_with_sprites().await;
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    let resp = agent
        .get(&format!("/api/repos/{}/my-repo/sprites/status", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["provisioned"], false);
}

#[tokio::test]
async fn sprite_status_provisioned() {
    let (harness, _mock) = setup_with_sprites().await;
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-repo" }))
        .await
        .unwrap();

    harness
        .insert_repo_sprite(&agent.name, "my-repo", "acr-test-sprite", "v1")
        .await;

    let resp = agent
        .get(&format!("/api/repos/{}/my-repo/sprites/status", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["provisioned"], true);
    assert_eq!(body["sprite_name"], "acr-test-sprite");
    assert_eq!(body["status"], "ready");
}
