use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn github_login_creates_session() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    // Session should be valid — /api/sponsor/me returns our identity
    let resp = sponsor.get("/api/sponsor/me").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "alice");
}

#[tokio::test]
async fn sponsor_me_requires_session() {
    let harness = TestHarness::start().await.unwrap();

    // No session cookie → 401
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/sponsor/me", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn human_sponsors_agent_via_oauth() {
    let harness = TestHarness::start().await.unwrap();

    // Human logs in via mocked GitHub OAuth
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    // Sponsor registers an agent
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Agent authenticates with Ed25519 and makes requests
    let resp = agent.get("/api/me").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["agent_name"], agent.name);
    assert_eq!(body["sponsor_name"], "alice");
}

#[tokio::test]
async fn agent_can_create_repo_under_oauth_sponsor() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor = harness.login_github("bob", 67890).await.unwrap();

    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Agent creates a repo
    let resp = agent
        .post(
            "/api/repos",
            &serde_json::json!({
                "name": "my-tool",
                "description": "A useful tool",
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "my-tool");
    assert_eq!(body["owner_name"], agent.name);
}

#[tokio::test]
async fn logout_invalidates_session() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor = harness.login_github("carol", 11111).await.unwrap();

    // Session works
    let resp = sponsor.get("/api/sponsor/me").await.unwrap();
    assert_eq!(resp.status(), 200);

    // Logout
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("{}/logout", harness.base_url))
        .header("Cookie", format!("session={}", sponsor.session_token))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_redirection());

    // Session should now be invalid
    let resp = sponsor.get("/api/sponsor/me").await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn second_login_reuses_sponsor() {
    let harness = TestHarness::start().await.unwrap();

    // First login
    let sponsor1 = harness.login_github("dave", 22222).await.unwrap();

    // Second login with same GitHub identity
    let sponsor2 = harness.login_github("dave", 22222).await.unwrap();

    // Same sponsor_id — the OAuth callback found the existing record
    assert_eq!(sponsor1.sponsor_id, sponsor2.sponsor_id);

    // Both sessions are valid (different tokens)
    assert_ne!(sponsor1.session_token, sponsor2.session_token);

    let resp = sponsor2.get("/api/sponsor/me").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn sponsor_can_list_agents_and_repos() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    // No agents yet
    let resp = sponsor.get("/api/sponsor/agents").await.unwrap();
    assert_eq!(resp.status(), 200);
    let agents: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(agents.is_empty());

    // Register an agent
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Now we have one agent
    let resp = sponsor.get("/api/sponsor/agents").await.unwrap();
    assert_eq!(resp.status(), 200);
    let agents: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], agent.name);

    // No repos yet
    let resp = sponsor.get("/api/sponsor/repos").await.unwrap();
    assert_eq!(resp.status(), 200);
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(repos.is_empty());

    // Agent creates a repo
    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    // Sponsor can see it
    let resp = sponsor.get("/api/sponsor/repos").await.unwrap();
    assert_eq!(resp.status(), 200);
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"], "my-lib");
    assert_eq!(repos[0]["owner_name"], agent.name);
}

#[tokio::test]
async fn sponsor_endpoints_require_session() {
    let harness = TestHarness::start().await.unwrap();
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/sponsor/agents", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client
        .get(format!("{}/api/sponsor/repos", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn register_agent_requires_sponsor_session() {
    let harness = TestHarness::start().await.unwrap();

    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let agent = harness.agent();

    use base64::Engine;
    let pk_b64 = base64::engine::general_purpose::STANDARD
        .encode(agent.verifying_key.as_bytes());

    // No session cookie → 401
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/sponsors/{}/agents",
            harness.base_url, sponsor.sponsor_id
        ))
        .json(&serde_json::json!({
            "name": "rogue-agent",
            "public_key_base64": pk_b64,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Wrong sponsor's session → 403
    let sponsor2 = harness.login_github("bob", 99999).await.unwrap();
    let resp = sponsor2
        .post(
            &format!("/sponsors/{}/agents", sponsor.sponsor_id),
            &serde_json::json!({
                "name": "rogue-agent",
                "public_key_base64": pk_b64,
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn registration_form_requires_login() {
    let harness = TestHarness::start().await.unwrap();

    // Not logged in → 401
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/sponsors/agents/new", harness.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Logged in → 200 with form
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let resp = sponsor.get("/sponsors/agents/new").await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Register an Agent"));
    assert!(body.contains(&sponsor.sponsor_id));
}
