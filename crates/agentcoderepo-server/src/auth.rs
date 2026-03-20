use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use uuid::Uuid;

use crate::state::AppState;

/// Maximum age of a bearer token before it's considered expired.
const TOKEN_MAX_AGE_SECS: u64 = 300; // 5 minutes

/// The namespace used for SSH signatures (`ssh-keygen -Y sign -n <namespace>`).
pub const SSH_SIG_NAMESPACE: &str = "agentcoderepo";

/// Authenticated agent identity, extracted from the `Authorization: Bearer` header.
///
/// Two token formats are supported:
///
/// 1. SSH signature (recommended — zero dependencies for agents):
///    `{timestamp}:{ssh_signature_base64}`
///    The server extracts the public key from the SSH signature and
///    looks up the agent by key in the `agent_keys` table.
///
/// 2. Legacy raw Ed25519 signature:
///    `{agent_id}:{timestamp}:{raw_signature_base64}`
///    The server looks up the agent by ID and verifies against all
///    registered keys for that agent.
#[derive(Debug, Clone)]
pub struct AuthAgent {
    pub agent_id: Uuid,
    pub agent_name: String,
    pub sponsor_name: String,
}

/// Parsed bearer token — either the new 2-part SSH format or the legacy 3-part format.
enum ParsedToken {
    /// `{timestamp}:{ssh_sig_base64}` — agent identity is in the signature.
    Ssh {
        timestamp: u64,
        ssh_sig: ssh_key::SshSig,
    },
    /// `{agent_id}:{timestamp}:{raw_sig_base64}` — legacy format.
    Legacy {
        agent_id: Uuid,
        timestamp: u64,
        signature: Signature,
    },
}

impl FromRequestParts<Arc<AppState>> for AuthAgent {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let token = header
            .strip_prefix("Bearer ")
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let parsed = parse_token(token)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;

        // Check token freshness
        let timestamp = match &parsed {
            ParsedToken::Ssh { timestamp, .. } => *timestamp,
            ParsedToken::Legacy { timestamp, .. } => *timestamp,
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now.abs_diff(timestamp) > TOKEN_MAX_AGE_SECS {
            tracing::warn!("token expired");
            return Err(StatusCode::UNAUTHORIZED);
        }

        let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        match parsed {
            ParsedToken::Ssh { timestamp, ssh_sig } => {
                // Extract public key from the SSH signature envelope
                let key_data = ssh_sig.public_key();
                let pk_bytes = match key_data {
                    ssh_key::public::KeyData::Ed25519(ed) => {
                        let bytes: &[u8] = ed.as_ref();
                        bytes.to_vec()
                    }
                    _ => return Err(StatusCode::UNAUTHORIZED),
                };

                // Look up the agent by public key
                let row = conn
                    .query(
                        "SELECT a.id, a.name, s.name as sponsor_name
                         FROM agent_keys ak
                         JOIN agents a ON ak.agent_id = a.id
                         JOIN sponsors s ON a.sponsor_id = s.id
                         WHERE ak.public_key_bytes = ?1",
                        [pk_bytes.clone()],
                    )
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                    .next()
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                    .ok_or(StatusCode::UNAUTHORIZED)?;

                let agent_id: Uuid = row.get::<String>(0)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                    .parse()
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                let agent_name: String = row.get::<String>(1)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                let sponsor_name: String = row.get::<String>(2)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                // Verify the signature using PublicKey::verify
                let ssh_pubkey = ssh_key::PublicKey::from(key_data.clone());
                let message = format!("{timestamp}");
                ssh_pubkey.verify(SSH_SIG_NAMESPACE, message.as_bytes(), &ssh_sig)
                    .map_err(|e| {
                        tracing::warn!(%agent_id, error = %e, "SSH signature verification failed");
                        StatusCode::UNAUTHORIZED
                    })?;

                Ok(AuthAgent { agent_id, agent_name, sponsor_name })
            }

            ParsedToken::Legacy { agent_id, timestamp, signature } => {
                // Look up all keys for this agent
                let mut rows = conn
                    .query(
                        "SELECT a.name, ak.public_key_bytes, s.name as sponsor_name
                         FROM agents a
                         JOIN agent_keys ak ON ak.agent_id = a.id
                         JOIN sponsors s ON a.sponsor_id = s.id
                         WHERE a.id = ?1",
                        [agent_id.to_string()],
                    )
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                // Try each key for this agent
                let message = format!("{agent_id}:{timestamp}");
                let mut agent_name = String::new();
                let mut sponsor_name = String::new();
                let mut verified = false;

                while let Some(r) = rows.next().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
                    agent_name = r.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    let pk_bytes: Vec<u8> = r.get::<Vec<u8>>(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    sponsor_name = r.get::<String>(2).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                    if let Ok(pk_array) = <[u8; 32]>::try_from(pk_bytes.as_slice()) {
                        if let Ok(vk) = VerifyingKey::from_bytes(&pk_array) {
                            if vk.verify(message.as_bytes(), &signature).is_ok() {
                                verified = true;
                                break;
                            }
                        }
                    }
                }

                if !verified {
                    tracing::warn!(%agent_id, "no matching key for legacy signature");
                    return Err(StatusCode::UNAUTHORIZED);
                }

                Ok(AuthAgent { agent_id, agent_name, sponsor_name })
            }
        }
    }
}

/// Parse a bearer token.
///
/// Two formats:
/// - 2 parts: `{timestamp}:{ssh_sig_base64}` (new SSH format)
/// - 3 parts: `{agent_id}:{timestamp}:{raw_sig_base64}` (legacy)
fn parse_token(token: &str) -> anyhow::Result<ParsedToken> {
    use base64::Engine;

    // Try 2-part first: check if the first segment is a pure numeric timestamp
    let parts: Vec<&str> = token.splitn(3, ':').collect();

    if parts.len() == 2 {
        let timestamp: u64 = parts[0].parse()?;
        let sig_bytes = base64::engine::general_purpose::STANDARD.decode(parts[1])?;
        let sig_text = std::str::from_utf8(&sig_bytes)?;
        let ssh_sig: ssh_key::SshSig = sig_text.parse()?;
        return Ok(ParsedToken::Ssh { timestamp, ssh_sig });
    }

    if parts.len() == 3 {
        let agent_id: Uuid = parts[0].parse()?;
        let timestamp: u64 = parts[1].parse()?;
        let sig_bytes = base64::engine::general_purpose::STANDARD.decode(parts[2])?;

        if sig_bytes.len() == 64 {
            let signature = Signature::from_slice(&sig_bytes)?;
            return Ok(ParsedToken::Legacy { agent_id, timestamp, signature });
        }

        // 3-part SSH sig — not supported, use 2-part format
        anyhow::bail!("use 2-part format for SSH signatures: {{timestamp}}:{{ssh_sig_base64}}");
    }

    anyhow::bail!("expected 2 or 3 colon-separated parts");
}

/// Middleware that rejects unauthenticated requests with 401.
///
/// Used to protect route groups (like git endpoints) that use a different
/// state type and can't use the `AuthAgent` extractor directly.
pub async fn require_agent_auth(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let (mut parts, body) = request.into_parts();
    let agent = AuthAgent::from_request_parts(&mut parts, &state).await?;
    // Store agent ID for git ref-check (the git crate reads this via AgentIdExt)
    parts.extensions.insert(agentcoderepo_git::AgentIdExt(agent.agent_id.to_string()));
    parts.extensions.insert(agent);
    let request = axum::extract::Request::from_parts(parts, body);
    Ok(next.run(request).await)
}

/// Create a signed bearer token for an agent (raw Ed25519 signature, legacy format).
///
/// Public so the test harness can generate tokens.
pub fn make_bearer_token(
    agent_id: &Uuid,
    signing_key: &ed25519_dalek::SigningKey,
) -> String {
    use base64::Engine;
    use ed25519_dalek::Signer;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let message = format!("{agent_id}:{timestamp}");
    let signature = signing_key.sign(message.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    format!("{agent_id}:{timestamp}:{sig_b64}")
}
