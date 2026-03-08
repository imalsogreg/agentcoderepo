use std::path::Path;

use anyhow::{Context, Result};
use axum::body::Body;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::io::ReaderStream;
use tracing::Instrument;

/// Run `git <service> --stateless-rpc --advertise-refs <repo>` and return stdout.
///
/// The advertise-refs response is small (ref list + capabilities), so buffering is fine.
#[tracing::instrument(skip(repo_path), fields(repo = %repo_path.display()))]
pub async fn advertise_refs(repo_path: &Path, service: &str) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args([service, "--stateless-rpc", "--advertise-refs"])
        .arg(repo_path)
        .output()
        .await
        .context("failed to spawn git")?;

    if !output.status.success() {
        anyhow::bail!(
            "git {service} --advertise-refs exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }

    Ok(output.stdout)
}

/// Spawn `git <service> --stateless-rpc <repo>`, stream the request body
/// into stdin, and return stdout as a streaming response body.
///
/// This avoids buffering the entire packfile in memory — chunks flow
/// from the HTTP request through git and back out as they arrive.
///
/// Spawned tasks are instrumented with the caller's span, so chunk-level
/// events appear grouped under the originating request in traces.
#[tracing::instrument(skip(repo_path, request_body), fields(repo = %repo_path.display()))]
pub async fn stateless_rpc_streaming(
    repo_path: &Path,
    service: &str,
    request_body: Body,
) -> Result<Body> {
    let mut child = Command::new("git")
        .args([service, "--stateless-rpc"])
        .arg(repo_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git")?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");

    tracing::debug!("spawned git process");

    // Capture the current span so spawned tasks inherit it.
    // Without this, tokio::spawn starts a new root span and
    // chunk events won't be grouped under the request.
    let stdin_span = tracing::info_span!("pipe_stdin");
    tokio::spawn(
        async move {
            if let Err(e) = pipe_body_to_stdin(request_body, stdin).await {
                tracing::warn!(error = %e, "stdin pipe failed");
            }
        }
        .instrument(stdin_span),
    );

    // Stream git's stdout back as the response body.
    let stdout_stream = ReaderStream::new(tokio::io::BufReader::new(stdout));
    let body = Body::from_stream(stdout_stream);

    // Spawn a background task to wait on the child and log exit status.
    let wait_span = tracing::info_span!("wait_child");
    tokio::spawn(
        async move {
            match child.wait().await {
                Ok(status) if !status.success() => {
                    tracing::warn!(%status, "git process exited with error");
                }
                Ok(status) => {
                    tracing::debug!(%status, "git process exited");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to wait on git process");
                }
            }
        }
        .instrument(wait_span),
    );

    Ok(body)
}

/// Run `git <service> --stateless-rpc <repo>`, pipe the request body to stdin,
/// and collect the full stdout output. Used for receive-pack where the response
/// is small (status lines) and we need to wait for completion before indexing.
#[tracing::instrument(skip(repo_path, request_body), fields(repo = %repo_path.display()))]
pub async fn stateless_rpc_buffered(
    repo_path: &Path,
    service: &str,
    request_body: Body,
) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .args([service, "--stateless-rpc"])
        .arg(repo_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git")?;

    let stdin = child.stdin.take().expect("stdin was piped");

    // Pipe request body to git stdin
    let stdin_span = tracing::info_span!("pipe_stdin");
    let pipe_handle = tokio::spawn(
        async move {
            if let Err(e) = pipe_body_to_stdin(request_body, stdin).await {
                tracing::warn!(error = %e, "stdin pipe failed");
            }
        }
        .instrument(stdin_span),
    );

    // Wait for both stdin piping and process completion
    pipe_handle.await.ok();
    let output = child.wait_with_output().await.context("failed to wait on git")?;

    if !output.status.success() {
        tracing::warn!(
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "git process exited with error"
        );
    } else {
        tracing::debug!(status = %output.status, bytes = output.stdout.len(), "git process complete");
    }

    Ok(output.stdout)
}

/// Get the current HEAD SHA, or a string of zeros if the repo has no commits.
pub async fn current_head(repo_path: &Path) -> Result<String> {
    let zeros = "0000000000000000000000000000000000000000".to_string();

    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .await
        .context("failed to run git rev-parse HEAD")?;

    if !output.status.success() {
        return Ok(zeros);
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Empty repos return the literal "HEAD" instead of a SHA
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(zeros);
    }

    Ok(sha)
}

/// Read chunks from an HTTP body and write them to a process's stdin.
async fn pipe_body_to_stdin(
    body: Body,
    mut stdin: tokio::process::ChildStdin,
) -> Result<()> {
    use http_body_util::BodyExt as _;

    let mut body = body;
    let mut total_bytes: usize = 0;
    let mut chunk_count: usize = 0;

    while let Some(frame_result) = body.frame().await {
        let frame = frame_result.context("error reading request body")?;
        if let Some(chunk) = frame.data_ref() {
            let len = chunk.len();
            stdin.write_all(chunk).await.context("error writing to git stdin")?;
            total_bytes += len;
            chunk_count += 1;
            tracing::trace!(chunk_bytes = len, total_bytes, chunk_count, "wrote chunk to stdin");
        }
    }

    stdin.shutdown().await.context("error closing git stdin")?;
    tracing::debug!(total_bytes, chunk_count, "stdin pipe complete");
    Ok(())
}
