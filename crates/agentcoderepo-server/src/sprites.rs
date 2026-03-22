use serde::{Deserialize, Serialize};

/// REST client for the Fly.io Sprites API.
pub struct SpritesClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Sprite {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub create_time: Option<String>,
}

impl SpritesClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    // -----------------------------------------------------------------------
    // Sprite lifecycle
    // -----------------------------------------------------------------------

    /// Create a new sprite.
    pub async fn create(&self, name: &str) -> Result<Sprite, SpritesError> {
        let resp = self
            .http
            .post(format!("{}/v1/sprites", self.base_url))
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        resp.json().await.map_err(SpritesError::Http)
    }

    /// Get sprite details.
    pub async fn get(&self, name: &str) -> Result<Sprite, SpritesError> {
        let resp = self
            .http
            .get(format!("{}/v1/sprites/{name}", self.base_url))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        resp.json().await.map_err(SpritesError::Http)
    }

    /// Delete a sprite.
    pub async fn delete(&self, name: &str) -> Result<(), SpritesError> {
        let resp = self
            .http
            .delete(format!("{}/v1/sprites/{name}", self.base_url))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Filesystem
    // -----------------------------------------------------------------------

    /// Write a file to the sprite's filesystem.
    pub async fn write_file(
        &self,
        sprite: &str,
        path: &str,
        content: &[u8],
    ) -> Result<(), SpritesError> {
        let resp = self
            .http
            .put(format!(
                "{}/v1/sprites/{sprite}/fs/write?path={path}&mkdir=true",
                self.base_url
            ))
            .header("Authorization", self.auth_header())
            .body(content.to_vec())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        Ok(())
    }

    /// Read a file from the sprite's filesystem.
    pub async fn read_file(&self, sprite: &str, path: &str) -> Result<Vec<u8>, SpritesError> {
        let resp = self
            .http
            .get(format!(
                "{}/v1/sprites/{sprite}/fs/read?path={path}",
                self.base_url
            ))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(SpritesError::Http)
    }

    // -----------------------------------------------------------------------
    // Execution (REST, non-TTY)
    // -----------------------------------------------------------------------

    /// Execute a command on the sprite and return the result.
    ///
    /// Uses the REST exec endpoint (non-TTY). The `cmd` slice contains
    /// the command and its arguments.
    pub async fn exec(
        &self,
        sprite: &str,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<ExecResult, SpritesError> {
        let mut url = format!("{}/v1/sprites/{sprite}/exec?tty=false", self.base_url);

        for c in cmd {
            url.push_str(&format!("&cmd={}", urlencoding_encode(c)));
        }
        for (k, v) in env {
            url.push_str(&format!("&env={}={}", k, urlencoding_encode(v)));
        }

        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        // The REST exec endpoint returns NDJSON events.
        // We need to collect stdout/stderr and the exit code.
        let body = resp.text().await.map_err(SpritesError::Http)?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = -1i32;

        for line in body.lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                match event["type"].as_str() {
                    Some("exit") => {
                        exit_code = event["exit_code"].as_i64().unwrap_or(-1) as i32;
                    }
                    Some("stdout") => {
                        if let Some(data) = event["data"].as_str() {
                            stdout.push_str(data);
                        }
                    }
                    Some("stderr") => {
                        if let Some(data) = event["data"].as_str() {
                            stderr.push_str(data);
                        }
                    }
                    _ => {
                        // The response may also be raw output (not JSON)
                        // In that case, treat the whole body as stdout
                    }
                }
            } else {
                // Non-JSON output — likely raw stdout
                stdout.push_str(line);
                stdout.push('\n');
            }
        }

        Ok(ExecResult {
            stdout,
            stderr,
            exit_code,
        })
    }

    // -----------------------------------------------------------------------
    // Checkpoints
    // -----------------------------------------------------------------------

    /// Create a checkpoint and return its ID.
    pub async fn checkpoint(
        &self,
        sprite: &str,
        comment: &str,
    ) -> Result<String, SpritesError> {
        let resp = self
            .http
            .post(format!(
                "{}/v1/sprites/{sprite}/checkpoint",
                self.base_url
            ))
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "comment": comment }))
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        // Response is NDJSON. The "complete" event contains the checkpoint info.
        let body = resp.text().await.map_err(SpritesError::Http)?;
        for line in body.lines() {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                if event["type"].as_str() == Some("complete") {
                    // Extract checkpoint ID from the "data" field
                    // Format: "Checkpoint v8 created"
                    if let Some(data) = event["data"].as_str() {
                        if let Some(id) = data.strip_prefix("Checkpoint ").and_then(|s| s.strip_suffix(" created")) {
                            return Ok(id.to_string());
                        }
                    }
                }
            }
        }

        Err(SpritesError::Api {
            status: 200,
            body: "checkpoint created but could not parse ID".to_string(),
        })
    }

    /// Restore a sprite to a checkpoint.
    pub async fn restore(
        &self,
        sprite: &str,
        checkpoint_id: &str,
    ) -> Result<(), SpritesError> {
        let resp = self
            .http
            .post(format!(
                "{}/v1/sprites/{sprite}/checkpoints/{checkpoint_id}/restore",
                self.base_url
            ))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        // Consume the NDJSON stream — wait for "complete"
        let body = resp.text().await.map_err(SpritesError::Http)?;
        for line in body.lines() {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                if event["type"].as_str() == Some("error") {
                    return Err(SpritesError::Api {
                        status: 500,
                        body: event["data"].as_str().unwrap_or("restore failed").to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// List checkpoints for a sprite.
    pub async fn list_checkpoints(
        &self,
        sprite: &str,
    ) -> Result<Vec<Checkpoint>, SpritesError> {
        let resp = self
            .http
            .get(format!(
                "{}/v1/sprites/{sprite}/checkpoints",
                self.base_url
            ))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(SpritesError::Http)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SpritesError::Api { status, body });
        }

        resp.json().await.map_err(SpritesError::Http)
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SpritesError {
    Http(reqwest::Error),
    Api { status: u16, body: String },
}

impl std::fmt::Display for SpritesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpritesError::Http(e) => write!(f, "HTTP error: {e}"),
            SpritesError::Api { status, body } => {
                write!(f, "Sprites API error {status}: {body}")
            }
        }
    }
}

impl std::error::Error for SpritesError {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple percent-encoding for URL query parameters.
fn urlencoding_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{b:02X}"));
            }
        }
    }
    result
}
