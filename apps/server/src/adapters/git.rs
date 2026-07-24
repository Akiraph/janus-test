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
    HttpsBasic {
        username: String,
        password: String,
    },
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

/// One path that collides between local working-tree edits and the incoming
/// remote update. Used to persist a Git Update Conflict without writing markers.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateConflictPath {
    pub path: String,
    pub kind: String,
    pub base_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub main_hash: Option<String>,
}

/// Result of a three-way `update`: fast-forward, a stable non-conflict failure,
/// or a content conflict that records the affected paths (`WS-GIT-04`).
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// HEAD advanced to the remote tip; working-tree-only edits were preserved.
    FastForward {
        new_head: String,
        base_tree: String,
        remote_tree: String,
    },
    Failed(GitError),
    /// Main was left completely unchanged. Caller persists the conflict rows.
    Conflict {
        paths: Vec<UpdateConflictPath>,
        base_tree: String,
        remote_tree: String,
        main_tree: String,
        head_sha: String,
        remote_sha: String,
    },
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

    fn status(
        &self,
        repo: &Path,
    ) -> impl std::future::Future<Output = Result<GitStatus, GitError>> + Send;

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
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        command.arg("status").arg("--porcelain=v2").arg("--branch");
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
        command
            .arg("for-each-ref")
            .arg("--format=%(refname:short)")
            .arg("refs/heads/");
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
        let head_sha = rev_parse(repo, "HEAD").await?;
        let remote_sha = rev_parse(repo, &upstream).await?;
        let base_sha = match merge_base(repo, "HEAD", &upstream).await {
            Ok(sha) => sha,
            Err(_) => {
                return Ok(UpdateOutcome::Failed(GitError::Diverged));
            }
        };

        if base_sha == remote_sha {
            // Remote is behind or equal to HEAD: nothing to update.
            return Ok(UpdateOutcome::FastForward {
                new_head: head_sha.clone(),
                base_tree: head_sha.clone(),
                remote_tree: remote_sha,
            });
        }
        if base_sha != head_sha {
            // Local history diverged from the remote branch.
            return Ok(UpdateOutcome::Failed(GitError::Diverged));
        }

        // Compute path-level conflicts between working tree edits and the
        // incoming remote tree *before* mutating HEAD. If anything collides,
        // leave HEAD/index/working tree completely unchanged.
        let conflict_paths =
            compute_update_conflicts(repo, &base_sha, &remote_sha, &head_sha).await?;
        if !conflict_paths.is_empty() {
            return Ok(UpdateOutcome::Conflict {
                paths: conflict_paths,
                base_tree: base_sha,
                remote_tree: remote_sha.clone(),
                main_tree: head_sha.clone(),
                head_sha,
                remote_sha,
            });
        }

        // No path conflicts: advance HEAD with --ff-only. Working-tree-only
        // edits on non-conflicting paths are preserved by git.
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
            return Ok(UpdateOutcome::FastForward {
                new_head: remote_sha.clone(),
                base_tree: base_sha,
                remote_tree: remote_sha,
            });
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // If git refused because local edits would be overwritten, recompute
        // path conflicts and surface them instead of a bare checkout error.
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("overwritten") || lower.contains("conflict") {
            let mut conflict_paths =
                compute_update_conflicts(repo, &base_sha, &remote_sha, &head_sha).await?;
            if conflict_paths.is_empty() {
                // Last-resort: parse "would be overwritten by merge:\n\t<path>" lines.
                for line in stderr.lines() {
                    let path = line.trim().trim_start_matches('\t');
                    if path.is_empty()
                        || path.contains(' ')
                        || path.contains(':')
                        || path.starts_with("error")
                        || path.starts_with("Updating")
                        || path.starts_with("hint")
                    {
                        continue;
                    }
                    if path.contains('/') || path.contains('.') {
                        let base_hash = blob_hash_at(repo, &base_sha, path).await.ok().flatten();
                        let remote_hash = blob_hash_at(repo, &remote_sha, path).await.ok().flatten();
                        let main_hash = working_blob_hash(repo, path)
                            .await
                            .ok()
                            .flatten()
                            .or(blob_hash_at(repo, &head_sha, path).await.ok().flatten());
                        conflict_paths.push(UpdateConflictPath {
                            path: path.to_owned(),
                            kind: "text".into(),
                            base_hash,
                            remote_hash,
                            main_hash,
                        });
                    }
                }
            }
            if !conflict_paths.is_empty() {
                return Ok(UpdateOutcome::Conflict {
                    paths: conflict_paths,
                    base_tree: base_sha,
                    remote_tree: remote_sha.clone(),
                    main_tree: head_sha.clone(),
                    head_sha,
                    remote_sha,
                });
            }
        }
        Ok(UpdateOutcome::Failed(classify_failure(&stderr)))
    }

    async fn checkout(&self, repo: &Path, branch: &str) -> Result<(), GitError> {
        let mut command = Self::base(repo);
        command.arg("checkout").arg("--quiet").arg(branch);
        Self::run(&mut command).await?;
        Ok(())
    }
}

async fn rev_parse(repo: &Path, rev: &str) -> Result<String, GitError> {
    let mut command = SystemGit::base(repo);
    command.arg("rev-parse").arg(rev);
    Ok(SystemGit::run(&mut command).await?.trim().to_owned())
}

async fn merge_base(repo: &Path, a: &str, b: &str) -> Result<String, GitError> {
    let mut command = SystemGit::base(repo);
    command.arg("merge-base").arg(a).arg(b);
    Ok(SystemGit::run(&mut command).await?.trim().to_owned())
}

/// List paths whose working-tree content differs from HEAD, or whose remote
/// tree differs from base, and mark true three-way collisions.
async fn compute_update_conflicts(
    repo: &Path,
    base_sha: &str,
    remote_sha: &str,
    head_sha: &str,
) -> Result<Vec<UpdateConflictPath>, GitError> {
    // Paths changed on the remote side (base..remote).
    let remote_changed = diff_name_status(repo, base_sha, remote_sha).await?;
    // Paths dirty in the working tree relative to HEAD.
    let working_dirty = working_dirty_paths(repo).await?;

    let mut conflicts = Vec::new();
    for (path, remote_kind) in &remote_changed {
        if !working_dirty.contains(path) {
            continue;
        }
        let base_hash = blob_hash_at(repo, base_sha, path).await.ok().flatten();
        let remote_hash = blob_hash_at(repo, remote_sha, path).await.ok().flatten();
        let main_hash = blob_hash_at(repo, head_sha, path).await.ok().flatten();
        // Working tree may have uncommitted edits; prefer a content hash of the
        // working file when it still exists.
        let working_hash = working_blob_hash(repo, path).await.ok().flatten();
        let main_hash = working_hash.or(main_hash);
        let kind = classify_conflict_kind(remote_kind, base_hash.as_deref(), remote_hash.as_deref(), main_hash.as_deref());
        conflicts.push(UpdateConflictPath {
            path: path.clone(),
            kind,
            base_hash,
            remote_hash,
            main_hash,
        });
    }
    Ok(conflicts)
}

async fn diff_name_status(
    repo: &Path,
    a: &str,
    b: &str,
) -> Result<Vec<(String, String)>, GitError> {
    let mut command = SystemGit::base(repo);
    command
        .arg("diff")
        .arg("--name-status")
        .arg("--no-renames")
        .arg(format!("{a}..{b}"));
    let output = SystemGit::run(&mut command).await?;
    let mut out = Vec::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let status = parts.next().unwrap_or("M").to_owned();
        let path = parts.next().unwrap_or("").to_owned();
        if !path.is_empty() {
            out.push((path, status));
        }
    }
    Ok(out)
}

async fn working_dirty_paths(repo: &Path) -> Result<std::collections::HashSet<String>, GitError> {
    // Prefer name-only diffs so Windows autocrlf / porcelain quirks cannot hide
    // a dirty path that would block merge --ff-only.
    let mut set = std::collections::HashSet::new();
    let mut modified = SystemGit::base(repo);
    modified.arg("diff").arg("--name-only").arg("HEAD");
    if let Ok(output) = SystemGit::run(&mut modified).await {
        for line in output.lines() {
            if !line.is_empty() {
                set.insert(line.to_owned());
            }
        }
    }
    let mut untracked = SystemGit::base(repo);
    untracked
        .arg("ls-files")
        .arg("--others")
        .arg("--exclude-standard");
    if let Ok(output) = SystemGit::run(&mut untracked).await {
        for line in output.lines() {
            if !line.is_empty() {
                set.insert(line.to_owned());
            }
        }
    }
    // Fallback to porcelain status if the above is empty but status is dirty.
    if set.is_empty() {
        let status = SystemGit.status(repo).await?;
        for path in status
            .working
            .into_iter()
            .chain(status.untracked.into_iter())
        {
            set.insert(path);
        }
    }
    Ok(set)
}

async fn blob_hash_at(
    repo: &Path,
    treeish: &str,
    path: &str,
) -> Result<Option<String>, GitError> {
    let mut command = SystemGit::base(repo);
    command
        .arg("ls-tree")
        .arg(treeish)
        .arg("--")
        .arg(path);
    let output = match SystemGit::run(&mut command).await {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    // format: <mode> <type> <hash>\t<path>
    let line = output.lines().next().unwrap_or("");
    if line.is_empty() {
        return Ok(None);
    }
    let hash = line.split_whitespace().nth(2).map(str::to_owned);
    Ok(hash)
}

async fn working_blob_hash(repo: &Path, path: &str) -> Result<Option<String>, GitError> {
    let abs = repo.join(path);
    if !abs.exists() {
        return Ok(None);
    }
    let mut command = SystemGit::base(repo);
    command.arg("hash-object").arg("--").arg(path);
    match SystemGit::run(&mut command).await {
        Ok(o) => Ok(Some(o.trim().to_owned())),
        Err(_) => Ok(None),
    }
}

fn classify_conflict_kind(
    remote_status: &str,
    base: Option<&str>,
    remote: Option<&str>,
    main: Option<&str>,
) -> String {
    match (base, remote, main) {
        (Some(_), None, Some(_)) => "deleted".into(),
        (None, Some(_), Some(_)) => "added".into(),
        (Some(_), Some(_), None) => "deleted".into(),
        _ if remote_status.starts_with('A') => "added".into(),
        _ if remote_status.starts_with('D') => "deleted".into(),
        _ => "text".into(),
    }
}

/// Apply a resolved path choice onto the Main working tree. Used by the
/// conflict-resolve completion step after all paths have a choice.
pub async fn apply_conflict_choice(
    repo: &Path,
    path: &str,
    choice: &str,
    remote_hash: Option<&str>,
    main_hash: Option<&str>,
    edited_bytes: Option<&[u8]>,
) -> Result<(), GitError> {
    let abs = repo.join(path);
    match choice {
        "main" => {
            // Keep current working tree content. If the path is missing and
            // main_hash is None, ensure it stays deleted.
            if main_hash.is_none() && abs.exists() {
                let _ = tokio::fs::remove_file(&abs).await;
            }
            Ok(())
        }
        "remote" => match remote_hash {
            Some(hash) => {
                let bytes = cat_blob(repo, hash).await?;
                if let Some(parent) = abs.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&abs, bytes)
                    .await
                    .map_err(|e| GitError::CommandFailed(e.to_string()))?;
                Ok(())
            }
            None => {
                if abs.exists() {
                    let _ = tokio::fs::remove_file(&abs).await;
                }
                Ok(())
            }
        },
        "delete" => {
            if abs.exists() {
                if abs.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&abs).await;
                } else {
                    let _ = tokio::fs::remove_file(&abs).await;
                }
            }
            Ok(())
        }
        "edited_text" => {
            let bytes = edited_bytes.ok_or_else(|| {
                GitError::BadOutput("edited_text choice requires body".into())
            })?;
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&abs, bytes)
                .await
                .map_err(|e| GitError::CommandFailed(e.to_string()))?;
            Ok(())
        }
        other => Err(GitError::BadOutput(format!("unknown conflict choice {other}"))),
    }
}

async fn cat_blob(repo: &Path, hash: &str) -> Result<Vec<u8>, GitError> {
    let mut command = SystemGit::base(repo);
    command.arg("cat-file").arg("blob").arg(hash);
    let output = command
        .output()
        .await
        .map_err(|e| GitError::CommandFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(GitError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(output.stdout)
}

/// Fast-forward HEAD/index to remote after conflicts are resolved in the
/// working tree. Caller must ensure the working tree already holds the merged
/// result and the index is empty or will be reset.
pub async fn complete_fast_forward(
    repo: &Path,
    remote: &str,
    branch: &str,
) -> Result<String, GitError> {
    let upstream = format!("{remote}/{branch}");
    // Reset HEAD to remote tip without touching working tree, then re-read sha.
    let mut command = SystemGit::base(repo);
    command
        .arg("update-ref")
        .arg("HEAD")
        .arg(&upstream);
    SystemGit::run(&mut command).await?;
    // Clear the index and re-add the working tree so status is consistent.
    let mut reset = SystemGit::base(repo);
    reset.arg("read-tree").arg("HEAD");
    let _ = SystemGit::run(&mut reset).await;
    rev_parse(repo, "HEAD").await
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
            ahead = parts
                .next()
                .and_then(|s| s.trim_start_matches('+').parse().ok())
                .unwrap_or(0);
            behind = parts
                .next()
                .and_then(|s| s.trim_start_matches('-').parse().ok())
                .unwrap_or(0);
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
