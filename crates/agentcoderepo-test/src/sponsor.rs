use anyhow::Result;

/// A logged-in human sponsor session, authenticated via (mock) GitHub OAuth.
pub struct TestSponsorSession {
    pub sponsor_id: String,
    pub sponsor_name: String,
    pub session_token: String,
    pub client: reqwest::Client,
    pub base_url: String,
}

impl TestSponsorSession {
    /// Perform the OAuth callback flow against the test server with a mock code,
    /// then look up the sponsor identity via the session.
    pub(crate) async fn from_callback(
        base_url: &str,
        github_login: String,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        // Hit the callback endpoint directly with a fake code.
        // The wiremock GitHub mock will accept any code.
        let resp = client
            .get(format!("{base_url}/auth/github/callback"))
            .query(&[("code", "mock-auth-code")])
            .send()
            .await?;

        if !resp.status().is_redirection() && !resp.status().is_success() {
            anyhow::bail!(
                "OAuth callback failed with status {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }

        // Extract session cookie from Set-Cookie header
        let set_cookie = resp
            .headers()
            .get("set-cookie")
            .ok_or_else(|| anyhow::anyhow!("no Set-Cookie header in callback response"))?
            .to_str()?;

        let session_token = set_cookie
            .split(';')
            .next()
            .and_then(|s| s.strip_prefix("session="))
            .ok_or_else(|| anyhow::anyhow!("could not parse session token from: {set_cookie}"))?
            .to_string();

        // Look up sponsor identity via the session
        let me_resp = client
            .get(format!("{base_url}/api/sponsor/me"))
            .header("Cookie", format!("session={session_token}"))
            .send()
            .await?;

        if !me_resp.status().is_success() {
            anyhow::bail!("/api/sponsor/me failed: {}", me_resp.status());
        }

        let me: serde_json::Value = me_resp.json().await?;
        let sponsor_id = me["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing id in /api/sponsor/me response"))?
            .to_string();

        Ok(Self {
            sponsor_id,
            sponsor_name: github_login,
            session_token,
            client: reqwest::Client::new(),
            base_url: base_url.to_string(),
        })
    }

    /// Register an agent under this sponsor's account.
    pub async fn register_agent(&self, agent: &mut crate::TestAgent) -> Result<()> {
        use base64::Engine;
        let pk_b64 = base64::engine::general_purpose::STANDARD
            .encode(agent.verifying_key.as_bytes());

        let resp = self
            .client
            .post(format!(
                "{}/sponsors/{}/agents",
                self.base_url, self.sponsor_id
            ))
            .header("Cookie", format!("session={}", self.session_token))
            .json(&serde_json::json!({
                "name": agent.name,
                "public_key_base64": pk_b64,
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("register_agent failed: {}", resp.status());
        }

        let body: serde_json::Value = resp.json().await?;
        agent.id = body["id"].as_str().unwrap().parse()?;
        agent.sponsor_id = Some(self.sponsor_id.clone());
        agent.sponsor_name = self.sponsor_name.clone();
        Ok(())
    }

    /// Make a GET request with the session cookie.
    pub async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Cookie", format!("session={}", self.session_token))
            .send()
            .await?;
        Ok(resp)
    }

    /// Make a POST request with a JSON body and the session cookie.
    pub async fn post(&self, path: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("Cookie", format!("session={}", self.session_token))
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }
}
