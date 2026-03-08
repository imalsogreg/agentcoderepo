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
