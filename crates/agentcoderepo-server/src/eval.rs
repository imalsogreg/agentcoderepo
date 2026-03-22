use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::AuthAgent;
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::sprites::SpritesClient;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EvalRequest {
    pub language: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct EvalResponse {
    pub id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub status: String,
}

impl TextFormat for EvalResponse {
    fn to_text(&self) -> String {
        let mut s = format!("[{}] exit={} ({}ms)\n", self.status, self.exit_code, self.duration_ms);
        if !self.stdout.is_empty() {
            s.push_str(&format!("--- stdout ---\n{}\n", self.stdout));
        }
        if !self.stderr.is_empty() {
            s.push_str(&format!("--- stderr ---\n{}\n", self.stderr));
        }
        s
    }
}

#[derive(Serialize)]
pub struct SpriteStatusResponse {
    pub provisioned: bool,
    pub sprite_name: Option<String>,
    pub status: Option<String>,
}

impl TextFormat for SpriteStatusResponse {
    fn to_text(&self) -> String {
        if self.provisioned {
            format!(
                "provisioned: {} ({})\n",
                self.sprite_name.as_deref().unwrap_or("?"),
                self.status.as_deref().unwrap_or("?"),
            )
        } else {
            "not provisioned\n".to_string()
        }
    }
}

#[derive(Serialize)]
pub struct ProvisionResponse {
    pub sprite_name: String,
    pub checkpoint_id: String,
    pub status: String,
}

impl TextFormat for ProvisionResponse {
    fn to_text(&self) -> String {
        format!(
            "provisioned: {} (checkpoint: {})\n",
            self.sprite_name, self.checkpoint_id,
        )
    }
}

// ---------------------------------------------------------------------------
// Language support
// ---------------------------------------------------------------------------

fn file_extension(language: &str) -> Option<&'static str> {
    match language {
        "python" => Some("py"),
        "javascript" => Some("js"),
        "typescript" => Some("ts"),
        "rust" => Some("rs"),
        "haskell" => Some("hs"),
        "c" => Some("c"),
        "cpp" => Some("cpp"),
        _ => None,
    }
}

/// Generate the runner script for a language.
/// This script enters the nix dev environment and runs the agent's code.
fn runner_script(language: &str) -> Option<String> {
    let script = match language {
        "python" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  PYTHONPATH=/home/sprite/repo/impl/python:${PYTHONPATH:-} \
  timeout 10 python /home/sprite/eval/agent_code.py
'
"#,
        "javascript" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  NODE_PATH=/home/sprite/repo/impl/javascript:${NODE_PATH:-} \
  timeout 10 node /home/sprite/eval/agent_code.js
'
"#,
        "typescript" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  NODE_PATH=/home/sprite/repo/impl/typescript:${NODE_PATH:-} \
  timeout 10 npx tsx /home/sprite/eval/agent_code.ts
'
"#,
        "rust" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  cd /home/sprite/eval
  timeout 30 cargo run --release 2>&1
'
"#,
        "haskell" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  timeout 10 runghc -i/home/sprite/repo/impl/haskell \
    /home/sprite/eval/agent_code.hs
'
"#,
        "c" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  gcc -I/home/sprite/repo/impl/c -o /tmp/eval_bin \
    /home/sprite/eval/agent_code.c \
    $(find /home/sprite/repo/impl/c -name "*.c" ! -name "agent_code.c" 2>/dev/null) && \
  timeout 10 /tmp/eval_bin
'
"#,
        "cpp" => r#"#!/bin/bash
set -e
cd /home/sprite/repo
exec nix develop --command bash -c '
  g++ -I/home/sprite/repo/impl/cpp -o /tmp/eval_bin \
    /home/sprite/eval/agent_code.cpp \
    $(find /home/sprite/repo/impl/cpp -name "*.cpp" ! -name "agent_code.cpp" 2>/dev/null) && \
  timeout 10 /tmp/eval_bin
'
"#,
        _ => return None,
    };
    Some(script.to_string())
}

// ---------------------------------------------------------------------------
// Eval handler
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/eval
pub async fn eval_code(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    Json(body): Json<EvalRequest>,
) -> Result<Negotiated<EvalResponse>, StatusCode> {
    let sprites_config = state.sprites.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let ext = file_extension(&body.language).ok_or(StatusCode::BAD_REQUEST)?;
    let runner = runner_script(&body.language).ok_or(StatusCode::BAD_REQUEST)?;

    if body.code.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Look up repo
    let repo_row = conn
        .query(
            "SELECT r.id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner.clone(), repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo_id: String = repo_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Look up provisioned sprite
    let sprite_row = conn
        .query(
            "SELECT sprite_name, clean_checkpoint_id, status FROM repo_sprites WHERE repo_id = ?1",
            [repo_id.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?; // 404 = sprite not provisioned

    let sprite_name: String = sprite_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let checkpoint_id: String = sprite_row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let sprite_status: String = sprite_row.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if sprite_status != "ready" {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let client = SpritesClient::new(&sprites_config.base_url, &sprites_config.token);

    // Restore to clean checkpoint
    client
        .restore(&sprite_name, &checkpoint_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to restore sprite checkpoint");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Write agent's code
    let code_path = format!("/home/sprite/eval/agent_code.{ext}");
    client
        .write_file(&sprite_name, &code_path, body.code.as_bytes())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to write agent code to sprite");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Write runner script
    client
        .write_file(&sprite_name, "/home/sprite/eval/run.sh", runner.as_bytes())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to write runner script to sprite");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Execute
    let start = Instant::now();
    let result = client
        .exec(&sprite_name, &["bash", "/home/sprite/eval/run.sh"], &[])
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sprite exec failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let status = if result.exit_code == 0 {
        "completed"
    } else if result.exit_code == 124 {
        "timeout" // `timeout` command returns 124
    } else {
        "failed"
    };

    // Record eval run
    let eval_id = uuid::Uuid::new_v4().to_string();
    let _ = conn
        .execute(
            "INSERT INTO eval_runs (id, repo_id, agent_id, language, code, stdout, stderr, exit_code, duration_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            turso::params![
                eval_id.clone(),
                repo_id,
                agent.agent_id.to_string(),
                body.language,
                body.code,
                result.stdout.clone(),
                result.stderr.clone(),
                result.exit_code as i64,
                duration_ms as i64,
                status.to_string(),
            ],
        )
        .await;

    Ok(neg.ok(EvalResponse {
        id: eval_id,
        stdout: result.stdout,
        stderr: result.stderr,
        exit_code: result.exit_code,
        duration_ms,
        status: status.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// Provisioning
// ---------------------------------------------------------------------------

/// POST /api/repos/{owner}/{repo}/sprites/provision  (repo owner only)
pub async fn provision_sprite(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    agent: AuthAgent,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
) -> Result<Negotiated<ProvisionResponse>, StatusCode> {
    let sprites_config = state.sprites.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Verify repo ownership
    let repo_row = conn
        .query(
            "SELECT r.id, a.id as owner_id FROM repos r
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner.clone(), repo.clone()],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let repo_id: String = repo_row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let owner_id: String = repo_row.get::<String>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if owner_id != agent.agent_id.to_string() {
        return Err(StatusCode::FORBIDDEN);
    }

    let sprite_name = format!("acr-{owner}-{repo}");
    let client = SpritesClient::new(&sprites_config.base_url, &sprites_config.token);

    // Create sprite
    client.create(&sprite_name).await.map_err(|e| {
        tracing::error!(error = %e, "failed to create sprite");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Write and run setup script
    let git_url = format!("https://agentcoderepo.fly.dev/git/{owner}/{repo}");
    let setup_script = format!(
        r#"#!/bin/bash
set -ex
# Clone the repo
git clone {git_url} /home/sprite/repo || true
cd /home/sprite/repo
# Create eval directory
mkdir -p /home/sprite/eval
# Enter nix environment to cache dependencies
if [ -f flake.nix ]; then
    nix develop --command echo "nix environment ready"
fi
echo "setup complete"
"#
    );

    client
        .write_file(&sprite_name, "/home/sprite/setup.sh", setup_script.as_bytes())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to write setup script");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let setup_result = client
        .exec(&sprite_name, &["bash", "/home/sprite/setup.sh"], &[])
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sprite setup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if setup_result.exit_code != 0 {
        tracing::error!(
            stdout = %setup_result.stdout,
            stderr = %setup_result.stderr,
            "sprite setup script failed"
        );
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Create clean checkpoint
    let checkpoint_id = client
        .checkpoint(&sprite_name, "clean state after provisioning")
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to create sprite checkpoint");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Record in DB
    conn.execute(
        "INSERT OR REPLACE INTO repo_sprites (repo_id, sprite_name, clean_checkpoint_id, status, updated_at)
         VALUES (?1, ?2, ?3, 'ready', datetime('now'))",
        [repo_id, sprite_name.clone(), checkpoint_id.clone()],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(neg.created(ProvisionResponse {
        sprite_name,
        checkpoint_id,
        status: "ready".to_string(),
    }))
}

/// GET /api/repos/{owner}/{repo}/sprites/status
pub async fn sprite_status(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
) -> Result<Negotiated<SpriteStatusResponse>, StatusCode> {
    let conn = state.db.connect().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let row = conn
        .query(
            "SELECT rs.sprite_name, rs.status
             FROM repo_sprites rs
             JOIN repos r ON rs.repo_id = r.id
             JOIN agents a ON r.owner_id = a.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner, repo],
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .next()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match row {
        Some(r) => Ok(neg.ok(SpriteStatusResponse {
            provisioned: true,
            sprite_name: Some(r.get::<String>(0).unwrap_or_default()),
            status: Some(r.get::<String>(1).unwrap_or_default()),
        })),
        None => Ok(neg.ok(SpriteStatusResponse {
            provisioned: false,
            sprite_name: None,
            status: None,
        })),
    }
}
