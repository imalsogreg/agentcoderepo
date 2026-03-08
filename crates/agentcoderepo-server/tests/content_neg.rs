//! Tests for content negotiation — verifying text/plain responses.

use agentcoderepo_test::TestHarness;
use std::path::Path;
use std::process::Command;

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

async fn setup(harness: &TestHarness) -> agentcoderepo_test::TestAgent {
    harness.registered_agent().await.unwrap()
}

/// Helper: make an authenticated GET with Accept: text/plain.
async fn get_text(agent: &agentcoderepo_test::TestAgent, path: &str) -> String {
    let resp = agent
        .client
        .get(format!("{}{}", agent.base_url, path))
        .header("Authorization", format!("Bearer {}", agent.bearer_token()))
        .header("Accept", "text/plain")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    resp.text().await.unwrap()
}

/// Helper: make an authenticated POST with Accept: text/plain.
async fn post_text(
    agent: &agentcoderepo_test::TestAgent,
    path: &str,
    body: &serde_json::Value,
) -> (u16, String) {
    let resp = agent
        .client
        .post(format!("{}{}", agent.base_url, path))
        .header("Authorization", format!("Bearer {}", agent.bearer_token()))
        .header("Accept", "text/plain")
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (status, text)
}

#[tokio::test]
async fn get_repo_as_text() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // Create a repo (JSON, default)
    agent
        .post(
            "/api/repos",
            &serde_json::json!({
                "name": "text-test",
                "description": "A test repo"
            }),
        )
        .await
        .unwrap();

    // GET with Accept: text/plain
    let text = get_text(&agent, &format!("/api/repos/{}/text-test", agent.name)).await;
    assert!(text.contains(&format!("{}/text-test", agent.name)));
    assert!(text.contains("A test repo"));
}

#[tokio::test]
async fn create_repo_as_text() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let (status, text) = post_text(
        &agent,
        "/api/repos",
        &serde_json::json!({
            "name": "created-text",
            "description": "Created via text"
        }),
    )
    .await;

    assert_eq!(status, 201);
    assert!(text.contains(&format!("{}/created-text", agent.name)));
    assert!(text.contains("Created via text"));
}

#[tokio::test]
async fn list_repos_as_text() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    agent
        .post(
            "/api/repos",
            &serde_json::json!({ "name": "repo-a", "description": "First" }),
        )
        .await
        .unwrap();
    agent
        .post(
            "/api/repos",
            &serde_json::json!({ "name": "repo-b", "description": "Second" }),
        )
        .await
        .unwrap();

    let text = get_text(&agent, "/api/repos").await;
    assert!(text.contains("repo-a"));
    assert!(text.contains("repo-b"));
    assert!(text.contains("First"));
    assert!(text.contains("Second"));
}

#[tokio::test]
async fn me_as_text() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    let text = get_text(&agent, "/api/me").await;
    assert!(text.contains(&format!("name: {}", agent.name)));
    assert!(text.contains("sponsor:"));
}

#[tokio::test]
async fn search_type_as_text() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // Create repo + push indexed code
    agent
        .post(
            "/api/repos",
            &serde_json::json!({ "name": "search-text" }),
        )
        .await
        .unwrap();

    harness
        .mock_llm
        .queue_response(
            r#"[{
                "name": "add",
                "type": "Int -> Int -> Int",
                "description": "Add two numbers"
            }]"#,
        )
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "t@t.com"]).await;
    git(&repo_dir, &["config", "user.name", "T"]).await;
    std::fs::write(repo_dir.join("lib.rs"), "pub fn add(a: i32, b: i32) -> i32 { a + b }").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "init"]).await;

    let remote = format!("{}/git/{}/search-text", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote]).await;
    git_auth(&repo_dir, &agent.bearer_token(), &["push", "origin", "main"]).await;

    // Search with text/plain
    let (status, text) = post_text(
        &agent,
        "/api/search/type",
        &serde_json::json!({ "query": "Int -> Int -> Int" }),
    )
    .await;

    assert_eq!(status, 200);
    assert!(text.contains("add"), "should find the add function: {text}");
    assert!(text.contains("Int -> Int -> Int"));
    assert!(text.contains("Add two numbers"));
}

#[tokio::test]
async fn root_returns_sitemap() {
    let harness = TestHarness::start().await.unwrap();

    let resp = reqwest::get(format!("{}/", harness.base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("text/html"), "root should be text/html, got: {ct}");

    let text = resp.text().await.unwrap();
    assert!(text.contains("AgentCodeRepo"));
    assert!(text.contains("/api/search/type"));
    assert!(text.contains("/api/repos"));
    assert!(text.contains("Accept: text/plain"));
}

#[tokio::test]
async fn default_accept_returns_json() {
    let harness = TestHarness::start().await.unwrap();
    let agent = setup(&harness).await;

    // No Accept header → should get JSON
    let resp = agent.get("/api/me").await.unwrap();
    let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(
        ct.contains("application/json"),
        "default should be JSON, got: {ct}"
    );
}
