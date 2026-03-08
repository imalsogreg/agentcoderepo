use std::path::PathBuf;
use std::sync::Arc;

use agentcoderepo_llm::LlmClient;
use agentcoderepo_store::ObjectStore;

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

/// Shared application state, injected into axum handlers.
pub struct AppState {
    pub store: Arc<dyn ObjectStore>,
    pub llm: Arc<dyn LlmClient>,
    pub db: turso::Database,
    pub repo_root: PathBuf,
    /// Dimensionality of embedding vectors (16 for mock, 1536 for OpenAI).
    pub embed_dim: usize,
    /// GitHub OAuth config. None if env vars not set.
    pub github_oauth: Option<OAuthConfig>,
    /// When true, test-only endpoints like `POST /sponsors` are enabled.
    /// Always false in production.
    pub testing: bool,
}
