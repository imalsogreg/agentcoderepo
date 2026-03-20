use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn create_and_get_request() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .post(
            "/api/requests",
            &serde_json::json!({
                "title": "Need a neural network framework",
                "body": "Looking for a lightweight NN framework in Rust",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let req: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(req["title"], "Need a neural network framework");
    assert_eq!(req["status"], "open");
    assert_eq!(req["author_name"], agent.name);

    // Get it back
    let req_id = req["id"].as_str().unwrap();
    let resp = agent.get(&format!("/api/requests/{req_id}")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn list_requests_filters_by_status() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "Open request" }),
        )
        .await
        .unwrap();

    let resp = agent
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "Will close" }),
        )
        .await
        .unwrap();
    let req: serde_json::Value = resp.json().await.unwrap();
    let req_id = req["id"].as_str().unwrap();

    agent
        .patch(
            &format!("/api/requests/{req_id}"),
            &serde_json::json!({ "status": "closed" }),
        )
        .await
        .unwrap();

    // Default: open only
    let resp = agent.get("/api/requests").await.unwrap();
    let requests: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["title"], "Open request");

    // All
    let resp = agent.get("/api/requests?status=all").await.unwrap();
    let requests: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn fulfill_request_links_repo() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Create repo
    agent
        .post(
            "/api/repos",
            &serde_json::json!({ "name": "nn-framework" }),
        )
        .await
        .unwrap();

    // Create request
    let resp = agent
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "Need NN framework" }),
        )
        .await
        .unwrap();
    let req: serde_json::Value = resp.json().await.unwrap();
    let req_id = req["id"].as_str().unwrap();

    // Fulfill it
    let resp = agent
        .patch(
            &format!("/api/requests/{req_id}"),
            &serde_json::json!({
                "status": "fulfilled",
                "fulfilled_by": format!("{}/nn-framework", agent.name),
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "fulfilled");
}

#[tokio::test]
async fn semantic_search_requests() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Create requests — the mock LLM uses keyword-based embeddings
    agent
        .post(
            "/api/requests",
            &serde_json::json!({
                "title": "Sort and order algorithm library",
                "body": "Need sorting and ordering utilities",
            }),
        )
        .await
        .unwrap();

    agent
        .post(
            "/api/requests",
            &serde_json::json!({
                "title": "HTTP send receive framework",
                "body": "Looking for network send and receive tools",
            }),
        )
        .await
        .unwrap();

    // Search for sorting — should match the first request
    let resp = agent
        .post(
            "/api/requests/search",
            &serde_json::json!({
                "query": "sort order",
                "limit": 10,
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let results: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(!results.is_empty());
    // The sorting request should be ranked first (closer distance)
    assert!(results[0]["title"].as_str().unwrap().contains("Sort"));
}

#[tokio::test]
async fn only_author_can_update_request() {
    let harness = TestHarness::start().await.unwrap();
    let agent_a = harness.registered_agent().await.unwrap();
    let agent_b = harness.registered_agent().await.unwrap();

    let resp = agent_a
        .post(
            "/api/requests",
            &serde_json::json!({ "title": "My request" }),
        )
        .await
        .unwrap();
    let req: serde_json::Value = resp.json().await.unwrap();
    let req_id = req["id"].as_str().unwrap();

    // B can't close it
    let resp = agent_b
        .patch(
            &format!("/api/requests/{req_id}"),
            &serde_json::json!({ "status": "closed" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}
