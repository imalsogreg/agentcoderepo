//! Git plumbing helpers for reading changed files from bare repos.

use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

/// A file that changed between two commits.
#[derive(Debug)]
pub struct ChangedFile {
    pub status: FileStatus,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
}

/// Find files that changed between two commits in a bare repo.
///
/// If `old_sha` is all zeros (new branch), lists all files at `new_sha`.
pub async fn changed_files(
    repo_path: &Path,
    old_sha: &str,
    new_sha: &str,
) -> Result<Vec<ChangedFile>> {
    let is_new_branch = old_sha.chars().all(|c| c == '0');

    let output = if is_new_branch {
        Command::new("git")
            .args(["diff-tree", "-r", "--name-status", "--no-commit-id", "--root", new_sha])
            .current_dir(repo_path)
            .output()
            .await
            .context("failed to run git diff-tree")?
    } else {
        Command::new("git")
            .args(["diff-tree", "-r", "--name-status", "--no-commit-id", old_sha, new_sha])
            .current_dir(repo_path)
            .output()
            .await
            .context("failed to run git diff-tree")?
    };

    if !output.status.success() {
        anyhow::bail!(
            "git diff-tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: "A\tpath/to/file" or "M\tpath/to/file"
        let (status_char, path) = line
            .split_once('\t')
            .context("unexpected diff-tree line format")?;

        let status = match status_char {
            "A" => FileStatus::Added,
            "M" => FileStatus::Modified,
            "D" => FileStatus::Deleted,
            _ => continue, // skip renames, copies, etc for now
        };

        files.push(ChangedFile {
            status,
            path: path.to_string(),
        });
    }

    Ok(files)
}

/// Read the contents of a file at a specific commit from a bare repo.
pub async fn read_file_at_commit(
    repo_path: &Path,
    commit: &str,
    file_path: &str,
) -> Result<String> {
    let output = Command::new("git")
        .args(["show", &format!("{commit}:{file_path}")])
        .current_dir(repo_path)
        .output()
        .await
        .context("failed to run git show")?;

    if !output.status.success() {
        anyhow::bail!(
            "git show {}:{} failed: {}",
            commit,
            file_path,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Get the current HEAD ref SHA of a bare repo, or None if the repo is empty.
pub async fn head_sha(repo_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .await
        .context("failed to run git rev-parse HEAD")?;

    if !output.status.success() {
        // Empty repo has no HEAD
        return Ok(None);
    }

    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}
