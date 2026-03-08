use anyhow::Result;
use base64::Engine;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use uuid::Uuid;

/// A test agent with a generated identity and signing key.
pub struct TestAgent {
    pub id: Uuid,
    pub name: String,
    pub sponsor_id: Option<String>,
    pub sponsor_name: String,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
    pub client: reqwest::Client,
    pub base_url: String,
}

impl TestAgent {
    /// Create a new test agent with a fresh keypair.
    ///
    /// The agent ID is a placeholder until registration — the real ID
    /// is assigned by the server and set by `register_agent`.
    pub fn new(base_url: &str) -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let id = Uuid::new_v4();
        let name = format!("test-agent-{}", &id.to_string()[..8]);

        Self {
            id,
            name: name.clone(),
            sponsor_id: None,
            sponsor_name: format!("sponsor-of-{name}"),
            signing_key,
            verifying_key,
            client: reqwest::Client::new(),
            base_url: base_url.to_string(),
        }
    }

    /// Register the sponsor account. Must be called before `register_agent`.
    pub async fn register_sponsor(&mut self) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/sponsors", self.base_url))
            .json(&serde_json::json!({ "name": self.sponsor_name }))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("register_sponsor failed: {}", resp.status());
        }

        let body: serde_json::Value = resp.json().await?;
        self.sponsor_id = Some(body["id"].as_str().unwrap().to_string());
        Ok(())
    }

    /// Register this agent under its sponsor. Must call `register_sponsor` first.
    pub async fn register_agent(&mut self) -> Result<()> {
        let sponsor_id = self.sponsor_id.as_ref()
            .ok_or_else(|| anyhow::anyhow!("must register sponsor first"))?;

        let pk_b64 = base64::engine::general_purpose::STANDARD
            .encode(self.verifying_key.as_bytes());

        let resp = self
            .client
            .post(format!(
                "{}/sponsors/{}/agents",
                self.base_url, sponsor_id
            ))
            .json(&serde_json::json!({
                "name": self.name,
                "public_key_base64": pk_b64,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("register_agent failed: {}", resp.status());
        }

        let body: serde_json::Value = resp.json().await?;
        self.id = body["id"].as_str().unwrap().parse()?;
        Ok(())
    }

    /// Generate a signed bearer token for this agent (raw Ed25519 signature).
    pub fn bearer_token(&self) -> String {
        agentcoderepo_server::make_bearer_token(&self.id, &self.signing_key)
    }

    /// Generate a signed bearer token using SSH signature format (2-part).
    ///
    /// This produces the same token an agent would create using:
    ///   echo -n "{timestamp}" | ssh-keygen -Y sign -f key -n agentcoderepo
    pub fn bearer_token_ssh(&self) -> String {
        use base64::Engine;
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let message = format!("{timestamp}");

        // Convert ed25519-dalek key to ssh-key types
        let ssh_private_key = ssh_key::PrivateKey::from(
            ssh_key::private::Ed25519Keypair::from_seed(self.signing_key.as_bytes()),
        );

        let ssh_sig = ssh_private_key
            .sign(agentcoderepo_server::auth::SSH_SIG_NAMESPACE, ssh_key::HashAlg::Sha512, message.as_bytes())
            .expect("SSH signing failed");

        let sig_pem = ssh_sig.to_pem(ssh_key::LineEnding::LF)
            .expect("SSH sig PEM encoding failed");
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(sig_pem.as_bytes());

        format!("{timestamp}:{sig_b64}")
    }

    /// Make an authenticated GET request to a path on the test server.
    pub async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.bearer_token()))
            .send()
            .await?;
        Ok(resp)
    }

    /// Make an authenticated POST request with a JSON body.
    pub async fn post(&self, path: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.bearer_token()))
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }

    /// Make an authenticated PATCH request with a JSON body.
    pub async fn patch(&self, path: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        let resp = self
            .client
            .patch(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.bearer_token()))
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }

    /// Make an authenticated PUT request (no body).
    pub async fn put(&self, path: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.bearer_token()))
            .send()
            .await?;
        Ok(resp)
    }

    /// Make an authenticated DELETE request.
    pub async fn delete(&self, path: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.bearer_token()))
            .send()
            .await?;
        Ok(resp)
    }
}
