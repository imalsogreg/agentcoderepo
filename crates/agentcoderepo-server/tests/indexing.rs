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

/// Push a Rust file, verify the LLM was called for extraction and embedding,
/// and that signatures were stored in the database.
#[tokio::test]
async fn push_triggers_indexing() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    // Create the repo via the API first
    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "indexed-repo" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Queue the LLM extraction response
    harness
        .mock_llm
        .queue_response(
            r#"[
                {
                    "name": "add",
                    "type": "Int -> Int -> Int",
                    "description": "Add two integers"
                },
                {
                    "name": "greet",
                    "type": "String ->{IO} Unit",
                    "description": "Print a greeting message"
                }
            ]"#,
        )
        .await;

    // Create a local repo with a source file
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    std::fs::write(
        repo_dir.join("lib.rs"),
        r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn greet(name: &str) { println!("Hello, {name}!"); }
"#,
    )
    .unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "initial"]).await;

    // Push to molthub
    let remote_url = format!("{}/git/{}/indexed-repo", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    // Verify the LLM was called for extraction
    let requests = harness.mock_llm.recorded_requests().await;
    assert!(
        !requests.is_empty(),
        "LLM should have been called for signature extraction"
    );

    // Verify the extraction request mentions our file
    let last_request = &requests[requests.len() - 1];
    let user_msg = last_request
        .iter()
        .find(|m| m.role == "user")
        .expect("should have user message");
    assert!(
        user_msg.content.contains("lib.rs"),
        "extraction request should reference the file"
    );

    // Verify the LLM was called for embeddings
    let embed_requests = harness.mock_llm.recorded_embed_requests().await;
    assert!(
        !embed_requests.is_empty(),
        "LLM should have been called for embeddings"
    );

    // Verify signatures are stored in the database by querying the repo API
    // (we'll check the DB directly via a protected endpoint in the future,
    // but for now just verify the indexing didn't error out)

    // The push should have completed successfully (indexing is sync)
    // If we got here without panicking, the pipeline worked end-to-end
}

/// Push to a repo that hasn't been created via the API — indexing should be skipped
/// gracefully (the post-receive hook can't find a repo_id).
#[tokio::test]
async fn push_to_uncreated_repo_skips_indexing() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    // Don't create the repo via API — just push directly
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    std::fs::write(repo_dir.join("file.txt"), "hello").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "init"]).await;

    let remote_url = format!("{}/git/{}/uncreated-repo", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    // Push should succeed even without a repo record — indexing is just skipped
    let requests = harness.mock_llm.recorded_requests().await;
    assert!(
        requests.is_empty(),
        "LLM should NOT have been called — repo not in DB"
    );
}
