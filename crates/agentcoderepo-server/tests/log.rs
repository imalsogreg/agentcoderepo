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
                "git {} failed:\nstderr: {}",
                args.join(" "),
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
                "git {} failed:\nstderr: {}",
                full_args.join(" "),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn log_returns_commit_history() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    // Create repo via API
    agent
        .post("/api/repos", &serde_json::json!({ "name": "log-test" }))
        .await
        .unwrap();

    // Push some commits
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "TestAgent"]).await;

    std::fs::write(repo_dir.join("file.txt"), "hello").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "first commit"]).await;

    std::fs::write(repo_dir.join("file.txt"), "hello world").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "second commit"]).await;

    let remote = format!("{}/git/{}/log-test", harness.base_url, agent.name);
    git_auth(&repo_dir, &token, &["push", &remote, "main"]).await;

    // Query the log API
    let resp = agent
        .get(&format!("/api/repos/{}/log-test/log", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let entries: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(entries.len(), 2);

    // Most recent first
    assert_eq!(entries[0]["message"], "second commit");
    assert_eq!(entries[0]["author"], "TestAgent");
    assert!(!entries[0]["commit_id"].as_str().unwrap().is_empty());
    assert_eq!(entries[0]["parents"].as_array().unwrap().len(), 1);

    assert_eq!(entries[1]["message"], "first commit");
    assert!(entries[1]["parents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn log_respects_limit() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "limit-test" }))
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    // Create 5 commits
    for i in 1..=5 {
        std::fs::write(repo_dir.join("file.txt"), format!("v{i}")).unwrap();
        git(&repo_dir, &["add", "."]).await;
        git(&repo_dir, &["commit", "-m", &format!("commit {i}")]).await;
    }

    let remote = format!("{}/git/{}/limit-test", harness.base_url, agent.name);
    git_auth(&repo_dir, &token, &["push", &remote, "main"]).await;

    // Default returns all 5
    let resp = agent
        .get(&format!("/api/repos/{}/limit-test/log", agent.name))
        .await
        .unwrap();
    let entries: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(entries.len(), 5);

    // Limit to 2
    let resp = agent
        .get(&format!("/api/repos/{}/limit-test/log?limit=2", agent.name))
        .await
        .unwrap();
    let entries: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["message"], "commit 5"); // most recent
}

#[tokio::test]
async fn log_on_empty_repo_returns_empty() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "empty-repo" }))
        .await
        .unwrap();

    let resp = agent
        .get(&format!("/api/repos/{}/empty-repo/log", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let entries: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(entries.is_empty());
}

#[tokio::test]
async fn log_on_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .get(&format!("/api/repos/{}/no-repo/log", agent.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
