use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use axum::Router;
use agentcoderepo_llm::mock::{MockLlm, EMBED_DIM};
use agentcoderepo_server::{AppState, router};
use agentcoderepo_server::state::OAuthConfig;
use agentcoderepo_store::mem::MemStore;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::TestAgent;
use crate::sponsor::TestSponsorSession;

/// Counter for generating unique GitHub IDs across tests.
static GITHUB_ID_COUNTER: AtomicU64 = AtomicU64::new(100_000);

/// Spins up a test server with in-memory storage and a mock LLM.
pub struct TestHarness {
    pub base_url: String,
    pub mock_llm: MockLlm,
    mock_github: MockServer,
    _handle: tokio::task::JoinHandle<()>,
    _repo_dir: tempfile::TempDir,
}

impl TestHarness {
    /// Start a test server on a random available port with mock OAuth.
    ///
    /// Initializes tracing on first call (subsequent calls are no-ops).
    /// Set RUST_LOG to control verbosity, e.g.:
    ///   RUST_LOG=agentcoderepo_git=debug cargo test -- --nocapture
    pub async fn start() -> Result<Self> {
        // init is idempotent — only the first call takes effect
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();

        let mock_llm = MockLlm::new();
        let repo_dir = tempfile::tempdir()?;
        let mock_github = MockServer::start().await;

        // Use a temp file for the DB to ensure all connections share the same data.
        let db_path = repo_dir.path().join("test.db");
        let db = turso::Builder::new_local(db_path.to_str().unwrap()).build().await?;

        // Initialize database schema
        agentcoderepo_server::db::init_schema(&db, EMBED_DIM).await?;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{addr}");

        let github_oauth = Some(OAuthConfig {
            client_id: "test-client-id".to_string(),
            client_secret: "test-client-secret".to_string(),
            base_url: base_url.clone(),
            token_url: format!("{}/login/oauth/access_token", mock_github.uri()),
            userinfo_url: format!("{}/user", mock_github.uri()),
        });

        let state = Arc::new(AppState {
            store: Arc::new(MemStore::new()),
            llm: Arc::new(mock_llm.clone()),
            db,
            repo_root: repo_dir.path().to_path_buf(),
            embed_dim: EMBED_DIM,
            testing: true,
            github_oauth,
        });

        let app: Router = router(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.ok();
        });

        Ok(Self {
            base_url,
            mock_llm,
            mock_github,
            _handle: handle,
            _repo_dir: repo_dir,
        })
    }

    /// Create a new test agent pointed at this server (not yet registered).
    pub fn agent(&self) -> TestAgent {
        TestAgent::new(&self.base_url)
    }

    /// Create a fully registered test agent in one call.
    ///
    /// This simulates the complete production flow:
    /// 1. A sponsor logs in via GitHub OAuth
    /// 2. The sponsor registers the agent with its public key
    /// 3. The agent is ready to make authenticated API calls
    pub async fn registered_agent(&self) -> Result<TestAgent> {
        let mut agent = self.agent();
        let sponsor = self.login_github(&agent.sponsor_name, GITHUB_ID_COUNTER.fetch_add(1, Ordering::Relaxed)).await?;
        sponsor.register_agent(&mut agent).await?;
        Ok(agent)
    }

    /// Simulate a human logging in via GitHub OAuth.
    ///
    /// Sets up wiremock expectations for the given GitHub user, then drives
    /// the callback flow to obtain a session cookie. Returns a
    /// `TestSponsorSession` that can register agents and make session-authed
    /// requests.
    pub async fn login_github(
        &self,
        github_login: &str,
        github_id: u64,
    ) -> Result<TestSponsorSession> {
        // Use scoped mocks so they're removed after the callback completes.
        // This prevents stale mocks from interfering when login_github is
        // called multiple times in one test with different users.

        // Mock: POST /login/oauth/access_token → returns a fake token
        let _token_guard = Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": format!("gho_test_{github_login}"),
                    "token_type": "bearer",
                    "scope": "read:user",
                })),
            )
            .mount_as_scoped(&self.mock_github)
            .await;

        // Mock: GET /user → returns the fake GitHub profile.
        let _user_guard = Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": github_id,
                    "login": github_login,
                    "avatar_url": format!("https://avatars.example.com/{github_login}"),
                })),
            )
            .mount_as_scoped(&self.mock_github)
            .await;

        let session = TestSponsorSession::from_callback(&self.base_url, github_login.to_string()).await?;

        // Guards are dropped here — mocks are removed from the server
        drop(_token_guard);
        drop(_user_guard);

        Ok(session)
    }
}
