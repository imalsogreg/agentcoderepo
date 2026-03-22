use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use agentcoderepo_types::semver::{Version, VersionReq};
use crate::format::{ContentNeg, Negotiated, TextFormat};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ResolveRequest {
    pub dependencies: HashMap<String, DepSpec>,
}

#[derive(Debug, Deserialize)]
pub struct DepSpec {
    pub repo: String,        // "owner/repo"
    pub version: String,     // "^1.0"
}

#[derive(Serialize)]
pub struct ResolveResponse {
    pub resolved: HashMap<String, ResolvedDep>,
    pub flake_inputs: HashMap<String, String>,
}

impl TextFormat for ResolveResponse {
    fn to_text(&self) -> String {
        let mut s = String::new();
        for (name, dep) in &self.resolved {
            let transitive = if dep.transitive { " (transitive)" } else { "" };
            s.push_str(&format!(
                "{name}: {repo} v{version} @ {sha}{transitive}\n",
                repo = dep.repo,
                version = dep.version,
                sha = &dep.commit_sha[..12.min(dep.commit_sha.len())],
            ));
        }
        if !self.flake_inputs.is_empty() {
            s.push_str("\nnix flake inputs:\n");
            for (name, url) in &self.flake_inputs {
                s.push_str(&format!("  {name} = \"{url}\"\n"));
            }
        }
        s
    }
}

#[derive(Serialize, Clone)]
pub struct ResolvedDep {
    pub repo: String,
    pub version: String,
    pub commit_sha: String,
    #[serde(default)]
    pub transitive: bool,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

struct ResolverCtx<'a> {
    conn: &'a turso::Connection,
    resolved: HashMap<String, ResolvedDep>,
    base_url: String,
}

impl<'a> ResolverCtx<'a> {
    /// Resolve a set of dependencies, walking transitive deps.
    async fn resolve(
        &mut self,
        deps: &HashMap<String, (String, VersionReq)>, // name → (owner/repo, version_req)
        transitive: bool,
    ) -> Result<(), (StatusCode, String)> {
        for (name, (repo_ref, version_req)) in deps {
            // Check if already resolved
            if let Some(existing) = self.resolved.get(name) {
                let existing_version = Version::parse(&existing.version)
                    .ok_or((StatusCode::INTERNAL_SERVER_ERROR, format!("invalid version in resolved: {}", existing.version)))?;
                if version_req.matches(&existing_version) {
                    continue; // Compatible with already-resolved version
                } else {
                    return Err((
                        StatusCode::CONFLICT,
                        format!(
                            "conflict: {name} requires {version_req} but {} already resolved",
                            existing.version
                        ),
                    ));
                }
            }

            // Parse owner/repo
            let parts: Vec<&str> = repo_ref.splitn(2, '/').collect();
            if parts.len() != 2 {
                return Err((StatusCode::BAD_REQUEST, format!("invalid repo ref: {repo_ref}")));
            }
            let (owner, repo_name) = (parts[0], parts[1]);

            // Look up repo_id
            let repo_row = self.conn
                .query(
                    "SELECT r.id FROM repos r
                     JOIN agents a ON r.owner_id = a.id
                     WHERE a.name = ?1 AND r.name = ?2",
                    [owner.to_string(), repo_name.to_string()],
                )
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                .next()
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                .ok_or((StatusCode::NOT_FOUND, format!("repo not found: {repo_ref}")))?;

            let repo_id: String = repo_row.get::<String>(0)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            // Find all non-yanked versions
            let mut version_rows = self.conn
                .query(
                    "SELECT version, commit_sha FROM repo_versions
                     WHERE repo_id = ?1 AND yanked = 0
                     ORDER BY created_at DESC",
                    [repo_id.clone()],
                )
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            // Find newest compatible version
            let mut chosen: Option<(Version, String)> = None;
            while let Some(row) = version_rows.next().await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
                let ver_str: String = row.get::<String>(0)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                let sha: String = row.get::<String>(1)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

                if let Some(ver) = Version::parse(&ver_str) {
                    if version_req.matches(&ver) {
                        match &chosen {
                            Some((best, _)) if ver > *best => {
                                chosen = Some((ver, sha));
                            }
                            None => {
                                chosen = Some((ver, sha));
                            }
                            _ => {}
                        }
                    }
                }
            }

            let (version, commit_sha) = chosen.ok_or((
                StatusCode::NOT_FOUND,
                format!("no version of {repo_ref} satisfies {version_req}"),
            ))?;

            self.resolved.insert(name.clone(), ResolvedDep {
                repo: repo_ref.clone(),
                version: version.to_string(),
                commit_sha: commit_sha.clone(),
                transitive,
            });

            // Load transitive dependencies from repo_dependencies
            let mut dep_rows = self.conn
                .query(
                    "SELECT dep_name, dep_owner, dep_repo, version_req
                     FROM repo_dependencies
                     WHERE repo_id = ?1 AND commit_sha = ?2",
                    [repo_id, commit_sha],
                )
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let mut transitive_deps: HashMap<String, (String, VersionReq)> = HashMap::new();
            while let Some(row) = dep_rows.next().await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
                let dep_name: String = row.get::<String>(0)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                let dep_owner: String = row.get::<String>(1)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                let dep_repo: String = row.get::<String>(2)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                let ver_req_str: String = row.get::<String>(3)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

                if let Some(ver_req) = VersionReq::parse(&ver_req_str) {
                    transitive_deps.insert(
                        dep_name,
                        (format!("{dep_owner}/{dep_repo}"), ver_req),
                    );
                }
            }

            if !transitive_deps.is_empty() {
                // Recursive resolution (bounded by graph size)
                Box::pin(self.resolve(&transitive_deps, true)).await?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// POST /api/resolve
pub async fn resolve_deps(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    Json(body): Json<ResolveRequest>,
) -> Result<Negotiated<ResolveResponse>, (StatusCode, String)> {
    let conn = state.db.connect().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Parse version requirements
    let mut deps: HashMap<String, (String, VersionReq)> = HashMap::new();
    for (name, spec) in &body.dependencies {
        let version_req = VersionReq::parse(&spec.version)
            .ok_or((StatusCode::BAD_REQUEST, format!("invalid version requirement: {}", spec.version)))?;
        deps.insert(name.clone(), (spec.repo.clone(), version_req));
    }

    let base_url = state.github_oauth
        .as_ref()
        .map(|o| o.base_url.clone())
        .unwrap_or_else(|| "https://agentcoderepo.fly.dev".to_string());

    let mut ctx = ResolverCtx {
        conn: &conn,
        resolved: HashMap::new(),
        base_url: base_url.clone(),
    };

    ctx.resolve(&deps, false).await?;

    // Build flake inputs
    let mut flake_inputs = HashMap::new();
    for (name, dep) in &ctx.resolved {
        let parts: Vec<&str> = dep.repo.splitn(2, '/').collect();
        if parts.len() == 2 {
            flake_inputs.insert(
                name.clone(),
                format!(
                    "git+{}/git/{}/{}?rev={}",
                    base_url, parts[0], parts[1], dep.commit_sha
                ),
            );
        }
    }

    Ok(neg.ok(ResolveResponse {
        resolved: ctx.resolved,
        flake_inputs,
    }))
}

/// POST /api/repos/{owner}/{repo}/resolve
///
/// Resolve the dependencies declared in the repo's agentcoderepo.toml.
pub async fn resolve_repo_deps(
    State(state): State<Arc<AppState>>,
    neg: ContentNeg,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
) -> Result<Negotiated<ResolveResponse>, (StatusCode, String)> {
    let conn = state.db.connect().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Look up repo and get latest commit
    let repo_row = conn
        .query(
            "SELECT r.id, COALESCE(idx.indexed_commit, '')
             FROM repos r
             JOIN agents a ON r.owner_id = a.id
             LEFT JOIN index_state idx ON idx.repo_id = r.id
             WHERE a.name = ?1 AND r.name = ?2",
            [owner.clone(), repo.clone()],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .next()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("repo not found: {owner}/{repo}")))?;

    let repo_id: String = repo_row.get::<String>(0)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let commit_sha: String = repo_row.get::<String>(1)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if commit_sha.is_empty() {
        return Err((StatusCode::NOT_FOUND, "repo has not been indexed yet".to_string()));
    }

    // Load declared dependencies
    let mut dep_rows = conn
        .query(
            "SELECT dep_name, dep_owner, dep_repo, version_req
             FROM repo_dependencies
             WHERE repo_id = ?1 AND commit_sha = ?2",
            [repo_id, commit_sha],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut deps: HashMap<String, (String, VersionReq)> = HashMap::new();
    while let Some(row) = dep_rows.next().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))? {
        let name: String = row.get::<String>(0)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let dep_owner: String = row.get::<String>(1)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let dep_repo: String = row.get::<String>(2)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let ver_str: String = row.get::<String>(3)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        if let Some(ver_req) = VersionReq::parse(&ver_str) {
            deps.insert(name, (format!("{dep_owner}/{dep_repo}"), ver_req));
        }
    }

    if deps.is_empty() {
        return Ok(neg.ok(ResolveResponse {
            resolved: HashMap::new(),
            flake_inputs: HashMap::new(),
        }));
    }

    let base_url = state.github_oauth
        .as_ref()
        .map(|o| o.base_url.clone())
        .unwrap_or_else(|| "https://agentcoderepo.fly.dev".to_string());

    let mut ctx = ResolverCtx {
        conn: &conn,
        resolved: HashMap::new(),
        base_url: base_url.clone(),
    };

    ctx.resolve(&deps, false).await?;

    let mut flake_inputs = HashMap::new();
    for (name, dep) in &ctx.resolved {
        let parts: Vec<&str> = dep.repo.splitn(2, '/').collect();
        if parts.len() == 2 {
            flake_inputs.insert(
                name.clone(),
                format!(
                    "git+{}/git/{}/{}?rev={}",
                    base_url, parts[0], parts[1], dep.commit_sha
                ),
            );
        }
    }

    Ok(neg.ok(ResolveResponse {
        resolved: ctx.resolved,
        flake_inputs,
    }))
}
