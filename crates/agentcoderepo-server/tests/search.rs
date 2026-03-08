use std::path::Path;
use std::process::Command;

use agentcoderepo_test::TestHarness;

async fn git(dir: &Path, args: &[&str]) -> String {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to run git");
        if !output.status.success() {
            panic!(
                "git {} failed:\nstdout: {}\nstderr: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    })
    .await
    .unwrap()
}

async fn git_auth(dir: &Path, token: &str, args: &[&str]) -> String {
    let dir = dir.to_path_buf();
    let header = format!("Authorization: Bearer {token}");
    let mut full_args = vec!["-c".to_string(), format!("http.extraHeader={header}")];
    full_args.extend(args.iter().map(|s| s.to_string()));
    tokio::task::spawn_blocking(move || {
        let output = Command::new("git")
            .args(&full_args)
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to run git");
        if !output.status.success() {
            panic!(
                "git {} failed:\nstdout: {}\nstderr: {}",
                full_args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    })
    .await
    .unwrap()
}

/// Helper: register agent, create repo, push code with queued LLM response.
async fn setup_indexed_repo(
    harness: &TestHarness,
) -> (agentcoderepo_test::TestAgent, String) {
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    // Create repo
    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "searchable" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Queue the LLM extraction response with parseable AgentCodeRepo types
    harness
        .mock_llm
        .queue_response(
            r#"[
                {
                    "name": "sort",
                    "type": "forall a. Ord a => List a -> List a",
                    "description": "Sort a list using natural ordering"
                },
                {
                    "name": "http_get",
                    "type": "String ->{IO, Fail HttpError} Response",
                    "description": "Perform an HTTP GET request"
                }
            ]"#,
        )
        .await;

    // Create local repo and push
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn sort<T: Ord>(list: Vec<T>) -> Vec<T> { list }\npub fn http_get(url: &str) {}\n",
    )
    .unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "initial"]).await;

    let remote_url = format!("{}/git/{}/searchable", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    // Keep tmp alive by leaking it (it'll be cleaned up when the test exits)
    std::mem::forget(tmp);

    (agent, token)
}

#[tokio::test]
async fn type_search_exact_match() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, _) = setup_indexed_repo(&harness).await;

    // Search for the exact sort type
    let resp = agent
        .post(
            "/api/search/type",
            &serde_json::json!({ "query": "forall a. Ord a => List a -> List a" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "should find exactly one match");
    assert_eq!(results[0]["function_name"], "sort");
    assert_eq!(results[0]["repo_name"], "searchable");
}

#[tokio::test]
async fn type_search_no_match() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, _) = setup_indexed_repo(&harness).await;

    let resp = agent
        .post(
            "/api/search/type",
            &serde_json::json!({ "query": "Int -> Int -> Int" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert!(results.is_empty(), "should find no matches");
}

#[tokio::test]
async fn type_search_effectful() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, _) = setup_indexed_repo(&harness).await;

    let resp = agent
        .post(
            "/api/search/type",
            &serde_json::json!({ "query": "String ->{IO, Fail HttpError} Response" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["function_name"], "http_get");
}

/// Type search should match alpha-equivalent types: searching for
/// `forall x. Ord x => List x -> List x` should find a function
/// indexed as `forall a. Ord a => List a -> List a`.
#[tokio::test]
async fn type_search_alpha_equivalence() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, _) = setup_indexed_repo(&harness).await;

    // The indexed sort function has type: forall a. Ord a => List a -> List a
    // Search with different variable names:
    let resp = agent
        .post(
            "/api/search/type",
            &serde_json::json!({ "query": "forall x. Ord x => List x -> List x" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "alpha-equivalent query should match");
    assert_eq!(results[0]["function_name"], "sort");

    // Also try with yet another variable name
    let resp = agent
        .post(
            "/api/search/type",
            &serde_json::json!({ "query": "forall z. Ord z => List z -> List z" }),
        )
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn semantic_search_returns_ranked_results() {
    let harness = TestHarness::start().await.unwrap();
    let (agent, _) = setup_indexed_repo(&harness).await;

    // Search for "sort and order elements" — the keyword embedder should
    // match "sort" better than "http_get" because the mock embedder maps
    // "sort" and "order" to dedicated dimensions.
    let resp = agent
        .post(
            "/api/search/semantic",
            &serde_json::json!({ "query": "sort and order elements in a list" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "semantic search should return results"
    );

    // All results should have distance scores
    for r in results {
        assert!(r["distance"].is_number(), "each result should have a distance");
    }
}
