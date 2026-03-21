use std::path::PathBuf;
use std::sync::Arc;

use agentcoderepo_llm::LlmClient;
use agentcoderepo_store::ObjectStore;

/// Unified database handle that supports both local-only and Turso Cloud sync modes.
pub enum Db {
    Local(turso::Database),
    Sync(turso::sync::Database),
}

impl Db {
    pub async fn connect(&self) -> Result<turso::Connection, turso::Error> {
        match self {
            Db::Local(db) => db.connect(),
            Db::Sync(db) => db.connect().await,
        }
    }
}

/// GitHub OAuth configuration, constructed at startup from env vars.
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub base_url: String,
    /// URL to exchange an authorization code for an access token.
    /// Defaults to `https://github.com/login/oauth/access_token`.
    pub token_url: String,
    /// URL to fetch the authenticated GitHub user profile.
    /// Defaults to `https://api.github.com/user`.
    pub userinfo_url: String,
}

impl OAuthConfig {
    /// Create a new config pointing at the real GitHub endpoints.
    pub fn new(client_id: String, client_secret: String, base_url: String) -> Self {
        Self {
            client_id,
            client_secret,
            base_url,
            token_url: "https://github.com/login/oauth/access_token".to_string(),
            userinfo_url: "https://api.github.com/user".to_string(),
        }
    }
}

/// Stripe configuration for credit purchases.
pub struct StripeConfig {
    pub secret_key: String,
    pub webhook_secret: String,
    pub base_url: String,
}

/// Shared application state, injected into axum handlers.
pub struct AppState {
    pub store: Arc<dyn ObjectStore>,
    pub llm: Arc<dyn LlmClient>,
    pub db: Db,
    pub repo_root: PathBuf,
    /// Dimensionality of embedding vectors (16 for mock, 1536 for OpenAI).
    pub embed_dim: usize,
    /// GitHub OAuth config. None if env vars not set.
    pub github_oauth: Option<OAuthConfig>,
    /// When true, test-only endpoints like `POST /sponsors` are enabled.
    /// Always false in production.
    pub testing: bool,
    /// Whether this instance is the primary writer.
    /// Non-primary instances replay write requests to the primary via fly-replay.
    pub is_primary: bool,
    /// The Fly.io machine ID of the primary instance, if known.
    /// Used in the fly-replay header to route writes.
    pub primary_machine_id: Option<String>,
    /// Stripe config for credit purchases. None if not configured.
    pub stripe: Option<StripeConfig>,
}
