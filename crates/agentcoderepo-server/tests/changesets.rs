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
async fn create_changeset_returns_push_info() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();

    // Owner creates a repo
    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    // Contributor creates a changeset
    let resp = contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Add helper function" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let cs: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(cs["status"], "proposed");
    assert_eq!(cs["author_name"], contributor.name);
    assert_eq!(cs["description"], "Add helper function");
    assert!(cs["ref_name"].as_str().unwrap().starts_with("refs/changesets/"));
    assert!(cs["push_url"].as_str().unwrap().contains("/git/"));
}

#[tokio::test]
async fn list_changesets_filters_by_status() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    // Create two changesets
    contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Change A" }),
        )
        .await
        .unwrap();

    contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Change B" }),
        )
        .await
        .unwrap();

    // Default: proposed
    let resp = contributor
        .get(&format!("/api/repos/{}/my-lib/changesets", owner.name))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let list: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 2);

    // All
    let resp = contributor
        .get(&format!(
            "/api/repos/{}/my-lib/changesets?status=all",
            owner.name
        ))
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn get_changeset_shows_commits_after_push() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let owner_token = owner.bearer_token();
    let contributor = harness.registered_agent().await.unwrap();
    let contributor_token = contributor.bearer_token();

    // Owner creates repo and pushes initial commit
    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("source");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git(&repo_dir, &["init"]).await;
    git(&repo_dir, &["config", "user.email", "owner@test.com"]).await;
    git(&repo_dir, &["config", "user.name", "Owner"]).await;
    std::fs::write(repo_dir.join("lib.rs"), "fn main() {}").unwrap();
    git(&repo_dir, &["add", "."]).await;
    git(&repo_dir, &["commit", "-m", "initial"]).await;

    let remote = format!("{}/git/{}/my-lib", harness.base_url, owner.name);
    git_auth(&repo_dir, &owner_token, &["push", &remote, "main"]).await;

    // Contributor creates changeset
    let resp = contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Add helper" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let cs_id = cs["id"].as_str().unwrap();
    let ref_name = cs["ref_name"].as_str().unwrap();

    // Contributor clones, creates a branch, and pushes to changeset ref
    let contrib_dir = tmp.path().join("contrib");
    git_auth(
        tmp.path(),
        &contributor_token,
        &["clone", &remote, "contrib"],
    )
    .await;
    git(&contrib_dir, &["config", "user.email", "contrib@test.com"]).await;
    git(&contrib_dir, &["config", "user.name", "Contributor"]).await;
    std::fs::write(contrib_dir.join("helper.rs"), "fn help() {}").unwrap();
    git(&contrib_dir, &["add", "."]).await;
    git(&contrib_dir, &["commit", "-m", "add helper function"]).await;

    // Push to the changeset ref
    git_auth(
        &contrib_dir,
        &contributor_token,
        &["push", &remote, &format!("HEAD:{ref_name}")],
    )
    .await;

    // Get changeset detail — should show the commit
    let resp = contributor
        .get(&format!(
            "/api/repos/{}/my-lib/changesets/{cs_id}",
            owner.name
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(detail["status"], "proposed");
    let commits = detail["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["message"], "add helper function");
    assert_eq!(commits[0]["author"], "Contributor");
}

#[tokio::test]
async fn changeset_on_nonexistent_repo_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/no-repo/changesets", agent.name),
            &serde_json::json!({ "description": "Nope" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn empty_description_rejected() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!("/api/repos/{}/my-lib/changesets", agent.name),
            &serde_json::json!({ "description": "" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
