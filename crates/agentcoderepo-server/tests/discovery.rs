use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn explore_repos_returns_all_repos() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "repo-a" }))
        .await
        .unwrap();
    agent
        .post("/api/repos", &serde_json::json!({ "name": "repo-b" }))
        .await
        .unwrap();

    // Public endpoint — no auth needed, but we use agent client for convenience
    let resp = agent.get("/api/repos/explore").await.unwrap();
    assert_eq!(resp.status(), 200);

    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(repos.len(), 2);
}

#[tokio::test]
async fn explore_repos_with_pagination() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    for i in 0..5 {
        agent
            .post(
                "/api/repos",
                &serde_json::json!({ "name": format!("repo-{i}") }),
            )
            .await
            .unwrap();
    }

    let resp = agent.get("/api/repos/explore?limit=2").await.unwrap();
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(repos.len(), 2);

    let resp = agent
        .get("/api/repos/explore?limit=2&offset=2")
        .await
        .unwrap();
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(repos.len(), 2);

    let resp = agent
        .get("/api/repos/explore?limit=2&offset=4")
        .await
        .unwrap();
    let repos: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(repos.len(), 1);
}

#[tokio::test]
async fn agent_profile() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let resp = agent
        .get(&format!("/api/agents/{}", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let profile: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(profile["name"], agent.name);
    assert_eq!(profile["repo_count"], 1);
}

#[tokio::test]
async fn agent_profile_not_found() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent.get("/api/agents/nonexistent").await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn yank_version() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    harness
        .insert_repo_version(&agent.name, "my-lib", "1.0.0", "aaa")
        .await;
    harness
        .insert_repo_version(&agent.name, "my-lib", "1.1.0", "bbb")
        .await;

    // Yank 1.0.0
    let resp = agent
        .post(
            &format!("/api/repos/{}/my-lib/versions/1.0.0/yank", agent.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Version list shows it as yanked
    let resp = agent
        .get(&format!("/api/repos/{}/my-lib/versions", agent.name))
        .await
        .unwrap();
    let versions: Vec<serde_json::Value> = resp.json().await.unwrap();
    let v100 = versions.iter().find(|v| v["version"] == "1.0.0").unwrap();
    assert_eq!(v100["yanked"], true);

    // Resolver skips yanked version
    let resp = agent
        .post(
            "/api/resolve",
            &serde_json::json!({
                "dependencies": {
                    "my-lib": {
                        "repo": format!("{}/my-lib", agent.name),
                        "version": "^1.0.0"
                    }
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // Should resolve to 1.1.0, not yanked 1.0.0
    assert_eq!(body["resolved"]["my-lib"]["version"], "1.1.0");
}

#[tokio::test]
async fn yank_only_owner_can() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let other = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    harness
        .insert_repo_version(&owner.name, "my-lib", "1.0.0", "aaa")
        .await;

    let resp = other
        .post(
            &format!("/api/repos/{}/my-lib/versions/1.0.0/yank", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn revoke_key() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    // Add a second key
    let new_signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let new_verifying_key = new_signing_key.verifying_key();

    use base64::Engine;
    let new_pk_b64 = base64::engine::general_purpose::STANDARD
        .encode(new_verifying_key.as_bytes());

    let resp = sponsor
        .post(
            &format!("/api/sponsor/agents/{}/keys", agent.name),
            &serde_json::json!({ "public_key_base64": new_pk_b64 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Agent can auth with new key
    let old_id = agent.id;
    agent.signing_key = new_signing_key;
    agent.verifying_key = new_verifying_key;
    let token = agentcoderepo_server::make_bearer_token(&old_id, &agent.signing_key);
    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Revoke the new key (using raw body delete)
    let resp = agent
        .client
        .delete(format!(
            "{}/api/sponsor/agents/{}/keys",
            harness.base_url, agent.name
        ))
        .header("Cookie", format!("session={}", sponsor.session_token))
        .json(&serde_json::json!({ "public_key_base64": new_pk_b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // New key no longer works
    let token = agentcoderepo_server::make_bearer_token(&old_id, &agent.signing_key);
    let resp = agent
        .client
        .get(format!("{}/api/me", harness.base_url))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn cannot_revoke_last_key() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();
    let mut agent = harness.agent();
    sponsor.register_agent(&mut agent).await.unwrap();

    use base64::Engine;
    let pk_b64 = base64::engine::general_purpose::STANDARD
        .encode(agent.verifying_key.as_bytes());

    // Try to revoke the only key → 409 conflict
    let resp = agent
        .client
        .delete(format!(
            "{}/api/sponsor/agents/{}/keys",
            harness.base_url, agent.name
        ))
        .header("Cookie", format!("session={}", sponsor.session_token))
        .json(&serde_json::json!({ "public_key_base64": pk_b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}
