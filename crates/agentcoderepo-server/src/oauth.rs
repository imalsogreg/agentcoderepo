use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use uuid::Uuid;

use serde_json;

use crate::state::AppState;

const SESSION_MAX_AGE_SECS: u64 = 7 * 24 * 3600; // 7 days

// ---------------------------------------------------------------------------
// Login → redirect to GitHub
// ---------------------------------------------------------------------------

pub async fn login_github(
    State(state): State<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    let oauth = state.github_oauth.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;

    let redirect_uri = format!("{}/auth/github/callback", oauth.base_url);
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read:user",
        oauth.client_id,
        redirect_uri,
    );

    Ok(Redirect::temporary(&url).into_response())
}

// ---------------------------------------------------------------------------
// Callback → exchange code, create/find sponsor, set session
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CallbackParams {
    code: String,
}

#[derive(Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GitHubUser {
    id: u64,
    login: String,
    #[serde(default)]
    avatar_url: String,
}

pub async fn github_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
) -> Result<Response, StatusCode> {
    let oauth = state.github_oauth.as_ref().ok_or(StatusCode::NOT_IMPLEMENTED)?;
    let http = reqwest::Client::new();

    // Exchange code for access token
    let token_resp: GitHubTokenResponse = http
        .post(&oauth.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", oauth.client_id.as_str()),
            ("client_secret", oauth.client_secret.as_str()),
            ("code", params.code.as_str()),
        ])
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to exchange code for token");
            StatusCode::BAD_GATEWAY
        })?
        .json()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to parse token response");
            StatusCode::BAD_GATEWAY
        })?;

    // Fetch GitHub user info
    let github_user: GitHubUser = http
        .get(&oauth.userinfo_url)
        .header("Authorization", format!("Bearer {}", token_resp.access_token))
        .header("User-Agent", "agentcoderepo")
        .send()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to fetch github user");
            StatusCode::BAD_GATEWAY
        })?
        .json()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to parse github user");
            StatusCode::BAD_GATEWAY
        })?;

    let github_id_str = github_user.id.to_string();

    // Find or create sponsor
    let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let sponsor_id = {
        // Check if this GitHub user already has a sponsor
        let existing = conn
            .query(
                "SELECT sponsor_id FROM sponsor_github WHERE github_id = ?1",
                [github_id_str.clone()],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .next()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        if let Some(row) = existing {
            // Update login/avatar in case they changed
            let sid: String = row.get::<String>(0).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let _ = conn
                .execute(
                    "UPDATE sponsor_github SET github_login = ?1, avatar_url = ?2 WHERE sponsor_id = ?3",
                    [github_user.login.clone(), github_user.avatar_url.clone(), sid.clone()],
                )
                .await;
            sid
        } else {
            // Create new sponsor
            let sid = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO sponsors (id, name) VALUES (?1, ?2)",
                [sid.clone(), github_user.login.clone()],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to create sponsor");
                StatusCode::CONFLICT
            })?;
            conn.execute(
                "INSERT INTO sponsor_github (sponsor_id, github_id, github_login, avatar_url) VALUES (?1, ?2, ?3, ?4)",
                [sid.clone(), github_id_str, github_user.login.clone(), github_user.avatar_url.clone()],
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            sid
        }
    };

    // Create session
    let session_token = generate_session_token();
    let expires_secs = SESSION_MAX_AGE_SECS;
    conn.execute(
        &format!(
            "INSERT INTO sessions (token, sponsor_id, expires_at) VALUES (?1, ?2, datetime('now', '+{expires_secs} seconds'))"
        ),
        [session_token.clone(), sponsor_id],
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let cookie = format!(
        "session={session_token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={expires_secs}"
    );

    Ok((
        [(axum::http::header::SET_COOKIE, cookie)],
        Redirect::temporary("/"),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if let Some(token) = extract_session_cookie(&headers) {
        let conn = state.db.connect().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let _ = conn
            .execute("DELETE FROM sessions WHERE token = ?1", [token])
            .await;
    }

    let clear_cookie = "session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";
    Ok((
        [(axum::http::header::SET_COOKIE, clear_cookie)],
        Redirect::temporary("/"),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

pub struct SessionSponsor {
    pub id: String,
    pub name: String,
    pub avatar_url: String,
}

/// Look up the logged-in sponsor from the session cookie, if any.
pub async fn get_session_sponsor(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<SessionSponsor> {
    let token = extract_session_cookie(headers)?;
    let conn = state.db.connect().ok()?;
    let row = conn
        .query(
            "SELECT s.id, s.name, COALESCE(sg.avatar_url, '')
             FROM sessions sess
             JOIN sponsors s ON sess.sponsor_id = s.id
             LEFT JOIN sponsor_github sg ON sg.sponsor_id = s.id
             WHERE sess.token = ?1 AND sess.expires_at > datetime('now')",
            [token],
        )
        .await
        .ok()?
        .next()
        .await
        .ok()??;
    Some(SessionSponsor {
        id: row.get::<String>(0).ok()?,
        name: row.get::<String>(1).ok()?,
        avatar_url: row.get::<String>(2).unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Sponsor identity endpoint (session-authed)
// ---------------------------------------------------------------------------

pub async fn sponsor_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    let sponsor = get_session_sponsor(&state, &headers)
        .await
        .ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(axum::Json(serde_json::json!({
        "id": sponsor.id,
        "name": sponsor.name,
        "avatar_url": sponsor.avatar_url,
    })))
}

fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("session=") {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn generate_session_token() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    // Hex-encode without pulling in a hex crate
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
