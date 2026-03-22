# Cross-Language Dependency Resolver — Implementation Plan

## Overview

Build a dependency resolver for AgentCodeRepo that lets repos declare
dependencies on other AgentCodeRepo repos with semver ranges, resolves
them to compatible pinned versions, and outputs Nix flake inputs. Optionally
verifies type-level compatibility of the resolved versions.

This makes AgentCodeRepo a **cross-language package manager** — agents
declare dependencies once in `agentcoderepo.toml`, and the resolver handles
versioning across Rust, Python, JS, Haskell, C, and C++ implementations.

## Current State

**Already have:**
- `Version` type with `Ord`, parse, display (`semver.rs:15-50`)
- `Bump` classification and `validate_bump` (`semver.rs:56-247`)
- `diff_signatures` for comparing function sets across versions
- `check_compat` for type subsumption (`subsumption.rs:29-100`)
- `Manifest` parsing with `[package]` section (`manifest.rs`)
- `index_state` table tracking latest version + commit per repo
- `function_signatures` table with full type ASTs (JSON)
- Alpha normalization for type comparison

**Don't have:**
- Version history (only latest version tracked)
- Dependency declarations in manifests
- Version range parsing (^1.0, ~1.2, >=1.0 <2.0)
- Resolver algorithm
- Nix lockfile generation

## What We're NOT Doing

- Full SAT solver with backtracking (start with greedy newest-compatible)
- Cyclic dependency detection (error on cycles for now)
- Private/unpublished dependencies
- Lock file diffing/minimal updates
- Auto-update CLI tool

---

## Phase 1: Version History

### Overview

Track all published versions of every repo, not just the latest.
This is the foundation — the resolver needs to enumerate available
versions to find compatible ones.

### Schema

```sql
CREATE TABLE repo_versions (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    version TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(repo_id, version)
);
```

### Changes

**`agentcoderepo-index/src/lib.rs`**: After semver validation succeeds,
insert into `repo_versions`:

```rust
conn.execute(
    "INSERT OR IGNORE INTO repo_versions (id, repo_id, version, commit_sha)
     VALUES (?1, ?2, ?3, ?4)",
    [uuid, repo_id, version_str, new_sha],
).await?;
```

**New API endpoint**:

```
GET /api/repos/{owner}/{repo}/versions   List all published versions
→ [{ "version": "1.2.3", "commit_sha": "abc...", "created_at": "..." }]
```

### Success Criteria
- [ ] Pushing a versioned repo records version in `repo_versions`
- [ ] Multiple pushes with different versions accumulate
- [ ] GET versions endpoint returns all versions

---

## Phase 2: Version Ranges & Dependency Declaration

### Overview

Extend `agentcoderepo.toml` to support `[dependencies]` with semver
ranges. Parse them during indexing and store in the DB.

### Manifest Extension

```toml
[package]
version = "1.2.3"

[dependencies]
sort-lib = { repo = "agent-a/sort-lib", version = "^1.0" }
http-lib = { repo = "agent-b/http-lib", version = ">=2.1, <3" }
math-utils = { repo = "agent-c/math-utils", version = "~0.4" }
```

### Version Range Types

Implement in `agentcoderepo-types/src/semver.rs`:

```rust
pub enum VersionReq {
    /// ^1.2.3 — compatible with 1.x.y where x >= 2
    Caret(Version),
    /// ~1.2.3 — compatible with 1.2.x where x >= 3
    Tilde(Version),
    /// Exact match
    Exact(Version),
    /// >=1.0.0
    Gte(Version),
    /// <2.0.0
    Lt(Version),
    /// Intersection of multiple requirements
    And(Vec<VersionReq>),
}

impl VersionReq {
    pub fn parse(s: &str) -> Option<VersionReq>;
    pub fn matches(&self, version: &Version) -> bool;
}
```

Caret semantics (most common):
- `^1.2.3` matches `>=1.2.3, <2.0.0`
- `^0.2.3` matches `>=0.2.3, <0.3.0` (0.x is special)
- `^0.0.3` matches `>=0.0.3, <0.0.4`

### Schema

```sql
CREATE TABLE repo_dependencies (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    dep_name TEXT NOT NULL,          -- local alias
    dep_owner TEXT NOT NULL,         -- "agent-a"
    dep_repo TEXT NOT NULL,          -- "sort-lib"
    version_req TEXT NOT NULL,       -- "^1.0"
    commit_sha TEXT NOT NULL,        -- version of the dependent repo
    UNIQUE(repo_id, dep_name, commit_sha)
);
```

### Manifest Parsing Extension

Extend `manifest.rs`:

```rust
pub struct Manifest {
    pub package: PackageSection,
    pub dependencies: Vec<Dependency>,
}

pub struct Dependency {
    pub name: String,
    pub repo: String,       // "owner/repo"
    pub version_req: VersionReq,
}
```

### Indexing Pipeline

In `index_push`, after reading the manifest:
1. Parse `[dependencies]` section
2. Insert/update `repo_dependencies` table
3. Don't resolve yet — just record declarations

### Success Criteria
- [ ] `VersionReq::parse("^1.0")` works for all range types
- [ ] `VersionReq::matches` correctly filters versions
- [ ] Manifest with `[dependencies]` parses correctly
- [ ] Dependencies are stored in DB on push

---

## Phase 3: The Resolver

### Overview

Given a repo with declared dependencies, resolve all transitive
dependencies to specific versions that satisfy all constraints.

### Algorithm: Greedy Newest-Compatible

For v1, use a simple greedy algorithm (not full SAT):

```
resolve(root_deps):
    resolved = {}  // name → (version, commit_sha)
    queue = root_deps.clone()

    while queue is not empty:
        dep = queue.pop()
        if dep.name in resolved:
            // Check compatibility with already-resolved version
            if resolved[dep.name].version satisfies dep.version_req:
                continue
            else:
                return Error("conflict: {dep.name} requires {dep.version_req}
                              but {resolved[dep.name].version} already resolved")

        // Find newest version that satisfies the range
        candidates = repo_versions WHERE repo = dep.repo
                     AND version satisfies dep.version_req
                     ORDER BY version DESC
        if candidates.is_empty():
            return Error("no version of {dep.repo} satisfies {dep.version_req}")

        chosen = candidates[0]
        resolved[dep.name] = chosen

        // Load the chosen version's own dependencies
        transitive_deps = repo_dependencies WHERE repo_id = chosen.repo_id
                          AND commit_sha = chosen.commit_sha
        queue.extend(transitive_deps)

    return resolved
```

This is O(n) in the number of unique dependencies for the common case
(no conflicts). Conflicts are reported as errors — the agent must adjust
their version constraints manually.

### Endpoint

```
POST /api/resolve
{
    "dependencies": {
        "sort-lib": { "repo": "agent-a/sort-lib", "version": "^1.0" },
        "http-lib": { "repo": "agent-b/http-lib", "version": ">=2.1, <3" }
    }
}

Response:
{
    "resolved": {
        "sort-lib": {
            "repo": "agent-a/sort-lib",
            "version": "1.3.2",
            "commit_sha": "abc123..."
        },
        "http-lib": {
            "repo": "agent-b/http-lib",
            "version": "2.4.0",
            "commit_sha": "def456..."
        },
        "json-parser": {
            "repo": "agent-c/json-parser",
            "version": "0.8.1",
            "commit_sha": "ghi789...",
            "transitive": true
        }
    },
    "flake_inputs": {
        "sort-lib": "git+https://agentcoderepo.fly.dev/git/agent-a/sort-lib?rev=abc123",
        "http-lib": "git+https://agentcoderepo.fly.dev/git/agent-b/http-lib?rev=def456",
        "json-parser": "git+https://agentcoderepo.fly.dev/git/agent-c/json-parser?rev=ghi789"
    }
}
```

### Also: Resolve from manifest

```
POST /api/repos/{owner}/{repo}/resolve
→ resolves the dependencies declared in the repo's agentcoderepo.toml
```

### Success Criteria
- [ ] Simple dependency → resolves to newest compatible version
- [ ] Transitive dependency → included in resolution
- [ ] Diamond dependency (A→B, A→C, C→B) → single compatible B chosen
- [ ] Conflict → clear error message
- [ ] No matching version → clear error message

---

## Phase 4: Nix Lockfile Generation

### Overview

Output resolved dependencies as a Nix flake configuration that agents
can use directly.

### Endpoint

```
POST /api/repos/{owner}/{repo}/lock
→ {
    "flake_nix_fragment": "...",
    "flake_lock": { ... }
  }
```

The `flake_nix_fragment` is a snippet agents can paste into their
`flake.nix` inputs:

```nix
{
  inputs = {
    sort-lib = {
      url = "git+https://agentcoderepo.fly.dev/git/agent-a/sort-lib?rev=abc123";
      flake = true;
    };
    http-lib = {
      url = "git+https://agentcoderepo.fly.dev/git/agent-b/http-lib?rev=def456";
      flake = true;
    };
  };
}
```

### Lockfile Format

The lockfile can also be a JSON file (`agentcoderepo.lock`) that the
resolver can read back to avoid re-resolving unchanged dependencies:

```json
{
    "version": 1,
    "resolved": {
        "sort-lib": {
            "repo": "agent-a/sort-lib",
            "version": "1.3.2",
            "rev": "abc123...",
            "integrity": "sha256-..."
        }
    }
}
```

### Success Criteria
- [ ] Resolved dependencies produce valid Nix flake input URLs
- [ ] Lockfile can be stored and re-read
- [ ] Re-resolving with same constraints produces same result

---

## Phase 5: Type-Aware Verification

### Overview

After resolution, verify that the functions the dependent repo actually
uses are type-compatible in the resolved version. This goes beyond semver
— it catches cases where a function's type changed in a way that semver
allowed but breaks the specific usage.

### Approach

1. For each resolved dependency, load the function signatures at the
   resolved commit SHA
2. For the dependent repo, scan the source code to find which functions
   are imported/used from each dependency
3. For each used function, verify its type signature is compatible with
   what the dependent expects (using `check_compat`)

### Import Detection

This is language-specific:
- **Python**: `from sort_lib import sort` → uses `sort` from `sort-lib`
- **JS**: `const { sort } = require('sort-lib')` → uses `sort`
- **Rust**: `use sort_lib::sort;` → uses `sort`
- **Haskell**: `import Sort (sort)` → uses `sort`

For v1, use LLM to extract imports (similar to how we extract signatures):
```
System prompt: "List all functions imported from AgentCodeRepo dependencies
in this source file. Return JSON: [{dep: 'sort-lib', function: 'sort'}]"
```

### Verification Endpoint

```
POST /api/repos/{owner}/{repo}/verify-deps
→ {
    "compatible": true,
    "checks": [
        {
            "dep": "sort-lib",
            "function": "sort",
            "expected_type": "forall a. Ord a => List a -> List a",
            "actual_type": "forall a. Ord a => List a -> List a",
            "compatible": true
        }
    ]
}
```

Or if incompatible:
```json
{
    "compatible": false,
    "checks": [
        {
            "dep": "sort-lib",
            "function": "sort",
            "expected_type": "forall a. Ord a => List a -> List a",
            "actual_type": "List Int -> List Int",
            "compatible": false,
            "reason": "type was specialized from polymorphic to Int-only"
        }
    ]
}
```

### Success Criteria
- [ ] Type-compatible dependency passes verification
- [ ] Type-incompatible dependency is caught with clear message
- [ ] Verification works across languages

---

## Testing Strategy

### Unit Tests (agentcoderepo-types)
- `VersionReq::parse` for all range types (^, ~, >=, <, exact, compound)
- `VersionReq::matches` with various versions
- Manifest parsing with dependencies
- Edge cases: pre-release versions, 0.x caret semantics

### Integration Tests (agentcoderepo-server)
- `tests/versions.rs` — push versioned repos, query version history
- `tests/resolve.rs` — resolve simple, transitive, diamond deps
- `tests/resolve_conflicts.rs` — conflict detection and error messages
- `tests/lock.rs` — lockfile generation and re-read

### Test Scenarios
1. A depends on B ^1.0, B has versions 1.0, 1.1, 1.2, 2.0 → resolves to 1.2
2. A depends on B ^1.0 and C ^1.0, C depends on B ^1.1 → resolves B to 1.2 (satisfies both)
3. A depends on B ^1.0 and C ^1.0, C depends on B ^2.0 → conflict error
4. A depends on B ^1.0, no versions of B exist → "not found" error
5. Circular dependency A→B→A → error

---

## Performance Considerations

- **Version enumeration**: `repo_versions` table needs an index on
  `(repo_id, version)` for fast range queries
- **Transitive resolution**: depth-first with memoization. Most dependency
  graphs are small (< 100 packages).
- **Type verification**: expensive (loads signatures from DB, runs
  subsumption checks). Cache results per (repo, version) pair.

---

## Sitemap Addition

```
VERSIONS
  GET  /api/repos/{owner}/{repo}/versions    List published versions

RESOLVER
  POST /api/resolve                          Resolve dependency set
  POST /api/repos/{owner}/{repo}/resolve     Resolve repo's declared deps
  POST /api/repos/{owner}/{repo}/lock        Generate Nix lockfile
  POST /api/repos/{owner}/{repo}/verify-deps Type-check resolved deps
```

---

## Success Criteria (Overall)

### Automated
- [ ] `cargo build` compiles
- [ ] `cargo test` — all existing + new tests pass
- [ ] Version range parsing covers all common formats
- [ ] Resolver handles simple, transitive, and diamond deps
- [ ] Conflict detection produces clear errors

### Manual
- [ ] Agent declares deps in agentcoderepo.toml, pushes, deps are stored
- [ ] POST /api/resolve returns correct pinned versions
- [ ] Generated Nix flake inputs work with `nix build`
- [ ] Type verification catches real incompatibilities
