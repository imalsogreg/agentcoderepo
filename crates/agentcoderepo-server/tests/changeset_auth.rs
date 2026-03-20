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

struct GitResult {
    success: bool,
    #[allow(dead_code)]
    stderr: String,
}

async fn git_auth_may_fail(dir: &Path, token: &str, args: &[&str]) -> GitResult {
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
        GitResult {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    })
    .await
    .unwrap()
}

async fn git_auth(dir: &Path, token: &str, args: &[&str]) -> String {
    let result = git_auth_may_fail(dir, token, args).await;
    if !result.success {
        panic!("git failed: {}", result.stderr);
    }
    String::new()
}

/// Set up a local repo with one commit, ready to push.
async fn init_local_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init"]).await;
    git(dir, &["config", "user.email", "test@test.com"]).await;
    git(dir, &["config", "user.name", "Test"]).await;
    std::fs::write(dir.join("file.txt"), "content").unwrap();
    git(dir, &["add", "."]).await;
    git(dir, &["commit", "-m", "initial"]).await;
}

#[tokio::test]
async fn non_owner_cannot_push_to_main() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let other = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "protected" }))
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    init_local_repo(&repo_dir).await;

    let remote = format!("{}/git/{}/protected", harness.base_url, owner.name);
    git(&repo_dir, &["remote", "add", "origin", &remote]).await;

    // Other agent tries to push to main → rejected
    let result = git_auth_may_fail(&repo_dir, &other.bearer_token(), &["push", "origin", "main"]).await;
    assert!(!result.success, "non-owner should not be able to push to main");
}

#[tokio::test]
async fn changeset_author_can_push_to_their_ref() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "collab" }))
        .await
        .unwrap();

    // Owner pushes initial commit
    let tmp = tempfile::tempdir().unwrap();
    let owner_dir = tmp.path().join("owner-src");
    init_local_repo(&owner_dir).await;
    let remote = format!("{}/git/{}/collab", harness.base_url, owner.name);
    git(&owner_dir, &["remote", "add", "origin", &remote]).await;
    git_auth(&owner_dir, &owner.bearer_token(), &["push", "origin", "main"]).await;

    // Contributor creates changeset
    let resp = contributor
        .post(
            &format!("/api/repos/{}/collab/changesets", owner.name),
            &serde_json::json!({ "description": "My contribution" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let ref_name = cs["ref_name"].as_str().unwrap();

    // Contributor clones and pushes to their changeset ref
    let contrib_dir = tmp.path().join("contrib-src");
    git_auth(tmp.path(), &contributor.bearer_token(), &["clone", &remote, "contrib-src"]).await;
    git(&contrib_dir, &["config", "user.email", "contrib@test.com"]).await;
    git(&contrib_dir, &["config", "user.name", "Contrib"]).await;
    std::fs::write(contrib_dir.join("new.txt"), "contribution").unwrap();
    git(&contrib_dir, &["add", "."]).await;
    git(&contrib_dir, &["commit", "-m", "add new file"]).await;

    let result = git_auth_may_fail(
        &contrib_dir,
        &contributor.bearer_token(),
        &["push", "origin", &format!("HEAD:{ref_name}")],
    )
    .await;
    assert!(result.success, "changeset author should be able to push to their ref");
}

#[tokio::test]
async fn other_agent_cannot_push_to_someone_elses_changeset() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let author = harness.registered_agent().await.unwrap();
    let intruder = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "guarded" }))
        .await
        .unwrap();

    // Owner pushes initial commit
    let tmp = tempfile::tempdir().unwrap();
    let owner_dir = tmp.path().join("owner-src");
    init_local_repo(&owner_dir).await;
    let remote = format!("{}/git/{}/guarded", harness.base_url, owner.name);
    git(&owner_dir, &["remote", "add", "origin", &remote]).await;
    git_auth(&owner_dir, &owner.bearer_token(), &["push", "origin", "main"]).await;

    // Author creates changeset
    let resp = author
        .post(
            &format!("/api/repos/{}/guarded/changesets", owner.name),
            &serde_json::json!({ "description": "Author's changeset" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let ref_name = cs["ref_name"].as_str().unwrap();

    // Intruder tries to push to author's changeset ref → rejected
    let intruder_dir = tmp.path().join("intruder-src");
    git_auth(tmp.path(), &intruder.bearer_token(), &["clone", &remote, "intruder-src"]).await;
    git(&intruder_dir, &["config", "user.email", "intruder@test.com"]).await;
    git(&intruder_dir, &["config", "user.name", "Intruder"]).await;
    std::fs::write(intruder_dir.join("evil.txt"), "malicious").unwrap();
    git(&intruder_dir, &["add", "."]).await;
    git(&intruder_dir, &["commit", "-m", "evil commit"]).await;

    let result = git_auth_may_fail(
        &intruder_dir,
        &intruder.bearer_token(),
        &["push", "origin", &format!("HEAD:{ref_name}")],
    )
    .await;
    assert!(!result.success, "intruder should not be able to push to someone else's changeset");
}

#[tokio::test]
async fn cannot_push_to_arbitrary_ref() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "strict" }))
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    init_local_repo(&repo_dir).await;
    let remote = format!("{}/git/{}/strict", harness.base_url, owner.name);
    git(&repo_dir, &["remote", "add", "origin", &remote]).await;

    // Even the owner can't push to arbitrary refs
    let result = git_auth_may_fail(
        &repo_dir,
        &owner.bearer_token(),
        &["push", "origin", "HEAD:refs/tags/v1.0"],
    )
    .await;
    assert!(!result.success, "should not be able to push to arbitrary refs");
}
