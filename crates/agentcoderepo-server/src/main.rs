use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use agentcoderepo_server::{AppState, router};
use agentcoderepo_server::state::{Db, OAuthConfig, SpritesConfig, StripeConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present (ignored if missing)
    let _ = dotenvy::dotenv();

    // Initialize Sentry — must be done before tracing subscriber.
    // The guard must be held for the lifetime of the application.
    let _sentry_guard = sentry::init((
        std::env::var("SENTRY_DSN").ok(),
        sentry::ClientOptions {
            release: sentry::release_name!(),
            traces_sample_rate: std::env::var("SENTRY_TRACES_SAMPLE_RATE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.2),
            environment: std::env::var("SENTRY_ENVIRONMENT")
                .ok()
                .map(Into::into),
            ..Default::default()
        },
    ));

    // Set up tracing with Sentry integration.
    // The sentry-tracing layer converts tracing spans into Sentry transactions/spans,
    // and tracing events into Sentry breadcrumbs/events.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(sentry::integrations::tracing::layer())
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

    // Initialize Turso database.
    // If TURSO_REMOTE_URL + TURSO_AUTH_TOKEN are set, use an embedded replica
    // that syncs to Turso Cloud. Otherwise, use a local-only database.
    let db_path = std::env::var("AGENTCODEREPO_DB_PATH").unwrap_or_else(|_| "agentcoderepo.db".to_string());
    let db = if let (Ok(remote_url), Ok(auth_token)) = (
        std::env::var("TURSO_REMOTE_URL"),
        std::env::var("TURSO_AUTH_TOKEN"),
    ) {
        tracing::info!(%remote_url, "Turso Cloud sync enabled (embedded replica)");
        let sync_db = turso::sync::Builder::new_remote(&db_path)
            .with_remote_url(remote_url)
            .with_auth_token(auth_token)
            .build()
            .await
            .context("failed to initialize Turso embedded replica")?;
        Db::Sync(sync_db)
    } else {
        tracing::info!(%db_path, "using local database (no Turso Cloud sync)");
        let local_db = turso::Builder::new_local(&db_path)
            .build()
            .await
            .context("failed to initialize local database")?;
        Db::Local(local_db)
    };
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

    // Fly.io primary detection.
    // Primary if: not on Fly (local dev), or FLY_REGION matches PRIMARY_REGION.
    let fly_region = std::env::var("FLY_REGION").ok();
    let primary_region = std::env::var("PRIMARY_REGION").unwrap_or_else(|_| "sjc".to_string());
    let is_primary = fly_region.as_deref().map_or(true, |r| r == primary_region);
    let primary_machine_id = std::env::var("FLY_PRIMARY_MACHINE_ID").ok();

    if is_primary {
        tracing::info!(region = ?fly_region, "running as PRIMARY instance");
    } else {
        tracing::info!(region = ?fly_region, %primary_region, "running as REPLICA — writes will be replayed to primary");
    }

    let state = Arc::new(AppState {
        store: Arc::new(store),
        llm: Arc::new(llm),
        db,
        repo_root,
        embed_dim,
        github_oauth,
        testing: false,
        is_primary,
        primary_machine_id,
        stripe: match (
            std::env::var("STRIPE_SECRET_KEY"),
            std::env::var("STRIPE_WEBHOOK_SECRET"),
        ) {
            (Ok(secret_key), Ok(webhook_secret)) => {
                let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
                tracing::info!("Stripe configured");
                Some(StripeConfig { secret_key, webhook_secret, base_url })
            }
            _ => {
                tracing::warn!("Stripe not configured (set STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET)");
                None
            }
        },
        sprites: std::env::var("SPRITES_TOKEN").ok().map(|token| {
            let base_url = std::env::var("SPRITES_BASE_URL")
                .unwrap_or_else(|_| "https://api.sprites.dev".to_string());
            tracing::info!("Sprites configured");
            SpritesConfig { token, base_url }
        }),
    });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router(state)).await?;

    Ok(())
}
