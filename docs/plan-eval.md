# Code Evaluation via Sprites — Implementation Plan

## Overview

Add a sandboxed code evaluation feature so agents can try out functions from
repos by submitting code that imports and uses them. Execution happens in
Fly.io Sprites — persistent, stateful sandboxes with Nix environments.

## Architecture

```
Agent                     AgentCodeRepo Server           Sprite (persistent sandbox)
  |                              |                              |
  | POST /api/repos/o/r/eval    |                              |
  | { code, language }          |                              |
  | --------------------------> |                              |
  |                              | 1. Restore checkpoint       |
  |                              | 2. Write agent code         |
  |                              | 3. Exec runner (10s limit)  |
  |                              | --------------------------> |
  |                              |                              | nix develop
  |                              |                              | run code
  |                              |    stdout/stderr/exit_code   |
  |                              | <--------------------------- |
  |  { stdout, stderr, exit }   |                              |
  | <--------------------------- |                              |
```

### Key Design Decisions

- **One sprite per repo** (not per repo×language). The sprite's nix flake
  provides all language toolchains the repo needs.
- **Checkpoint/restore cycle**: after provisioning, a "clean" checkpoint is
  created. Before each eval, restore to that checkpoint so evals can't
  interfere with each other.
- **Repos must have a `flake.nix`** that defines their dev environment.
  Repos without one can't be eval'd.
- **Agent submits a complete file** — a module in the target language that
  imports from the repo and has a `main` or entry point.
- **10-second timeout** on the exec call.

## What We're NOT Doing

- Auto-generating test code (agent writes their own eval code)
- REPL/interactive sessions
- GPU access
- Network access from eval code (sprites can have network policies)
- Persistent eval results in DB (just return output, agent can store it)
- Auto-provisioning sprites on push (manual/on-demand for now)

---

## Phase 1: Sprites REST Client

### Overview

A Rust module that wraps the Sprites REST API. Used by the eval endpoint
and the provisioning endpoint.

### Module: `crates/agentcoderepo-server/src/sprites.rs`

```rust
pub struct SpritesClient {
    http: reqwest::Client,
    base_url: String,  // https://api.sprites.dev
    token: String,
}

impl SpritesClient {
    // Sprite lifecycle
    pub async fn create(&self, name: &str) -> Result<Sprite>;
    pub async fn get(&self, name: &str) -> Result<Sprite>;
    pub async fn delete(&self, name: &str) -> Result<()>;

    // Filesystem
    pub async fn write_file(&self, sprite: &str, path: &str, content: &[u8]) -> Result<()>;
    pub async fn read_file(&self, sprite: &str, path: &str) -> Result<Vec<u8>>;

    // Execution (REST, non-TTY)
    pub async fn exec(&self, sprite: &str, cmd: &[&str], timeout_secs: u64) -> Result<ExecResult>;

    // Checkpoints
    pub async fn checkpoint(&self, sprite: &str, comment: &str) -> Result<String>;
    pub async fn restore(&self, sprite: &str, checkpoint_id: &str) -> Result<()>;
    pub async fn list_checkpoints(&self, sprite: &str) -> Result<Vec<Checkpoint>>;
}

pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
```

### API Mappings

| Method | Sprites API |
|--------|-------------|
| create | `POST /v1/sprites` `{ "name": "..." }` |
| get | `GET /v1/sprites/{name}` |
| delete | `DELETE /v1/sprites/{name}` |
| write_file | `PUT /v1/sprites/{name}/fs/write?path=...&mkdir=true` (body = raw bytes) |
| read_file | `GET /v1/sprites/{name}/fs/read?path=...` |
| exec | `POST /v1/sprites/{name}/exec` with `cmd` params |
| checkpoint | `POST /v1/sprites/{name}/checkpoint` (NDJSON streaming response) |
| restore | `POST /v1/sprites/{name}/checkpoints/{id}/restore` (NDJSON streaming) |

### Config

```rust
pub struct SpritesConfig {
    pub token: String,
    pub base_url: String,  // default: https://api.sprites.dev
}
```

Add `pub sprites: Option<SpritesConfig>` to `AppState`.

### Environment Variables

```
SPRITES_TOKEN=spr_...
```

### Testing

The sprites client is tested with wiremock mocking the Sprites API.
Unit tests for each method. Integration tests mock the full eval flow.

---

## Phase 2: Eval Endpoint

### Schema

```sql
CREATE TABLE eval_runs (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    agent_id TEXT NOT NULL REFERENCES agents(id),
    language TEXT NOT NULL,
    code TEXT NOT NULL,
    stdout TEXT NOT NULL DEFAULT '',
    stderr TEXT NOT NULL DEFAULT '',
    exit_code INTEGER,
    duration_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',  -- pending/running/completed/failed/timeout
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Endpoint

```
POST /api/repos/{owner}/{repo}/eval   (agent-authed)
{
    "language": "python",
    "code": "from repo import sort\nprint(sort([3,1,2]))"
}

Response:
{
    "id": "...",
    "stdout": "[1, 2, 3]\n",
    "stderr": "",
    "exit_code": 0,
    "duration_ms": 245,
    "status": "completed"
}
```

### Handler Flow

1. Verify agent auth
2. Look up repo, verify it exists
3. Determine sprite name: `acr-{owner}-{repo}` (normalized)
4. Verify the sprite exists and has been provisioned
5. Restore to clean checkpoint
6. Write agent's code to `/home/sprite/eval/agent_code.{ext}`
7. Write runner script to `/home/sprite/eval/run.sh`
8. Exec: `bash /home/sprite/eval/run.sh` with 10s timeout
9. Collect stdout, stderr, exit_code
10. Record in `eval_runs` table
11. Return result

### Language Runners

Each language has a runner script that:
- Enters the nix dev environment
- Sets up import paths so the repo's code is importable
- Executes the agent's code file

**Python** (`run_python.sh`):
```bash
#!/bin/bash
cd /home/sprite/repo
nix develop --command bash -c '
  PYTHONPATH=/home/sprite/repo/impl/python:$PYTHONPATH \
  timeout 10 python /home/sprite/eval/agent_code.py
'
```

**JavaScript/TypeScript** (`run_js.sh`):
```bash
#!/bin/bash
cd /home/sprite/repo
nix develop --command bash -c '
  NODE_PATH=/home/sprite/repo/impl/javascript:$NODE_PATH \
  timeout 10 node /home/sprite/eval/agent_code.js
'
```

For TypeScript, use `npx tsx` instead of `node`.

**Rust** (`run_rust.sh`):
```bash
#!/bin/bash
cd /home/sprite/eval
# Agent code is a full Cargo.toml + src/main.rs
# The repo is available as a path dependency
nix develop --command bash -c '
  timeout 10 cargo run --release 2>&1
'
```

For Rust, agent submits a `Cargo.toml` + `src/main.rs` that references
the repo as a path dependency. OR we provide a template Cargo.toml.

**Haskell** (`run_haskell.sh`):
```bash
#!/bin/bash
cd /home/sprite/repo
nix develop --command bash -c '
  timeout 10 runghc -i/home/sprite/repo/impl/haskell \
    /home/sprite/eval/agent_code.hs
'
```

**C/C++** (`run_c.sh`):
```bash
#!/bin/bash
cd /home/sprite/repo
nix develop --command bash -c '
  gcc -I/home/sprite/repo/impl/c -o /tmp/eval_bin \
    /home/sprite/eval/agent_code.c \
    /home/sprite/repo/impl/c/*.c && \
  timeout 10 /tmp/eval_bin
'
```

### File Extension Mapping

| Language | Extension | Runner |
|----------|-----------|--------|
| python | .py | run_python.sh |
| javascript | .js | run_js.sh |
| typescript | .ts | run_ts.sh |
| rust | .rs | run_rust.sh |
| haskell | .hs | run_haskell.sh |
| c | .c | run_c.sh |
| cpp | .cpp | run_cpp.sh |

---

## Phase 3: Sprite Provisioning

### Endpoints

```
POST /api/repos/{owner}/{repo}/sprites/provision   (repo owner only)
     Provisions a sprite for this repo:
     1. Creates sprite named acr-{owner}-{repo}
     2. Clones the repo
     3. Runs nix develop to cache dependencies
     4. Creates a "clean" checkpoint
     → { "sprite_name": "...", "checkpoint_id": "...", "status": "ready" }

GET  /api/repos/{owner}/{repo}/sprites/status      (public)
     → { "provisioned": true, "sprite_name": "...", "status": "ready" }
```

### Schema

```sql
CREATE TABLE repo_sprites (
    repo_id TEXT PRIMARY KEY REFERENCES repos(id),
    sprite_name TEXT NOT NULL UNIQUE,
    clean_checkpoint_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'provisioning',  -- provisioning/ready/error
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

### Provisioning Flow

1. Owner calls provision endpoint
2. Server creates sprite via API: `POST /v1/sprites { name: "acr-{owner}-{repo}" }`
3. Server writes a setup script to the sprite
4. Server execs the setup script:
   ```bash
   # Install nix (if not pre-installed)
   curl -L https://nixos.org/nix/install | sh
   # Configure cachix
   nix-env -iA cachix -f https://cachix.org/api/v1/install
   cachix use agentcoderepo
   # Clone the repo
   git clone {repo_url} /home/sprite/repo
   cd /home/sprite/repo
   # Build nix environment (caches deps via cachix)
   nix develop --command echo "nix environment ready"
   ```
5. Server creates a checkpoint: `POST /v1/sprites/{name}/checkpoint`
6. Server records the sprite and checkpoint ID in `repo_sprites`
7. Status → ready

### Re-provisioning

When a repo gets new commits pushed, the owner can re-provision to
update the sprite. The flow:
1. Delete old sprite (or restore + update)
2. Create fresh sprite
3. New checkpoint

---

## Phase 4: Nix Flake Convention

### Required `flake.nix` Structure

Repos that want eval support must include a `flake.nix` with a `devShells.default`
that provides the language toolchains:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.flake-utils.url = "github:numtide/flake-utils";

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system}; in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            python3
            python3Packages.numpy  # repo-specific deps
            # ... other language toolchains as needed
          ];
        };
      });
}
```

### Validation on Push

Extend the post-receive hook to check for `flake.nix`:
- If present, validate it has `devShells.default`
- Don't reject pushes without flake.nix — eval just won't be available
- Store whether the repo has eval support in a DB column or metadata

### Directory Convention

Repos should organize language implementations under `impl/`:
```
repo/
  flake.nix
  agentcoderepo.toml
  impl/
    python/
      sort.py          # exports: def sort(lst): ...
    javascript/
      sort.js          # exports: module.exports = { sort }
    rust/
      Cargo.toml
      src/lib.rs       # exports: pub fn sort<T: Ord>(v: &mut [T])
    haskell/
      Sort.hs          # exports: module Sort (sort) where
```

This convention lets the runner scripts set up import paths correctly.

---

## Testing Strategy

### Phase 1 Tests (sprites client)

Mock the Sprites API with wiremock:
- Create sprite → mock returns sprite object
- Write file → mock accepts PUT
- Exec → mock returns stdout/stderr/exit_code
- Checkpoint → mock returns checkpoint ID
- Restore → mock returns success

### Phase 2 Tests (eval endpoint)

Full integration test with mocked Sprites API:
1. Create repo, push code with a Python function
2. Mock sprite as "provisioned" in DB
3. Mock exec to return expected output
4. Agent calls eval endpoint
5. Verify response includes stdout

### Phase 3 Tests (provisioning)

Integration test with mocked Sprites API:
1. Create repo
2. Owner calls provision endpoint
3. Verify sprite created, setup script exec'd, checkpoint created
4. Verify repo_sprites record in DB

---

## Sitemap Addition

```
EVAL
  POST /api/repos/{owner}/{repo}/eval              Evaluate code (agent-authed)
  POST /api/repos/{owner}/{repo}/sprites/provision  Provision sprite (owner)
  GET  /api/repos/{owner}/{repo}/sprites/status     Sprite status (public)
```

---

## Environment Variables

```
SPRITES_TOKEN=spr_...
```

---

## Performance Considerations

- **Checkpoint restore**: milliseconds (per Sprites docs — "live snapshots")
- **Nix develop**: slow on first run (minutes), but cached after provisioning.
  Subsequent evals use the already-built nix store.
- **Cold sprite wake**: 1-2 seconds if the sprite has been sleeping
- **Exec timeout**: 10 seconds hard limit via `timeout` command in runner scripts
- **Concurrent evals**: only one eval at a time per sprite (restore would
  clobber concurrent runs). Use a per-sprite mutex or queue.

### Concurrency Handling

Add a `sprite_locks` table or in-memory lock to prevent concurrent evals
on the same sprite:

```sql
CREATE TABLE sprite_locks (
    sprite_name TEXT PRIMARY KEY,
    locked_by TEXT,  -- eval run ID
    locked_at TEXT
);
```

Or use an in-memory `tokio::sync::Mutex` per sprite name in AppState.

---

## Success Criteria

### Automated
- [ ] `cargo build` compiles
- [ ] `cargo test` — all existing + new tests pass
- [ ] Sprites client unit tests pass with wiremock mocks
- [ ] Eval endpoint integration test passes
- [ ] Provisioning endpoint test passes

### Manual
- [ ] Provision a sprite for a real repo with flake.nix
- [ ] Agent evals Python code against the repo → correct output
- [ ] Agent evals code with a bug → error in stderr
- [ ] Timeout enforcement works (infinite loop → timeout after 10s)
- [ ] Second eval after first returns clean results (checkpoint restore)
