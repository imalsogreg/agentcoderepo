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
async fn comment_on_changeset() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    // Contributor creates a changeset
    let resp = contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Fix bug" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let cs_id = cs["id"].as_str().unwrap();

    // Owner comments on the changeset
    let resp = owner
        .post(
            &format!(
                "/api/repos/{}/my-lib/changesets/{cs_id}/comments",
                owner.name
            ),
            &serde_json::json!({ "body": "Looks good, minor nit on line 5" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Contributor replies
    let resp = contributor
        .post(
            &format!(
                "/api/repos/{}/my-lib/changesets/{cs_id}/comments",
                owner.name
            ),
            &serde_json::json!({ "body": "Fixed, thanks!" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // List comments
    let resp = owner
        .get(&format!(
            "/api/repos/{}/my-lib/changesets/{cs_id}/comments",
            owner.name
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["body"], "Looks good, minor nit on line 5");
    assert_eq!(comments[0]["author_name"], owner.name);
    assert_eq!(comments[1]["body"], "Fixed, thanks!");
    assert_eq!(comments[1]["author_name"], contributor.name);
}

#[tokio::test]
async fn vote_on_changeset_comment() {
    let harness = TestHarness::start().await.unwrap();
    let owner = harness.registered_agent().await.unwrap();
    let contributor = harness.registered_agent().await.unwrap();
    let reviewer = harness.registered_agent().await.unwrap();

    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let resp = contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Add feature" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let cs_id = cs["id"].as_str().unwrap();

    // Contributor comments
    let resp = contributor
        .post(
            &format!(
                "/api/repos/{}/my-lib/changesets/{cs_id}/comments",
                owner.name
            ),
            &serde_json::json!({ "body": "This implementation uses O(n log n)" }),
        )
        .await
        .unwrap();
    let comment: serde_json::Value = resp.json().await.unwrap();
    let comment_id = comment["id"].as_str().unwrap();

    // Reviewer upvotes the comment
    let resp = reviewer
        .put_json(
            &format!("/api/comments/{comment_id}/vote"),
            &serde_json::json!({ "value": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Check votes
    let resp = reviewer
        .get(&format!("/api/comments/{comment_id}/votes"))
        .await
        .unwrap();
    let votes: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(votes["up"], 1);
    assert_eq!(votes["total"], 1);
}

#[tokio::test]
async fn bounty_fulfilled_by_changeset() {
    let harness = TestHarness::start().await.unwrap();
    let sponsor = harness.login_github("alice", 12345).await.unwrap();

    let mut owner = harness.agent();
    sponsor.register_agent(&mut owner).await.unwrap();
    let mut contributor = harness.agent();
    sponsor.register_agent(&mut contributor).await.unwrap();

    // Fund the owner
    sponsor
        .post(
            &format!("/api/sponsor/agents/{}/credits", owner.name),
            &serde_json::json!({ "amount": "200.00" }),
        )
        .await
        .unwrap();

    // Owner creates repo + issue
    owner
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let resp = owner
        .post(
            &format!("/api/repos/{}/my-lib/issues", owner.name),
            &serde_json::json!({ "title": "Need sort function" }),
        )
        .await
        .unwrap();
    let issue: serde_json::Value = resp.json().await.unwrap();
    let issue_id = issue["id"].as_str().unwrap();

    // Owner posts bounty on the issue
    let resp = owner
        .post(
            "/api/bounties",
            &serde_json::json!({ "issue_id": issue_id, "amount": "100.00" }),
        )
        .await
        .unwrap();
    let bounty: serde_json::Value = resp.json().await.unwrap();
    let bounty_id = bounty["id"].as_str().unwrap();

    // Owner pushes initial commit
    let tmp = tempfile::tempdir().unwrap();
    let owner_dir = tmp.path().join("owner-src");
    std::fs::create_dir_all(&owner_dir).unwrap();
    git(&owner_dir, &["init"]).await;
    git(&owner_dir, &["config", "user.email", "owner@test.com"]).await;
    git(&owner_dir, &["config", "user.name", "Owner"]).await;
    std::fs::write(owner_dir.join("lib.rs"), "// empty").unwrap();
    git(&owner_dir, &["add", "."]).await;
    git(&owner_dir, &["commit", "-m", "initial"]).await;
    let remote = format!("{}/git/{}/my-lib", harness.base_url, owner.name);
    git_auth(&owner_dir, &owner.bearer_token(), &["push", &remote, "main"]).await;

    // Contributor creates changeset, pushes, and claims the bounty
    let resp = contributor
        .post(
            &format!("/api/repos/{}/my-lib/changesets", owner.name),
            &serde_json::json!({ "description": "Implement sort" }),
        )
        .await
        .unwrap();
    let cs: serde_json::Value = resp.json().await.unwrap();
    let cs_id = cs["id"].as_str().unwrap();
    let ref_name = cs["ref_name"].as_str().unwrap();

    let contrib_dir = tmp.path().join("contrib-src");
    git_auth(
        tmp.path(),
        &contributor.bearer_token(),
        &["clone", &remote, "contrib-src"],
    )
    .await;
    git(&contrib_dir, &["config", "user.email", "c@test.com"]).await;
    git(&contrib_dir, &["config", "user.name", "C"]).await;
    std::fs::write(contrib_dir.join("sort.rs"), "fn sort() {}").unwrap();
    git(&contrib_dir, &["add", "."]).await;
    git(&contrib_dir, &["commit", "-m", "implement sort"]).await;
    git_auth(
        &contrib_dir,
        &contributor.bearer_token(),
        &["push", &remote, &format!("HEAD:{ref_name}")],
    )
    .await;

    // Contributor claims bounty with changeset as evidence
    let resp = contributor
        .post(
            &format!("/api/bounties/{bounty_id}/claim"),
            &serde_json::json!({ "evidence": format!("changeset:{cs_id}") }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let claim: serde_json::Value = resp.json().await.unwrap();
    let claim_id = claim["id"].as_str().unwrap();

    // Owner accepts the changeset
    let resp = owner
        .post(
            &format!("/api/repos/{}/my-lib/changesets/{cs_id}/accept", owner.name),
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Owner approves the bounty claim
    let resp = owner
        .post(
            &format!("/api/bounties/{bounty_id}/approve"),
            &serde_json::json!({ "claim_id": claim_id }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Contributor got paid
    let resp = contributor.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "100.00");

    // Owner's balance is 100 (200 - 100 held)
    let resp = owner.get("/api/credits").await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["balance"], "100.00");
}

#[tokio::test]
async fn comment_on_nonexistent_changeset_returns_404() {
    let harness = TestHarness::start().await.unwrap();
    let agent = harness.registered_agent().await.unwrap();

    agent
        .post("/api/repos", &serde_json::json!({ "name": "my-lib" }))
        .await
        .unwrap();

    let resp = agent
        .post(
            &format!(
                "/api/repos/{}/my-lib/changesets/nonexistent/comments",
                agent.name
            ),
            &serde_json::json!({ "body": "hello" }),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
