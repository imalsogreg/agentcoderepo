use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use axum::Router;
use agentcoderepo_llm::mock::{MockLlm, EMBED_DIM};
use agentcoderepo_server::{AppState, router};
use agentcoderepo_server::state::{OAuthConfig, SpritesConfig, StripeConfig};
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
    state: Arc<AppState>,
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
        Self::start_inner(None, None).await
    }

    /// Start with Stripe webhook verification enabled (for testing webhooks).
    pub async fn start_with_stripe(webhook_secret: &str) -> Result<Self> {
        Self::start_inner(Some(webhook_secret.to_string()), None).await
    }

    /// Start with Sprites API configured (pointing at a wiremock server).
    pub async fn start_with_sprites(sprites_base_url: &str) -> Result<Self> {
        Self::start_inner(None, Some(sprites_base_url.to_string())).await
    }

    async fn start_inner(
        stripe_webhook_secret: Option<String>,
        sprites_base_url: Option<String>,
    ) -> Result<Self> {
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
        let local_db = turso::Builder::new_local(db_path.to_str().unwrap()).build().await?;
        let db = agentcoderepo_server::state::Db::Local(local_db);

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
            is_primary: true,
            primary_machine_id: None,
            sprites: sprites_base_url.map(|url| SpritesConfig {
                token: "test-sprites-token".to_string(),
                base_url: url,
            }),
            stripe: stripe_webhook_secret.map(|secret| StripeConfig {
                secret_key: "sk_test_not_used_in_tests".to_string(),
                webhook_secret: secret,
                base_url: base_url.clone(),
            }),
        });

        let state_clone = state.clone();
        let app: Router = router(state);
        let handle = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service()).await.ok();
        });

        Ok(Self {
            base_url,
            mock_llm,
            state: state_clone,
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

    /// Insert a repo_sprites record directly into the DB for testing eval.
    pub async fn insert_repo_sprite(
        &self,
        owner: &str,
        repo_name: &str,
        sprite_name: &str,
        checkpoint_id: &str,
    ) {
        let conn = self.state.db.connect().await.unwrap();
        let row = conn
            .query(
                "SELECT r.id FROM repos r
                 JOIN agents a ON r.owner_id = a.id
                 WHERE a.name = ?1 AND r.name = ?2",
                [owner.to_string(), repo_name.to_string()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        let repo_id: String = row.get::<String>(0).unwrap();

        conn.execute(
            "INSERT INTO repo_sprites (repo_id, sprite_name, clean_checkpoint_id, status)
             VALUES (?1, ?2, ?3, 'ready')",
            [repo_id, sprite_name.to_string(), checkpoint_id.to_string()],
        )
        .await
        .unwrap();
    }

    /// Insert a repo_versions record for testing the resolver.
    pub async fn insert_repo_version(
        &self,
        owner: &str,
        repo_name: &str,
        version: &str,
        commit_sha: &str,
    ) {
        let conn = self.state.db.connect().await.unwrap();
        let row = conn
            .query(
                "SELECT r.id FROM repos r
                 JOIN agents a ON r.owner_id = a.id
                 WHERE a.name = ?1 AND r.name = ?2",
                [owner.to_string(), repo_name.to_string()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        let repo_id: String = row.get::<String>(0).unwrap();
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT OR IGNORE INTO repo_versions (id, repo_id, version, commit_sha)
             VALUES (?1, ?2, ?3, ?4)",
            [id, repo_id, version.to_string(), commit_sha.to_string()],
        )
        .await
        .unwrap();
    }

    /// Insert a repo_dependencies record for testing the resolver.
    pub async fn insert_repo_dependency(
        &self,
        owner: &str,
        repo_name: &str,
        commit_sha: &str,
        dep_name: &str,
        dep_owner: &str,
        dep_repo: &str,
        version_req: &str,
    ) {
        let conn = self.state.db.connect().await.unwrap();
        let row = conn
            .query(
                "SELECT r.id FROM repos r
                 JOIN agents a ON r.owner_id = a.id
                 WHERE a.name = ?1 AND r.name = ?2",
                [owner.to_string(), repo_name.to_string()],
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        let repo_id: String = row.get::<String>(0).unwrap();
        let id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT OR IGNORE INTO repo_dependencies (id, repo_id, dep_name, dep_owner, dep_repo, version_req, commit_sha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            [
                id,
                repo_id,
                dep_name.to_string(),
                dep_owner.to_string(),
                dep_repo.to_string(),
                version_req.to_string(),
                commit_sha.to_string(),
            ],
        )
        .await
        .unwrap();
    }

    /// Insert a stripe_purchases record directly into the DB for testing webhooks.
    pub async fn insert_stripe_purchase(
        &self,
        id: &str,
        stripe_session_id: &str,
        sponsor_id: &str,
        agent_id: &str,
        amount: &str,
    ) {
        let conn = self.state.db.connect().await.unwrap();
        conn.execute(
            "INSERT INTO stripe_purchases (id, stripe_session_id, sponsor_id, agent_id, amount)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            [
                id.to_string(),
                stripe_session_id.to_string(),
                sponsor_id.to_string(),
                agent_id.to_string(),
                amount.to_string(),
            ],
        )
        .await
        .unwrap();
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
