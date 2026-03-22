use agentcoderepo_test::TestHarness;

#[tokio::test]
async fn resolve_simple_dependency() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Create a dependency repo with a version
    agent
        .post("/api/repos", &serde_json::json!({ "name": "sort-lib" }))
        .await
        .unwrap();

    // Insert version records directly (normally done by indexing pipeline)
    harness
        .insert_repo_version(&agent.name, "sort-lib", "1.0.0", "aaa111")
        .await;
    harness
        .insert_repo_version(&agent.name, "sort-lib", "1.1.0", "bbb222")
        .await;
    harness
        .insert_repo_version(&agent.name, "sort-lib", "2.0.0", "ccc333")
        .await;

    // Resolve ^1.0
    let resp = agent
        .post(
            "/api/resolve",
            &serde_json::json!({
                "dependencies": {
                    "sort-lib": {
                        "repo": format!("{}/sort-lib", agent.name),
                        "version": "^1.0.0"
                    }
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let resolved = &body["resolved"]["sort-lib"];
    assert_eq!(resolved["version"], "1.1.0"); // newest compatible
    assert_eq!(resolved["commit_sha"], "bbb222");
    assert!(!body["flake_inputs"]["sort-lib"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn resolve_no_matching_version() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "empty-lib" }))
        .await
        .unwrap();

    // No versions published
    let resp = agent
        .post(
            "/api/resolve",
            &serde_json::json!({
                "dependencies": {
                    "empty-lib": {
                        "repo": format!("{}/empty-lib", agent.name),
                        "version": "^1.0.0"
                    }
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn resolve_diamond_dependency() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    // Create repos: A depends on B ^1.0 and C ^1.0, C depends on B ^1.1
    agent
        .post("/api/repos", &serde_json::json!({ "name": "lib-b" }))
        .await
        .unwrap();
    agent
        .post("/api/repos", &serde_json::json!({ "name": "lib-c" }))
        .await
        .unwrap();

    harness.insert_repo_version(&agent.name, "lib-b", "1.0.0", "b100").await;
    harness.insert_repo_version(&agent.name, "lib-b", "1.1.0", "b110").await;
    harness.insert_repo_version(&agent.name, "lib-b", "1.2.0", "b120").await;

    harness.insert_repo_version(&agent.name, "lib-c", "1.0.0", "c100").await;

    // C v1.0.0 depends on B ^1.1
    harness
        .insert_repo_dependency(
            &agent.name,
            "lib-c",
            "c100",
            "lib-b",
            &agent.name,
            "lib-b",
            "^1.1.0",
        )
        .await;

    // Resolve: depends on B ^1.0 and C ^1.0
    let resp = agent
        .post(
            "/api/resolve",
            &serde_json::json!({
                "dependencies": {
                    "lib-b": {
                        "repo": format!("{}/lib-b", agent.name),
                        "version": "^1.0.0"
                    },
                    "lib-c": {
                        "repo": format!("{}/lib-c", agent.name),
                        "version": "^1.0.0"
                    }
                }
            }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    // B should be resolved to 1.2.0 (newest that satisfies both ^1.0 and ^1.1)
    assert_eq!(body["resolved"]["lib-b"]["version"], "1.2.0");
    assert_eq!(body["resolved"]["lib-c"]["version"], "1.0.0");
}

#[tokio::test]
async fn list_versions() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    harness.insert_repo_version(&agent.name, "my-lib", "1.0.0", "aaa").await;
    harness.insert_repo_version(&agent.name, "my-lib", "1.1.0", "bbb").await;

    let resp = agent
        .get(&format!("/api/repos/{}/my-lib/versions", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let versions: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(versions.len(), 2);
}
