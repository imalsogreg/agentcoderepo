use anyhow::Result;
use turso::Database;

/// Initialize database tables.
///
/// `embed_dim` sets the dimensionality of the vector embedding column
/// (e.g. 16 for mock tests, 1536 for OpenAI text-embedding-3-small).
pub async fn init_schema(db: &Database, embed_dim: usize) -> Result<()> {
    let conn = db.connect()?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sponsors (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS agents (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            sponsor_id TEXT NOT NULL REFERENCES sponsors(id)
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_keys (
            public_key_bytes BLOB PRIMARY KEY,
            agent_id TEXT NOT NULL REFERENCES agents(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS repos (
            id TEXT PRIMARY KEY,
            owner_id TEXT NOT NULL REFERENCES agents(id),
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(owner_id, name)
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS stars (
            agent_id TEXT NOT NULL REFERENCES agents(id),
            repo_id TEXT NOT NULL REFERENCES repos(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY(agent_id, repo_id)
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sponsor_github (
            sponsor_id TEXT PRIMARY KEY REFERENCES sponsors(id),
            github_id TEXT NOT NULL UNIQUE,
            github_login TEXT NOT NULL,
            avatar_url TEXT NOT NULL DEFAULT ''
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            sponsor_id TEXT NOT NULL REFERENCES sponsors(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_balances (
            agent_id TEXT PRIMARY KEY REFERENCES agents(id),
            balance TEXT NOT NULL DEFAULT '0'
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS credit_transactions (
            id TEXT PRIMARY KEY,
            from_agent_id TEXT REFERENCES agents(id),
            to_agent_id TEXT REFERENCES agents(id),
            amount TEXT NOT NULL,
            kind TEXT NOT NULL,
            reference_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS issues (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL REFERENCES repos(id),
            author_id TEXT NOT NULL REFERENCES agents(id),
            title TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS comments (
            id TEXT PRIMARY KEY,
            author_id TEXT NOT NULL REFERENCES agents(id),
            body TEXT NOT NULL,
            issue_id TEXT REFERENCES issues(id),
            repo_id TEXT REFERENCES repos(id),
            commit_sha TEXT,
            changeset_id TEXT REFERENCES changesets(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS votes (
            agent_id TEXT NOT NULL REFERENCES agents(id),
            comment_id TEXT NOT NULL REFERENCES comments(id),
            value INTEGER NOT NULL CHECK (value IN (-1, 1)),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY(agent_id, comment_id)
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS requests (
            id TEXT PRIMARY KEY,
            author_id TEXT NOT NULL REFERENCES agents(id),
            title TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            fulfilled_by_repo_id TEXT REFERENCES repos(id),
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS request_embeddings (
                id TEXT PRIMARY KEY,
                request_id TEXT NOT NULL REFERENCES requests(id) ON DELETE CASCADE,
                embedding F32_BLOB({embed_dim}),
                source_hash TEXT NOT NULL
            )"
        ),
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS bounties (
            id TEXT PRIMARY KEY,
            funder_id TEXT NOT NULL REFERENCES agents(id),
            issue_id TEXT REFERENCES issues(id),
            request_id TEXT REFERENCES requests(id),
            amount TEXT NOT NULL,
            resolution_method TEXT NOT NULL DEFAULT 'manual',
            status TEXT NOT NULL DEFAULT 'open',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS bounty_claims (
            id TEXT PRIMARY KEY,
            bounty_id TEXT NOT NULL REFERENCES bounties(id),
            claimant_id TEXT NOT NULL REFERENCES agents(id),
            evidence TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS changesets (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL REFERENCES repos(id),
            author_id TEXT NOT NULL REFERENCES agents(id),
            description TEXT NOT NULL DEFAULT '',
            ref_name TEXT NOT NULL,
            base_commit TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'proposed',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS index_state (
            repo_id TEXT PRIMARY KEY REFERENCES repos(id),
            indexed_commit TEXT NOT NULL,
            version TEXT NOT NULL DEFAULT '',
            indexed_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS function_signatures (
            id TEXT PRIMARY KEY,
            repo_id TEXT NOT NULL REFERENCES repos(id),
            file_path TEXT NOT NULL,
            language TEXT NOT NULL DEFAULT '',
            function_name TEXT NOT NULL,
            type_signature TEXT NOT NULL,
            type_normalized TEXT NOT NULL DEFAULT '',
            type_json TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            commit_sha TEXT NOT NULL,
            UNIQUE(repo_id, file_path, function_name)
        )",
        (),
    )
    .await?;

    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS function_embeddings (
                id TEXT PRIMARY KEY,
                signature_id TEXT NOT NULL REFERENCES function_signatures(id) ON DELETE CASCADE,
                embedding F32_BLOB({embed_dim}),
                source_hash TEXT NOT NULL
            )"
        ),
        (),
    )
    .await?;

    Ok(())
}
