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

/// Try to push, returning true if the push succeeded, false if it failed.
async fn try_git_push(dir: &Path, token: &str) -> bool {
    let dir = dir.to_path_buf();
    let header = format!("Authorization: Bearer {token}");
    let full_args = vec![
        "-c".to_string(),
        format!("http.extraHeader={header}"),
        "push".to_string(),
        "origin".to_string(),
        "main".to_string(),
    ];
    tokio::task::spawn_blocking(move || {
        let output = Command::new("git")
            .args(&full_args)
            .current_dir(&dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to run git");
        output.status.success()
    })
    .await
    .unwrap()
}

/// Helper: set up an agent and repo, push an initial commit with a agentcoderepo.toml.
async fn setup_versioned_repo(
    harness: &TestHarness,
    version: &str,
    source: &str,
    llm_response: &str,
) -> (agentcoderepo_test::TestAgent, String, tempfile::TempDir) {
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "versioned" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    harness.mock_llm.queue_response(llm_response).await;

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    // Write agentcoderepo.toml
    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        format!("[package]\nversion = \"{version}\"\n"),
    )
    .unwrap();

    // Write source file
    std::fs::write(repo_dir.join("lib.rs"), source).unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "initial"]).await;

    let remote_url = format!("{}/git/{}/versioned", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    (agent, token, tmp)
}

#[tokio::test]
async fn initial_push_with_manifest_succeeds() {
    let harness = TestHarness::start().await.unwrap();
    let (_agent, _token, _tmp) = setup_versioned_repo(
        &harness,
        "0.1.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;
    // If we get here, the push succeeded — the initial push has no prior
    // version to compare against, so no semver check.
}

#[tokio::test]
async fn compatible_minor_bump_succeeds() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;

    // Second push: add a new function + bump minor version
    let repo_dir = tmp.path().join("source");

    // Queue LLM response for the new extraction (includes both functions)
    harness
        .mock_llm
        .queue_response(
            r#"[
                {"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"},
                {"name": "multiply", "type": "Int -> Int -> Int", "description": "Multiply two integers"}
            ]"#,
        )
        .await;

    // Update manifest and source
    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"1.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\npub fn multiply(x: i32, y: i32) -> i32 { x * y }\n",
    )
    .unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "add multiply"]).await;

    // This should succeed: adding a function is a minor bump
    assert!(
        try_git_push(&repo_dir, &token).await,
        "minor bump with added function should succeed"
    );
}

#[tokio::test]
async fn breaking_change_without_major_bump_fails() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;

    // Second push: remove the function (breaking!) but only bump minor
    let repo_dir = tmp.path().join("source");

    // Queue LLM responses: one for agentcoderepo.toml (will fail), one for lib.rs
    harness.mock_llm.queue_response(r#"[]"#).await;
    harness.mock_llm.queue_response(r#"[]"#).await;

    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"1.1.0\"\n", // Only minor bump!
    )
    .unwrap();
    std::fs::write(repo_dir.join("lib.rs"), "// empty\n").unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "remove add"]).await;

    // This should fail: removing a function requires a major bump
    assert!(
        !try_git_push(&repo_dir, &token).await,
        "breaking change with only minor bump should be rejected"
    );
}

#[tokio::test]
async fn breaking_change_with_major_bump_succeeds() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;

    // Second push: remove the function (breaking!) with major bump
    let repo_dir = tmp.path().join("source");

    // Queue response with changed type signature
    harness
        .mock_llm
        .queue_response(
            r#"[{"name": "add", "type": "String -> String -> String", "description": "Concatenate strings"}]"#,
        )
        .await;

    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"2.0.0\"\n", // Major bump ✓
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn add(x: &str, y: &str) -> String { format!(\"{x}{y}\") }\n",
    )
    .unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "v2: strings"]).await;

    assert!(
        try_git_push(&repo_dir, &token).await,
        "breaking change with major bump should succeed"
    );
}

#[tokio::test]
async fn generalization_is_non_breaking() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn id(x: i32) -> i32 { x }\n",
        r#"[{"name": "id", "type": "Int -> Int", "description": "Identity"}]"#,
    )
    .await;

    // Second push: generalize Int -> Int to forall a. a -> a (patch bump)
    let repo_dir = tmp.path().join("source");

    harness
        .mock_llm
        .queue_response(
            r#"[{"name": "id", "type": "forall a. a -> a", "description": "Identity"}]"#,
        )
        .await;

    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"1.0.1\"\n", // Just a patch bump
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn id<T>(x: T) -> T { x }\n",
    )
    .unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "generalize id"]).await;

    assert!(
        try_git_push(&repo_dir, &token).await,
        "generalizing a type should be non-breaking (patch bump is fine)"
    );
}

#[tokio::test]
async fn push_without_manifest_skips_semver() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();
    let token = agent.bearer_token();

    let resp = agent
        .post("/api/repos", &serde_json::json!({ "name": "nomanifest" }))
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Queue LLM response
    harness
        .mock_llm
        .queue_response(
            r#"[{"name": "foo", "type": "Int -> Int", "description": "foo"}]"#,
        )
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "test@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Test"]).await;

    // No agentcoderepo.toml — just source
    std::fs::write(repo_dir.join("lib.rs"), "pub fn foo(x: i32) -> i32 { x }\n").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "initial"]).await;

    let remote_url = format!("{}/git/{}/nomanifest", harness.base_url, agent.name);
    git(&repo_dir, &["remote", "add", "origin", &remote_url]).await;
    git_auth(&repo_dir, &token, &["push", "origin", "main"]).await;

    // Should succeed — no manifest means no semver enforcement
    std::mem::forget(tmp);
}

#[tokio::test]
async fn logic_breaking_change_detected_by_llm() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;

    // Second push: same type but different logic (subtract instead of add)
    let repo_dir = tmp.path().join("source");

    // Queue responses (consumed in file order: lib.rs, agentcoderepo.toml, then assessment):
    // 1. lib.rs extraction — same type signature
    harness
        .mock_llm
        .queue_response(
            r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
        )
        .await;
    // 2. agentcoderepo.toml extraction (will fail to parse, but consumes a response)
    harness.mock_llm.queue_response(r#"[]"#).await;
    // 3. Logic assessment — LLM says it's breaking
    harness
        .mock_llm
        .queue_response(
            r#"{"breaking": true, "reason": "function now subtracts instead of adding"}"#,
        )
        .await;

    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"1.0.1\"\n", // Only patch bump — should be rejected
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn add(x: i32, y: i32) -> i32 { x - y }\n", // Breaking logic change!
    )
    .unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "sneaky breaking change"]).await;

    assert!(
        !try_git_push(&repo_dir, &token).await,
        "logic-breaking change should be rejected even with same type"
    );
}

#[tokio::test]
async fn logic_breaking_change_accepted_with_major_bump() {
    let harness = TestHarness::start().await.unwrap();
    let (_, token, tmp) = setup_versioned_repo(
        &harness,
        "1.0.0",
        "pub fn add(x: i32, y: i32) -> i32 { x + y }\n",
        r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
    )
    .await;

    let repo_dir = tmp.path().join("source");

    // Queue responses (consumed in file order: lib.rs, agentcoderepo.toml, then assessment):
    // 1. lib.rs extraction — same type
    harness
        .mock_llm
        .queue_response(
            r#"[{"name": "add", "type": "Int -> Int -> Int", "description": "Add two integers"}]"#,
        )
        .await;
    // 2. agentcoderepo.toml extraction (wasted)
    harness.mock_llm.queue_response(r#"[]"#).await;
    // 3. Logic assessment — breaking
    harness
        .mock_llm
        .queue_response(
            r#"{"breaking": true, "reason": "function now subtracts instead of adding"}"#,
        )
        .await;

    std::fs::write(
        repo_dir.join("agentcoderepo.toml"),
        "[package]\nversion = \"2.0.0\"\n", // Major bump — should be accepted
    )
    .unwrap();
    std::fs::write(
        repo_dir.join("lib.rs"),
        "pub fn add(x: i32, y: i32) -> i32 { x - y }\n",
    )
    .unwrap();

    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "v2: now subtracts"]).await;

    assert!(
        try_git_push(&repo_dir, &token).await,
        "logic-breaking change with major bump should succeed"
    );
}
