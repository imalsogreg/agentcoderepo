use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use agentcoderepo_server::{AppState, router};
use agentcoderepo_server::state::OAuthConfig;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (ignored if missing)
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let repo_root = std::env::var("AGENTCODEREPO_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/data/repos"));

    let embed_dim: usize = std::env::var("AGENTCODEREPO_EMBED_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1536);

    // Initialize OpenAI client
    let llm = agentcoderepo_llm::openai::OpenAiClient::from_env()
        .context("failed to initialize OpenAI client (is OPENAI_API_KEY set?)")?;

    // Initialize Turso database
    let db_url = std::env::var("AGENTCODEREPO_DB_PATH").unwrap_or_else(|_| "agentcoderepo.db".to_string());
    let db = turso::Builder::new_local(&db_url)
        .build()
        .await
        .context("failed to initialize database")?;
    agentcoderepo_server::db::init_schema(&db, embed_dim).await?;

    // Initialize S3/Tigris object store
    let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3_client = aws_sdk_s3::Client::new(&aws_config);
    let bucket = std::env::var("AGENTCODEREPO_S3_BUCKET").unwrap_or_else(|_| "agentcoderepo-repos".to_string());
    let store = agentcoderepo_store::s3::S3Store::new(s3_client, bucket);

    // GitHub OAuth (optional)
    let github_oauth = match (
        std::env::var("GITHUB_CLIENT_ID"),
        std::env::var("GITHUB_CLIENT_SECRET"),
        std::env::var("BASE_URL"),
    ) {
        (Ok(client_id), Ok(client_secret), Ok(base_url)) => {
            tracing::info!("GitHub OAuth configured");
            Some(OAuthConfig::new(client_id, client_secret, base_url))
        }
        _ => {
            tracing::warn!("GitHub OAuth not configured (set GITHUB_CLIENT_ID, GITHUB_CLIENT_SECRET, BASE_URL)");
            None
        }
    };

    tokio::fs::create_dir_all(&repo_root).await?;

    let state = Arc::new(AppState {
        store: Arc::new(store),
        llm: Arc::new(llm),
        db,
        repo_root,
        embed_dim,
        github_oauth,
        testing: false,
    });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router(state)).await?;

    Ok(())
}
