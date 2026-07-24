//! GitRunner seam: a controlled system-`git` process adapter.
//!
//! `ARC-004` and the GitRunner seam: projects/workspace-sync drive clone,
//! status, diff, index and remote operations through this thin process layer.
//! Production and local both use the system Git adapter; tests use real
//! temporary Git repositories with the same adapter rather than rewriting Git
//! in memory. Only crash-simulation tests use a fake runner.
//!
//! Commands run with `GIT_OPTIONAL_LOCKS=0` and read-only arguments so `status`
//! does not try to refresh the index and fail when a reader races a writer.

use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

use rand::RngCore;

/// Normalized Git failure for the GitRunner seam. Callers map these to the
/// stable `GIT_*` Problem codes (`docs/08-events-and-errors.md`).
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
pub enum GitError {
    #[error("git authentication failed")]
    AuthFailed,
    #[error("git remote is unreachable")]
    RemoteUnavailable,
    #[error("git non-fast-forward")]
    NonFastForward,
    #[error("git index is not empty")]
    IndexNotEmpty,
    #[error("git histories diverged")]
    Diverged,
    #[error("git checkout would overwrite local changes")]
    CheckoutConflict,
    #[error("git update produced a three-way content conflict: {paths:?}")]
    UpdateConflict { paths: Vec<String> },
    #[error("git process failed: {0}")]
    CommandFailed(String),
    #[error("git output was not valid UTF-8 or unexpected: {0}")]
    BadOutput(String),
}

impl GitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthFailed => "GIT_AUTH_FAILED",
            Self::RemoteUnavailable => "GIT_REMOTE_UNAVAILABLE",
            Self::NonFastForward => "GIT_NON_FAST_FORWARD",
            Self::IndexNotEmpty => "GIT_INDEX_NOT_EMPTY",
            Self::Diverged => "GIT_DIVERGED",
            Self::CheckoutConflict => "GIT_CHECKOUT_CONFLICT",
            Self::UpdateConflict { .. } => "GIT_UPDATE_CONFLICT",
            Self::CommandFailed(_) | Self::BadOutput(_) => "INTERNAL_ERROR",
        }
    }
}

/// A Git credential passed to clone/fetch/push. PATs are injected via a
/// short-lived `GIT_ASKPASS` helper so the secret never lands in git config.
#[derive(Debug, Clone)]
pub enum GitCredential {
    None,
    /// `(username, password)` for HTTPS basic auth. The password is the PAT.
    HttpsBasic { username: String, password: String },
}

/// Three-layer status projection (`WS-GIT-01`, HTTP API `git/status`).
#[derive(Debug, Clone, Serialize, Default)]
pub struct GitStatus {
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub working: Vec<String>,
    pub index: Vec<String>,
    pub untracked: Vec<String>,
}

/// One diff view among the three supported by `GET /git/diff`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffView {
    WorkingVsIndex,
    IndexVsHead,
    WorkingVsHead,
}

impl DiffView {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::WorkingVsIndex => &["diff"],
            Self::IndexVsHead => &["diff", "--cached"],
            Self::WorkingVsHead => &["diff", "HEAD"],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GitLogEntry {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: String,
    pub message: String,
}

/// Result of a three-way `update`: fast-forward, a stable non-conflict failure,
/// or a content conflict that records the affected paths (`WS-GIT-04`).
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    FastForward { new_head: String },
    Failed(GitError),
    Conflict { paths: Vec<String> },
}

/// The GitRunner seam. The system implementation shells out to `git`; tests can
/// substitute a fake for crash simulation only.
pub trait GitRunner: Send + Sync {
    fn clone(
        &self,
        url: &str,
        branch: Option<&str>,
        into: &Path,
        credential: &GitCredential,
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;

    fn status(&self, repo: &Path) -> impl std::future::Future<Output = Result<GitStatus, GitError>> + Send;

    fn diff(
        &self,
        repo: &Path,
        view: DiffView,
    ) -> impl std::future::Future<Output = Result<String, GitError>> + Send;

    fn log(
        &self,
        repo: &Path,
        limit: u32,
    ) -> impl std::future::Future<Output = Result<Vec<GitLogEntry>, GitError>> + Send;

    fn branches(
        &self,
        repo: &Path,
    ) -> impl std::future::Future<Output = Result<Vec<String>, GitError>> + Send;

    fn remotes(
        &self,
        repo: &Path,
    ) -> impl std::future::Future<Output = Result<Vec<String>, GitError>> + Send;

    fn fetch(
        &self,
        repo: &Path,
        remote: &str,
        credential: &GitCredential,
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;

    fn stage(
        &self,
        repo: &Path,
        paths: &[String],
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;

    fn unstage(
        &self,
        repo: &Path,
        paths: &[String],
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;

    fn commit(
        &self,
        repo: &Path,
        message: &str,
    ) -> impl std::future::Future<Output = Result<String, GitError>> + Send;

    fn push(
        &self,
        repo: &Path,
        remote: &str,
        branch: &str,
        credential: &GitCredential,
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;

    fn update(
        &self,
        repo: &Path,
        remote: &str,
        branch: &str,
        credential: &GitCredential,
    ) -> impl std::future::Future<Output = Result<UpdateOutcome, GitError>> + Send;

    fn checkout(
        &self,
        repo: &Path,
        branch: &str,
    ) -> impl std::future::Future<Output = Result<(), GitError>> + Send;
}

/// System `git` process implementation of `GitRunner`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGit;

impl SystemGit {
    fn base(repo: &Path) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Apply a credential via a short-lived `GIT_ASKPASS` helper so the secret
    /// never lands in git config. Writes a tiny shell script to a temp file
    /// that prints username or password when git asks, sets `GIT_ASKPASS` +
    /// `GIT_USERNAME`/`GIT_PASSWORD` env on the command, and returns the script
    /// path so the caller can delete it after the command finishes.
    fn apply_credential(
        command: &mut Command,
        credential: &GitCredential,
    ) -> Result<Option<std::path::PathBuf>, GitError> {
        match credential {
            GitCredential::None => Ok(None),
            GitCredential::HttpsBasic { username, password } => {
                let script = write_askpass_script(username, password)?;
                command.env("GIT_USERNAME", username);
                command.env("GIT_PASSWORD", password);
                command.env("GIT_ASKPASS", &script);
                Ok(Some(script))
            }
        }
    }

    async fn run(command: &mut Command) -> Result<String, GitError> {
        let output = command
            .output()
            .await
            .map_err(|error| GitError::CommandFailed(error.to_string()))?;
        if output.status.success() {
            String::from_utf8(output.stdout).map_err(|e| GitError::BadOutput(e.to_string()))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(classify_failure(&stderr))
        }
    }
}

/// Map a git stderr blob to a normalized `GitError`. Covers the failure modes
/// the public API exposes: auth, unreachable, non-fast-forward, index-not-empty,
/// diverged, checkout conflict. Unknown failures become `CommandFailed`.
fn classify_failure(stderr: &str) -> GitError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("permission denied")
        || lower.contains("403")
    {
        GitError::AuthFailed
    } else if lower.contains("could not resolve host")
        || lower.contains("connection timed out")
        || lower.contains("failed to connect")
        || lower.contains("connection refused")
    {
        GitError::RemoteUnavailable
    } else if lower.contains("non-fast-forward") || lower.contains("rejected") {
        GitError::NonFastForward
    } else if lower.contains("diverged") {
        GitError::Diverged
    } else if lower.contains("your local changes to the following files would be overwritten")
        || lower.contains("would be overwritten by checkout")
    {
        GitError::CheckoutConflict
    } else {
        GitError::CommandFailed(stderr.trim().to_owned())
    }
}

impl GitRunner for SystemGit {
    async fn clone(
        &self,
        url: &str,
        branch: Option<&str>,
        into: &Path,
        credential: &GitCredential,
    ) -> Result<(), GitError> {
        let parent = into
            .parent()
            .ok_or_else(|| GitError::BadOutput("clone target has no parent dir".into()))?;
        std::fs::create_dir_all(parent).ok();
        let mut command = Command::new("git");
        command
            .current_dir(parent)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("clone")
            .arg("--quiet");
        if let Some(branch) = branch {
            command.arg("--branch").arg(branch);
        }
        command.arg(url).arg(into.file_name().unwrap_or_default());
        let askpass = Self::apply_credential(&mut command, credential)?;
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let output = command
            .output()
            .await
            .map_err(|e| GitError::CommandFailed(e.to_string()))?;
        if let Some(script) = askpass {
            let _ = std::fs::remove_file(script);
        }
        if output.status.success() {
            Ok(())
        } else {
            Err(classify_failure(&String::from_utf8_lossy(&output.stderr)))
        }
    }

    async fn status(&self, repo: &Path) -> Result<GitStatus, GitError> {
        let mut command = Self::base(repo);
        command
            .arg("status")
            .arg("--porcelain=v2")
            .arg("--branch");
        let output = Self::run(&mut command).await?;
        Ok(parse_porcelain_v2(&output))
    }

    async fn diff(&self, repo: &Path, view: DiffView) -> Result<String, GitError> {
        let mut command = Self::base(repo);
        command.args(view.args()).arg("--no-color");
        Self::run(&mut command).await
    }

    async fn log(&self, repo: &Path, limit: u32) -> Result<Vec<GitLogEntry>, GitError> {
        let mut command = Self::base(repo);
        command
            .arg("log")
            .arg(format!("-n{limit}"))
            .arg("--format=%H%x00%P%x00%an%x00%s");
        let output = Self::run(&mut command).await?;
        let mut entries = Vec::new();
        for line in output.lines() {
            let parts: Vec<&str> = line.split('\0').collect();
            if parts.len() == 4 {
                entries.push(GitLogEntry {
                    sha: parts[0].to_owned(),
                    parents: if parts[1].is_empty() {
                        Vec::new()
                    } else {
                        parts[1].split(' ').map(str::to_owned).collect()
                    },
                    author: parts[2].to_owned(),
                    message: parts[3].to_owned(),
                });
            }
        }
        Ok(entries)
    }

    async fn branches(&self, repo: &Path) -> Result<Vec<String>, GitError> {
        let mut command = Self::base(repo);
        command.arg("for-each-ref").arg("--format=%(refname:short)").arg("refs/heads/");
        let output = Self::run(&mut command).await?;
        Ok(output.lines().map(str::to_owned).collect())
    }

    async fn remotes(&self, repo: &Path) -> Result<Vec<String>, GitError> {
        let mut command = Self::base(repo);
        command.arg("remote");
        let output = Self::run(&mut command).await?;
        Ok(output.lines().map(str::to_owned).collect())
    }

    async fn fetch(
        &self,
        repo: &Path,
        remote: &str,
        credential: &GitCredential,
    ) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command.arg("fetch").arg("--quiet").arg(remote);
        let askpass = Self::apply_credential(&mut command, credential)?;
        let result = Self::run(&mut command).await;
        if let Some(script) = askpass {
            let _ = std::fs::remove_file(script);
        }
        result?;
        Ok(())
    }

    async fn stage(&self, repo: &Path, paths: &[String]) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command.arg("add").arg("--");
        for path in paths {
            command.arg(path);
        }
        Self::run(&mut command).await?;
        Ok(())
    }

    async fn unstage(&self, repo: &Path, paths: &[String]) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command.arg("reset").arg("HEAD").arg("--");
        for path in paths {
            command.arg(path);
        }
        Self::run(&mut command).await?;
        Ok(())
    }

    async fn commit(&self, repo: &Path, message: &str) -> Result<String, GitError> {
        let mut command = Self::base(repo);
        command.arg("commit").arg("--quiet").arg("-m").arg(message);
        Self::run(&mut command).await?;
        // Return the new HEAD sha.
        let mut rev = Self::base(repo);
        rev.arg("rev-parse").arg("HEAD");
        Self::run(&mut rev).await.map(|s| s.trim().to_owned())
    }

    async fn push(
        &self,
        repo: &Path,
        remote: &str,
        branch: &str,
        credential: &GitCredential,
    ) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command
            .arg("push")
            .arg("--quiet")
            .arg(remote)
            .arg(format!("refs/heads/{branch}:refs/heads/{branch}"));
        let askpass = Self::apply_credential(&mut command, credential)?;
        let result = Self::run(&mut command).await;
        if let Some(script) = askpass {
            let _ = std::fs::remove_file(script);
        }
        result?;
        Ok(())
    }

    async fn update(
        &self,
        repo: &Path,
        remote: &str,
        branch: &str,
        credential: &GitCredential,
    ) -> Result<UpdateOutcome, GitError> {
        // Fetch first (does not touch Main working tree).
        self.fetch(repo, remote, credential).await?;

        // Fast-forward only when the index is empty; otherwise return a stable
        // error and leave Main unchanged (`WS-GIT-04`).
        let status = self.status(repo).await?;
        if !status.index.is_empty() {
            return Ok(UpdateOutcome::Failed(GitError::IndexNotEmpty));
        }

        let upstream = format!("{remote}/{branch}");
        // Check merge-base relationship to detect divergence without touching HEAD.
        let mut base = Self::base(repo);
        base.arg("merge-base")
            .arg("HEAD")
            .arg(&upstream);
        let base_sha = Self::run(&mut base).await?.trim().to_owned();
        let mut head = Self::base(repo);
        head.arg("rev-parse").arg("HEAD");
        let head_sha = Self::run(&mut head).await?.trim().to_owned();
        let mut remote_head = Self::base(repo);
        remote_head
            .arg("rev-parse")
            .arg(&upstream);
        let remote_sha = Self::run(&mut remote_head).await?.trim().to_owned();

        if base_sha == remote_sha {
            // Remote is behind or equal to HEAD: nothing to update.
            return Ok(UpdateOutcome::FastForward { new_head: head_sha });
        }
        if base_sha != head_sha {
            // Local history diverged from the remote branch.
            return Ok(UpdateOutcome::Failed(GitError::Diverged));
        }
        // base == HEAD: fast-forward possible. Attempt a fast-forward merge that
        // does not commit conflicts; on conflict, leave HEAD/index/tree unchanged.
        let mut ff = Self::base(repo);
        ff.arg("merge")
            .arg("--ff-only")
            .arg("--no-edit")
            .arg(&upstream);
        let output = ff
            .output()
            .await
            .map_err(|e| GitError::CommandFailed(e.to_string()))?;
        if output.status.success() {
            return Ok(UpdateOutcome::FastForward { new_head: remote_sha });
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // A fast-forward should never conflict; if it does, surface the paths and
        // reset HEAD to its pre-merge state so Main is left untouched.
        let conflict_paths = extract_conflict_paths(&stderr);
        if conflict_paths.is_empty() {
            return Ok(UpdateOutcome::Failed(classify_failure(&stderr)));
        }
        let mut reset = Self::base(repo);
        reset.arg("merge").arg("--abort");
        let _ = reset.output().await;
        Ok(UpdateOutcome::Conflict {
            paths: conflict_paths,
        })
    }

    async fn checkout(&self, repo: &Path, branch: &str) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command.arg("checkout").arg("--quiet").arg(branch);
        Self::run(&mut command).await?;
        Ok(())
    }
}

/// Parse `git status --porcelain=v2 --branch` into the three-layer projection.
fn parse_porcelain_v2(output: &str) -> GitStatus {
    let mut head_sha = None;
    let mut branch = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut working = Vec::new();
    let mut index = Vec::new();
    let mut untracked = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("# branch.oid ") {
            if rest != "(initial)" {
                head_sha = Some(rest.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut parts = rest.split_whitespace();
            ahead = parts.next().and_then(|s| s.trim_start_matches('+').parse().ok()).unwrap_or(0);
            behind = parts.next().and_then(|s| s.trim_start_matches('-').parse().ok()).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("u ") {
            // Unmerged entry: conflict in working tree.
            if let Some(path) = rest.split_whitespace().nth(8) {
                working.push(path.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            untracked.push(rest.to_owned());
        } else {
            let (rest, is_rename) = if let Some(rest) = line.strip_prefix("1 ") {
                (rest, false)
            } else if let Some(rest) = line.strip_prefix("2 ") {
                (rest, true)
            } else {
                continue;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let xy = fields.first().copied().unwrap_or("");
            // path index differs between "1" (8) and "2" (rename: oldpath at 8, new at 9).
            let path = if is_rename {
                fields.get(9).copied().unwrap_or("")
            } else {
                fields.get(8).copied().unwrap_or("")
            };
            if !path.is_empty() {
                let (x, y) = (
                    xy.as_bytes().first().copied().unwrap_or(b' '),
                    xy.as_bytes().get(1).copied().unwrap_or(b' '),
                );
                if x == b'.' && y != b'.' {
                    working.push(path.to_owned());
                } else if x != b'.' && y == b'.' {
                    index.push(path.to_owned());
                } else if x != b'.' && y != b'.' {
                    index.push(path.to_owned());
                    working.push(path.to_owned());
                }
            }
        }
    }
    GitStatus {
        head_sha,
        branch,
        ahead,
        behind,
        working,
        index,
        untracked,
    }
}

fn extract_conflict_paths(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("CONFLICT") || line.starts_with("Merge conflict"))
        .map(str::to_owned)
        .collect()
}

/// Write a one-shot `GIT_ASKPASS` shell script that prints the username or
/// password from env. Linux-first (production is single-host Linux); on Windows
/// the script is still written but git needs a POSIX shell on PATH for private
/// clone — public clone is unaffected. The script is deleted by the caller after
/// the git command completes so the secret does not linger.
fn write_askpass_script(username: &str, password: &str) -> Result<std::path::PathBuf, GitError> {
    let _ = (username, password);
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let nonce = hex::encode(bytes);
    let path = std::env::temp_dir().join(format!("janus-askpass-{nonce}.sh"));
    let script = "#!/bin/sh\ncase \"$1\" in\n  *sername*) echo \"$GIT_USERNAME\" ;;\n  *assword*) echo \"$GIT_PASSWORD\" ;;\nesac\n";
    std::fs::write(&path, script)
        .map_err(|e| GitError::CommandFailed(format!("write askpass: {e}")))?;
    // Make the script executable where the platform supports it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::classify_failure;
    use crate::adapters::git::GitError;

    #[test]
    fn classifies_auth_and_remote_errors() {
        assert!(matches!(
            classify_failure("fatal: Authentication failed for 'https://x'"),
            GitError::AuthFailed
        ));
        assert!(matches!(
            classify_failure("fatal: unable to access: Could not resolve host: x"),
            GitError::RemoteUnavailable
        ));
    }

    #[test]
    fn classifies_non_fast_forward_and_diverged() {
        assert!(matches!(
            classify_failure("! [rejected] main -> main (non-fast-forward)"),
            GitError::NonFastForward
        ));
        assert!(matches!(
            classify_failure("fatal: refusing to merge unrelated histories (diverged)"),
            GitError::Diverged
        ));
    }
}
