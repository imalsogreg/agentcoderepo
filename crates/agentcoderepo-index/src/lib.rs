//! AgentCodeRepo indexing pipeline.
//!
//! After a git push completes, this crate extracts function signatures
//! and generates vector embeddings for the changed files, storing the
//! results in the database. If a `agentcoderepo.toml` manifest is present,
//! semver validation is enforced: breaking changes require a major bump.

pub mod extract;
pub mod git;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use agentcoderepo_llm::LlmClient;
use agentcoderepo_types::semver::{SigChange, Version, diff_signatures, validate_bump};
use agentcoderepo_types::manifest::parse_manifest;
use agentcoderepo_types::FunctionSig;
use sha2::{Digest, Sha256};
use turso::Database;

use git::FileStatus;

/// Index all changes from a push.
///
/// Called synchronously after `git receive-pack` completes, before
/// the HTTP response is returned to the client. Returns an error
/// if semver validation fails (the push should be rejected).
#[tracing::instrument(skip(llm, db))]
pub async fn index_push(
    repo_path: &Path,
    repo_id: &str,
    old_sha: &str,
    new_sha: &str,
    llm: &dyn LlmClient,
    db: &Database,
) -> Result<()> {
    tracing::info!(%old_sha, %new_sha, "starting index_push");

    let conn = db.connect().context("failed to connect to db")?;

    // -----------------------------------------------------------------------
    // Step 0: Read agentcoderepo.toml from the new commit (optional)
    // -----------------------------------------------------------------------
    let new_manifest = match git::read_file_at_commit(repo_path, new_sha, "agentcoderepo.toml").await {
        Ok(content) => match parse_manifest(&content) {
            Ok(m) => {
                tracing::info!(version = %m.package.version, "found agentcoderepo.toml");
                Some(m)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse agentcoderepo.toml, skipping semver check");
                None
            }
        },
        Err(_) => {
            tracing::debug!("no agentcoderepo.toml found, skipping semver check");
            None
        }
    };

    // -----------------------------------------------------------------------
    // Step 0b: Load previous version and signatures (if any)
    // -----------------------------------------------------------------------
    let prev_version = load_previous_version(&conn, repo_id).await;
    let prev_sigs = load_previous_signatures(&conn, repo_id).await;

    // -----------------------------------------------------------------------
    // Step 1: Compute changed files and extract new signatures
    // -----------------------------------------------------------------------
    let changed = git::changed_files(repo_path, old_sha, new_sha)
        .await
        .context("failed to get changed files")?;

    tracing::info!(file_count = changed.len(), "indexing changed files");

    // Handle deletions — remove signatures for deleted files
    for file in changed.iter().filter(|f| f.status == FileStatus::Deleted) {
        tracing::debug!(path = %file.path, "removing signatures for deleted file");
        conn.execute(
            "DELETE FROM function_signatures WHERE repo_id = ?1 AND file_path = ?2",
            [repo_id.to_string(), file.path.clone()],
        )
        .await
        .context("failed to delete signatures for removed file")?;
    }

    // Index added/modified files
    let indexable: Vec<_> = changed
        .iter()
        .filter(|f| f.status != FileStatus::Deleted)
        .collect();

    let mut all_new_sigs = Vec::new();
    // Track which function names came from which file paths (for logic assessment)
    let mut function_files: HashMap<String, String> = HashMap::new();
    // Track which files were modified (vs added) — we only assess logic changes for modifications
    let modified_files: HashSet<&str> = changed
        .iter()
        .filter(|f| f.status == FileStatus::Modified)
        .map(|f| f.path.as_str())
        .collect();

    for file in &indexable {
        let source = match git::read_file_at_commit(repo_path, new_sha, &file.path).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %file.path, error = %e, "skipping file, could not read");
                continue;
            }
        };

        // Skip binary / empty files
        if source.is_empty() || source.contains('\0') {
            continue;
        }

        tracing::debug!(path = %file.path, bytes = source.len(), "extracting signatures");

        // Extract type signatures via LLM
        let signatures = match extract::extract_signatures(llm, &file.path, &source).await {
            Ok(sigs) => sigs,
            Err(e) => {
                tracing::warn!(path = %file.path, error = %e, "signature extraction failed");
                continue;
            }
        };

        // Remove old signatures for this file before inserting new ones.
        // This must happen even if extraction returned empty (functions removed).
        conn.execute(
            "DELETE FROM function_signatures WHERE repo_id = ?1 AND file_path = ?2",
            [repo_id.to_string(), file.path.clone()],
        )
        .await?;

        if signatures.is_empty() {
            continue;
        }

        // Collect function source snippets for embedding
        let mut sig_ids = Vec::new();
        let mut embed_texts = Vec::new();

        let language = detect_language(&file.path);

        for sig in &signatures {
            let sig_id = uuid::Uuid::new_v4().to_string();

            let type_text = sig.ty.to_string();
            let type_normalized = sig.ty.alpha_normalize().to_string();
            let type_json =
                serde_json::to_string(&sig.ty).context("failed to serialize type to JSON")?;

            conn.execute(
                "INSERT INTO function_signatures (id, repo_id, file_path, language, function_name, type_signature, type_normalized, type_json, description, commit_sha)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                [
                    sig_id.clone(),
                    repo_id.to_string(),
                    file.path.clone(),
                    language.clone(),
                    sig.name.clone(),
                    type_text.clone(),
                    type_normalized,
                    type_json,
                    sig.description.clone(),
                    new_sha.to_string(),
                ],
            )
            .await
            .context("failed to insert function signature")?;

            // Build the embedding input
            let embed_text = format!(
                "Function: {}\nType: {}\nDescription: {}\nFile: {}\n\nSource:\n{}",
                sig.name, type_text, sig.description, file.path, source
            );

            sig_ids.push(sig_id);
            embed_texts.push(embed_text);
        }

        for sig in &signatures {
            function_files.insert(sig.name.clone(), file.path.clone());
        }
        all_new_sigs.extend(signatures);

        // Generate embeddings in batch
        if !embed_texts.is_empty() {
            match llm.embed(&embed_texts).await {
                Ok(embeddings) => {
                    for (sig_id, embedding) in sig_ids.iter().zip(embeddings.iter()) {
                        let source_hash = hex_sha256(&embed_texts[0]);
                        let emb_id = uuid::Uuid::new_v4().to_string();

                        let emb_json = serde_json::to_string(embedding)
                            .context("failed to serialize embedding")?;

                        conn.execute(
                            "INSERT OR REPLACE INTO function_embeddings (id, signature_id, embedding, source_hash)
                             VALUES (?1, ?2, vector(?3), ?4)",
                            [
                                emb_id,
                                sig_id.clone(),
                                emb_json,
                                source_hash,
                            ],
                        )
                        .await
                        .context("failed to insert embedding")?;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "embedding generation failed, skipping");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 2: Semver validation (if manifest is present and there's a prior version)
    // -----------------------------------------------------------------------
    if let Some(ref manifest) = new_manifest {
        let new_version = &manifest.package.version;

        if let Some(old_version) = prev_version {
            if !prev_sigs.is_empty() || !all_new_sigs.is_empty() {
                // Build complete new signature set: unchanged sigs from DB + newly extracted
                let new_sigs = load_current_signatures(&conn, repo_id).await;

                let mut diff = diff_signatures(&prev_sigs, &new_sigs);

                // Logic-breaking assessment: for functions whose type is unchanged
                // but whose source file was modified, ask the LLM if the behavioral
                // change is breaking.
                let is_initial_push = old_sha == "0000000000000000000000000000000000000000";
                if !is_initial_push {
                    for (name, change) in &mut diff.changes {
                        if !matches!(change, SigChange::Unchanged) {
                            continue;
                        }
                        // Only assess functions from modified files
                        let file_path = match function_files.get(name.as_str()) {
                            Some(p) if modified_files.contains(p.as_str()) => p.clone(),
                            _ => continue,
                        };
                        // Read old and new source for comparison
                        let old_source = match git::read_file_at_commit(repo_path, old_sha, &file_path).await {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        let new_source = match git::read_file_at_commit(repo_path, new_sha, &file_path).await {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        // Skip if source is identical
                        if old_source == new_source {
                            continue;
                        }
                        if let Some(reason) = extract::assess_logic_breaking(llm, name, &old_source, &new_source).await {
                            tracing::info!(function = %name, %reason, "LLM detected logic-breaking change");
                            *change = SigChange::LogicBreaking { reason };
                        }
                    }
                }

                if let Err(msg) = validate_bump(&old_version, new_version, &diff) {
                    // Semver violation — roll back new signatures and fail
                    // TODO: proper transaction rollback. For now, the sigs are
                    // already inserted but the index_state won't be updated,
                    // so the next push will re-index.
                    bail!("semver violation: {msg}");
                }

                tracing::info!(
                    old_version = %old_version,
                    new_version = %new_version,
                    "semver check passed"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 3: Update index state
    // -----------------------------------------------------------------------
    let version_str = new_manifest
        .as_ref()
        .map(|m| m.package.version.to_string())
        .unwrap_or_default();

    conn.execute(
        "INSERT OR REPLACE INTO index_state (repo_id, indexed_commit, version, indexed_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        [repo_id.to_string(), new_sha.to_string(), version_str],
    )
    .await
    .context("failed to update index state")?;

    tracing::info!("indexing complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// DB helpers for loading previous state
// ---------------------------------------------------------------------------

/// Load the previous version from index_state, if any.
async fn load_previous_version(
    conn: &turso::Connection,
    repo_id: &str,
) -> Option<Version> {
    let mut rows = conn
        .query(
            "SELECT version FROM index_state WHERE repo_id = ?1",
            [repo_id.to_string()],
        )
        .await
        .ok()?;

    let row = rows.next().await.ok()??;
    let version_str: String = row.get(0).ok()?;
    if version_str.is_empty() {
        return None;
    }
    Version::parse(&version_str)
}

/// Load all function signatures for a repo from the DB (the *previous* indexed state).
async fn load_previous_signatures(
    conn: &turso::Connection,
    repo_id: &str,
) -> Vec<FunctionSig> {
    load_signatures_from_db(conn, repo_id).await
}

/// Load all function signatures for a repo from the DB (current state, after inserts).
async fn load_current_signatures(
    conn: &turso::Connection,
    repo_id: &str,
) -> Vec<FunctionSig> {
    load_signatures_from_db(conn, repo_id).await
}

async fn load_signatures_from_db(
    conn: &turso::Connection,
    repo_id: &str,
) -> Vec<FunctionSig> {
    let mut rows = match conn
        .query(
            "SELECT function_name, type_json, description FROM function_signatures WHERE repo_id = ?1",
            [repo_id.to_string()],
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load previous signatures");
            return Vec::new();
        }
    };

    let mut sigs = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let name: String = match row.get(0) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let type_json: String = match row.get(1) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let description: String = row.get(2).unwrap_or_default();

        let ty: agentcoderepo_types::Ty = match serde_json::from_str(&type_json) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(name = %name, error = %e, "failed to deserialize type from DB");
                continue;
            }
        };

        sigs.push(FunctionSig {
            name,
            ty,
            description,
        });
    }

    sigs
}

/// Detect the implementation language from a file path.
///
/// Supports two conventions:
/// 1. `impl/{language}/...` directory structure (preferred)
/// 2. File extension fallback (`.rs` → "rust", `.py` → "python", etc.)
fn detect_language(path: &str) -> String {
    // Check for impl/ directory convention
    if let Some(rest) = path.strip_prefix("impl/") {
        if let Some(lang) = rest.split('/').next() {
            if !lang.is_empty() {
                return lang.to_string();
            }
        }
    }

    // Fallback: detect from file extension
    if let Some(ext) = path.rsplit('.').next() {
        match ext {
            "rs" => "rust",
            "py" => "python",
            "js" | "mjs" | "cjs" => "javascript",
            "ts" | "mts" | "cts" => "typescript",
            "go" => "go",
            "java" => "java",
            "kt" | "kts" => "kotlin",
            "swift" => "swift",
            "c" => "c",
            "cpp" | "cc" | "cxx" => "cpp",
            "hs" => "haskell",
            "ml" | "mli" => "ocaml",
            "ex" | "exs" => "elixir",
            "rb" => "ruby",
            "scala" => "scala",
            "zig" => "zig",
            "lua" => "lua",
            _ => "",
        }
        .to_string()
    } else {
        String::new()
    }
}

fn hex_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}
