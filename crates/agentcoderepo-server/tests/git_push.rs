use std::path::Path;
use std::process::Command;

use agentcoderepo_test::TestHarness;

/// Run a git command in a blocking context (avoids deadlocking the tokio runtime).
async fn git(dir: &Path, args: &[&str]) -> String {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || git_sync(&dir, &args))
        .await
        .unwrap()
}

fn git_sync(dir: &Path, args: &[String]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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
}

/// Run a git command with an Authorization header for authenticated operations.
async fn git_auth(dir: &Path, token: &str, args: &[&str]) -> String {
    let dir = dir.to_path_buf();
    let header = format!("Authorization: Bearer {token}");
    let mut full_args = vec!["-c".to_string(), format!("http.extraHeader={header}")];
    full_args.extend(args.iter().map(|s| s.to_string()));
    tokio::task::spawn_blocking(move || git_sync(&dir, &full_args))
        .await
        .unwrap()
}

async fn init_repo(dir: &Path) {
    git(dir, &["init"]).await;
    git(dir, &["config", "user.email", "test@test.com"]).await;
    git(dir, &["config", "user.name", "Test"]).await;
}

async fn commit_file(dir: &Path, name: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", "."]).await;
    git(dir, &["commit", "-m", msg]).await;
}

/// Register a test agent, create a repo, return (token, remote_url).
async fn setup(harness: &TestHarness, repo_name: &str) -> (String, String) {
    let agent = harness.registered_agent().await.unwrap();
    agent
        .post("/api/repos", &serde_json::json!({ "name": repo_name }))
        .await
        .unwrap();
    let token = agent.bearer_token();
    let remote_url = format!("{}/git/{}/{repo_name}", harness.base_url, agent.name);
    (token, remote_url)
}

#[tokio::test]
async fn push_and_clone_roundtrip() {
    let harness = TestHarness::start().await.unwrap();
    let (token, remote_url) = setup(&harness, "test-repo").await;
    let tmp = tempfile::tempdir().unwrap();

    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir).await;
    commit_file(&repo_dir, "hello.txt", "hello world", "initial commit").await;

    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    let clone_dir = tmp.path().join("cloned");
    let clone_str = clone_dir.to_str().unwrap().to_string();
    git_auth(tmp.path(), &token, &["clone", &remote_url, &clone_str]).await;

    let content = std::fs::read_to_string(clone_dir.join("hello.txt")).unwrap();
    assert_eq!(content, "hello world");
}

#[tokio::test]
async fn push_multiple_commits_and_clone() {
    let harness = TestHarness::start().await.unwrap();
    let (token, remote_url) = setup(&harness, "multi-repo").await;
    let tmp = tempfile::tempdir().unwrap();

    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir).await;
    commit_file(&repo_dir, "a.txt", "aaa", "first").await;
    commit_file(&repo_dir, "b.txt", "bbb", "second").await;

    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    let clone_dir = tmp.path().join("cloned");
    let clone_str = clone_dir.to_str().unwrap().to_string();
    git_auth(tmp.path(), &token, &["clone", &remote_url, &clone_str]).await;

    assert_eq!(std::fs::read_to_string(clone_dir.join("a.txt")).unwrap(), "aaa");
    assert_eq!(std::fs::read_to_string(clone_dir.join("b.txt")).unwrap(), "bbb");

    let log = git(&clone_dir, &["log", "--oneline"]).await;
    assert!(log.contains("first"));
    assert!(log.contains("second"));
}

#[tokio::test]
async fn incremental_push() {
    let harness = TestHarness::start().await.unwrap();
    let (token, remote_url) = setup(&harness, "incr-repo").await;
    let tmp = tempfile::tempdir().unwrap();

    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir).await;

    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;

    commit_file(&repo_dir, "v1.txt", "version 1", "v1").await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    commit_file(&repo_dir, "v2.txt", "version 2", "v2").await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    let clone_dir = tmp.path().join("cloned");
    let clone_str = clone_dir.to_str().unwrap().to_string();
    git_auth(tmp.path(), &token, &["clone", &remote_url, &clone_str]).await;

    assert_eq!(std::fs::read_to_string(clone_dir.join("v1.txt")).unwrap(), "version 1");
    assert_eq!(std::fs::read_to_string(clone_dir.join("v2.txt")).unwrap(), "version 2");
}
