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

/// Set up: owner creates repo, pushes initial commit, contributor creates changeset and pushes.
/// Returns (owner, contributor, changeset_id, remote_url).
async fn setup_changeset(
    harness: &TestHarness,
) -> (
    agentcoderepo_test::TestAgent,
    agentcoderepo_test::TestAgent,
    String,
    String,
) {
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "the-repo" }))
        .await
        .unwrap();

    let remote = format!("{}/git/{}/the-repo", harness.base_url, owner.name);

    // Owner pushes initial commit
    let tmp = tempfile::tempdir().unwrap();
    let owner_dir = tmp.path().join("owner-src");
    std::fs::create_dir_all(&owner_dir).unwrap();
    git(&owner_dir, &["init"]).await;
    git(&owner_dir, &["config", "user.email", "owner@test.com"]).await;
    git(&owner_dir, &["config", "user.name", "Owner"]).await;
    std::fs::write(owner_dir.join("main.rs"), "fn main() {}").unwrap();
    git(&owner_dir, &["add", "."]).await;
    git(&owner_dir, &["commit", "-m", "initial"]).await;
    git_auth(&owner_dir, &owner.bearer_token(), &["push", &remote, "main"]).await;

    // Contributor creates changeset
    let resp = contributor
        .post(
            &format!("/api/repos/{}/the-repo/changesets", owner.name),
            &serde_json::json!({ "description": "Add helper" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let cs_id = cs["id"].as_str().unwrap().to_string();
    let ref_name = cs["ref_name"].as_str().unwrap().to_string();

    // Contributor clones, commits, pushes to changeset ref
    let contrib_dir = tmp.path().join("contrib-src");
    git_auth(
        tmp.path(),
        &contributor.bearer_token(),
        &["clone", &remote, "contrib-src"],
    )
    .await;
    git(&contrib_dir, &["config", "user.email", "contrib@test.com"]).await;
    git(&contrib_dir, &["config", "user.name", "Contributor"]).await;
    std::fs::write(contrib_dir.join("helper.rs"), "fn help() {}").unwrap();
    git(&contrib_dir, &["add", "."]).await;
    git(&contrib_dir, &["commit", "-m", "add helper"]).await;
    git_auth(
        &contrib_dir,
        &contributor.bearer_token(),
        &["push", &remote, &format!("HEAD:{ref_name}")],
    )
    .await;

    (owner, contributor, cs_id, remote)
}

#[tokio::test]
async fn accept_changeset_merges_to_main() {
    let harness = TestHarness::start().await.unwrap();
    let (owner, _contributor, cs_id, remote) = setup_changeset(&harness).await;

    // Owner accepts
    let resp = owner
        .post(
            &format!("/api/repos/{}/the-repo/changesets/{cs_id}/accept", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
    assert!(!body["new_head"].as_str().unwrap().is_empty());

    // Verify the commit is now on main by cloning
    let tmp = tempfile::tempdir().unwrap();
    let clone_dir = tmp.path().join("verify");
    git_auth(
        tmp.path(),
        &owner.bearer_token(),
        &["clone", &remote, "verify"],
    )
    .await;

    // helper.rs should exist on main
    assert!(clone_dir.join("helper.rs").exists());
    let log = git(&clone_dir, &["log", "--oneline"]).await;
    assert!(log.contains("add helper"));

    // Changeset should be marked accepted
    let resp = owner
        .get(&format!("/api/repos/{}/the-repo/changesets/{cs_id}", owner.name))
        .await
        .unwrap();
    let detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(detail["status"], "accepted");
}

#[tokio::test]
async fn only_owner_can_accept() {
    let harness = TestHarness::start().await.unwrap();
    let (_owner, contributor, cs_id, _remote) = setup_changeset(&harness).await;

    // Contributor tries to accept → 403
    let resp = contributor
        .post(
            &format!("/api/repos/{}/the-repo/changesets/{cs_id}/accept", _owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn reject_changeset() {
    let harness = TestHarness::start().await.unwrap();
    let (owner, _contributor, cs_id, _remote) = setup_changeset(&harness).await;

    let resp = owner
        .post(
            &format!("/api/repos/{}/the-repo/changesets/{cs_id}/reject", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Changeset is now rejected
    let resp = owner
        .get(&format!(
            "/api/repos/{}/the-repo/changesets/{cs_id}",
            owner.name
        ))
        .await
        .unwrap();
    let detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(detail["status"], "rejected");
}

#[tokio::test]
async fn author_can_withdraw() {
    let harness = TestHarness::start().await.unwrap();
    let (owner, contributor, cs_id, _remote) = setup_changeset(&harness).await;

    let resp = contributor
        .post(
            &format!("/api/repos/{}/the-repo/changesets/{cs_id}/withdraw", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let resp = owner
        .get(&format!(
            "/api/repos/{}/the-repo/changesets/{cs_id}",
            owner.name
        ))
        .await
        .unwrap();
    let detail: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(detail["status"], "withdrawn");
}

#[tokio::test]
async fn non_author_cannot_withdraw() {
    let harness = TestHarness::start().await.unwrap();
    let (owner, _contributor, cs_id, _remote) = setup_changeset(&harness).await;

    // Owner tries to withdraw contributor's changeset → 403
    let resp = owner
        .post(
            &format!("/api/repos/{}/the-repo/changesets/{cs_id}/withdraw", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}
