//! GitRunner adapter: the system-`git` implementation of the port defined in
//! `janus-source-control`.
//!
//! `ARC-004` and the GitRunner seam: projects/workspace drive clone,
//! status, diff, index and remote operations through this thin process layer.
//! Production and local both use the system Git adapter; tests use real
//! temporary Git repositories with the same adapter rather than rewriting Git
//! in memory. Only crash-simulation tests use a fake runner.
//!
//! The DTOs and `GitRunner` trait are owned by `janus-source-control`; this
//! module re-exports them so existing call sites keep compiling and
//! implements the system adapter against them. Commands run with
//! `GIT_OPTIONAL_LOCKS=0` and read-only arguments so `status` does not try to
//! refresh the index and fail when a reader races a writer.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::process::Command;

use rand::RngCore;

pub use janus_source_control::{
    DiffView, GitCredential, GitError, GitLogEntry, GitRunner, GitStatus, UpdateConflictPath,
    UpdateOutcome,
};

/// Convenience constructor for wiring the system adapter through the
/// `Arc<dyn GitRunner>` port at the composition root.
pub fn system_runner() -> Arc<dyn GitRunner> {
    Arc::new(SystemGit)
}

/// System `git` process implementation of `GitRunner`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGit;

impl SystemGit {
    fn base(repo: &Path) -> Command {
        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg(format!("safe.directory={}", safe_directory(repo)))
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
    fn clone<'a>(
        &'a self,
        url: &'a str,
        branch: Option<&'a str>,
        into: &'a Path,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            let parent = into
                .parent()
                .ok_or_else(|| GitError::BadOutput("clone target has no parent dir".into()))?;
            std::fs::create_dir_all(parent).ok();
            let mut command = Command::new("git");
            command
                .arg("-c")
                .arg(format!("safe.directory={}", safe_directory(into)))
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
        })
    }

    fn status<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<GitStatus, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("status").arg("--porcelain=v2").arg("--branch");
            let output = Self::run(&mut command).await?;
            Ok(parse_porcelain_v2(&output))
        })
    }

    fn diff<'a>(
        &'a self,
        repo: &'a Path,
        view: DiffView,
    ) -> BoxFuture<'a, Result<String, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.args(view.args()).arg("--no-color");
            Self::run(&mut command).await
        })
    }

    fn log<'a>(
        &'a self,
        repo: &'a Path,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<GitLogEntry>, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command
                .arg("log")
                .arg(format!("-n{limit}"))
                .arg("--format=%x1e%H%x1f%P%x1f%an%x1f%aI%x1f%s")
                .arg("--numstat");
            let output = Self::run(&mut command).await?;
            Ok(parse_git_log(&output))
        })
    }

    fn branches<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<Vec<String>, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command
                .arg("for-each-ref")
                .arg("--format=%(refname:short)")
                .arg("refs/heads/");
            let output = Self::run(&mut command).await?;
            Ok(output.lines().map(str::to_owned).collect())
        })
    }

    fn remotes<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<Vec<String>, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("remote");
            let output = Self::run(&mut command).await?;
            Ok(output.lines().map(str::to_owned).collect())
        })
    }

    fn fetch<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("fetch").arg("--quiet").arg(remote);
            let askpass = Self::apply_credential(&mut command, credential)?;
            let result = Self::run(&mut command).await;
            if let Some(script) = askpass {
                let _ = std::fs::remove_file(script);
            }
            result?;
            Ok(())
        })
    }

    fn stage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("add").arg("--");
            for path in paths {
                command.arg(path);
            }
            Self::run(&mut command).await?;
            Ok(())
        })
    }

    fn unstage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("reset").arg("HEAD").arg("--");
            for path in paths {
                command.arg(path);
            }
            Self::run(&mut command).await?;
            Ok(())
        })
    }

    fn commit<'a>(
        &'a self,
        repo: &'a Path,
        message: &'a str,
    ) -> BoxFuture<'a, Result<String, GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("commit").arg("--quiet").arg("-m").arg(message);
            Self::run(&mut command).await?;
            // Return the new HEAD sha.
            let mut rev = Self::base(repo);
            rev.arg("rev-parse").arg("HEAD");
            Self::run(&mut rev).await.map(|s| s.trim().to_owned())
        })
    }

    fn push<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
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
        })
    }

    fn update<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<UpdateOutcome, GitError>> {
        Box::pin(async move {
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
                            let base_hash =
                                blob_hash_at(repo, &base_sha, path).await.ok().flatten();
                            let remote_hash =
                                blob_hash_at(repo, &remote_sha, path).await.ok().flatten();
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
        })
    }

    fn checkout<'a>(
        &'a self,
        repo: &'a Path,
        branch: &'a str,
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            let mut command = Self::base(repo);
            command.arg("checkout").arg("--quiet").arg(branch);
            Self::run(&mut command).await?;
            Ok(())
        })
    }

    fn apply_conflict_choice<'a>(
        &'a self,
        repo: &'a Path,
        path: &'a str,
        choice: &'a str,
        remote_hash: Option<&'a str>,
        main_hash: Option<&'a str>,
        edited_bytes: Option<&'a [u8]>,
    ) -> BoxFuture<'a, Result<(), GitError>> {
        Box::pin(async move {
            apply_conflict_choice(repo, path, choice, remote_hash, main_hash, edited_bytes).await
        })
    }

    fn complete_fast_forward<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
    ) -> BoxFuture<'a, Result<String, GitError>> {
        Box::pin(async move { complete_fast_forward(repo, remote, branch).await })
    }
}

fn safe_directory(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
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
        let kind = classify_conflict_kind(
            remote_kind,
            base_hash.as_deref(),
            remote_hash.as_deref(),
            main_hash.as_deref(),
        );
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
        for path in status.working.into_iter().chain(status.untracked) {
            set.insert(path);
        }
    }
    Ok(set)
}

async fn blob_hash_at(repo: &Path, treeish: &str, path: &str) -> Result<Option<String>, GitError> {
    let mut command = SystemGit::base(repo);
    command.arg("ls-tree").arg(treeish).arg("--").arg(path);
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
            let bytes = edited_bytes
                .ok_or_else(|| GitError::BadOutput("edited_text choice requires body".into()))?;
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&abs, bytes)
                .await
                .map_err(|e| GitError::CommandFailed(e.to_string()))?;
            Ok(())
        }
        other => Err(GitError::BadOutput(format!(
            "unknown conflict choice {other}"
        ))),
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
    command.arg("update-ref").arg("HEAD").arg(&upstream);
    SystemGit::run(&mut command).await?;
    // Clear the index and re-add the working tree so status is consistent.
    let mut reset = SystemGit::base(repo);
    reset.arg("read-tree").arg("HEAD");
    let _ = SystemGit::run(&mut reset).await;
    rev_parse(repo, "HEAD").await
}

/// Parse records emitted by `git log --format=... --numstat`.
///
/// Record (`0x1e`) and unit (`0x1f`) separators keep user-authored subjects
/// independent from the numstat lines. Binary changes use `-` for counts and
/// still contribute to `changed_files`.
fn parse_git_log(output: &str) -> Vec<GitLogEntry> {
    output
        .split('\x1e')
        .filter_map(|record| {
            let mut lines = record.trim_matches(['\r', '\n']).lines();
            let header = lines.next()?.trim_end_matches('\r');
            let mut fields = header.splitn(5, '\x1f');
            let sha = fields.next()?;
            let parents = fields.next()?;
            let author = fields.next()?;
            let committed_at = fields.next()?;
            let message = fields.next()?;
            if sha.is_empty() {
                return None;
            }

            let mut changed_files = 0_u64;
            let mut insertions = 0_u64;
            let mut deletions = 0_u64;
            for line in lines {
                let mut numstat = line.trim_end_matches('\r').splitn(3, '\t');
                let Some(added) = numstat.next() else {
                    continue;
                };
                let Some(deleted) = numstat.next() else {
                    continue;
                };
                if numstat.next().is_none() {
                    continue;
                }
                changed_files += 1;
                insertions += added.parse::<u64>().unwrap_or(0);
                deletions += deleted.parse::<u64>().unwrap_or(0);
            }

            Some(GitLogEntry {
                sha: sha.to_owned(),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
                author: author.to_owned(),
                committed_at: committed_at.to_owned(),
                message: message.to_owned(),
                changed_files,
                insertions,
                deletions,
            })
        })
        .collect()
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
            // Unmerged: u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            // path is field 9 (0-based) after the "u " prefix.
            if let Some(path) = rest.split_whitespace().nth(9) {
                working.push(path.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            untracked.push(rest.to_owned());
        } else {
            // Ordinary: 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>  → path @ 7
            // Rename:   2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <score> <path> <orig> → path @ 8
            let (rest, is_rename) = if let Some(rest) = line.strip_prefix("1 ") {
                (rest, false)
            } else if let Some(rest) = line.strip_prefix("2 ") {
                (rest, true)
            } else {
                continue;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let xy = fields.first().copied().unwrap_or("");
            let path = if is_rename {
                fields.get(8).copied().unwrap_or("")
            } else {
                fields.get(7).copied().unwrap_or("")
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
    use super::parse_git_log;
    use super::parse_porcelain_v2;
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

    /// Repro for "save file → git status shows no changes": real `git status
    /// --porcelain=v2 --branch` output from a working tree with modified files
    /// must populate `working`. The `1 .M N...` ordinary-changed entry has
    /// staged==HEAD (x='.') but working differs (y='M'), so it belongs in
    /// `working`, not `index`.
    #[test]
    fn parse_porcelain_v2_detects_working_tree_modifications() {
        let output = "\
# branch.oid 6523bfcfae7ad8b09b5b863def82400191c9578c
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
1 .M N... 100644 100644 100644 2d438b3d37c196e94f40976c89fc8a6f6db946bd 2d438b3d37c196e94f40976c89fc8a6f6db946bd .dockerignore
1 .M N... 100644 100644 100644 4070d6e2fd2e5aa1a6478f5e336fa295e743b79f 4070d6e2fd2e5aa1a6478f5e336fa295e743b79f .npmrc
1 .M N... 100644 100644 100644 ab4a65267255f1799dbfe2c65ccd9713907c49f7 ab4a65267255f1799dbfe2c65ccd9713907c49f7 AGENTS.md
";
        let status = parse_porcelain_v2(output);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(
            status.head_sha.as_deref(),
            Some("6523bfcfae7ad8b09b5b863def82400191c9578c")
        );
        assert_eq!(status.working, vec![".dockerignore", ".npmrc", "AGENTS.md"]);
        assert!(status.index.is_empty());
        assert!(status.untracked.is_empty());
    }

    #[test]
    fn parse_git_log_preserves_topology_and_sums_numstat() {
        let output = concat!(
            "\x1eaaaaaaaa\x1fbbbbbbbb cccccccc\x1fAkiraph\x1f2026-07-24T10:13:00+08:00\x1ffeat: graph details\n\n",
            "2664\t99\tapps/web/src/main.tsx\n",
            "-\t-\tapps/web/public/logo.png\n",
            "\x1ebbbbbbbb\x1f\x1fJanus\x1f2026-07-23T18:00:00+08:00\x1finitial commit\n\n",
            "10\t0\tREADME.md\n",
        );

        let entries = parse_git_log(output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].parents, vec!["bbbbbbbb", "cccccccc"]);
        assert_eq!(entries[0].committed_at, "2026-07-24T10:13:00+08:00");
        assert_eq!(entries[0].changed_files, 2);
        assert_eq!(entries[0].insertions, 2664);
        assert_eq!(entries[0].deletions, 99);
        assert_eq!(entries[1].parents, Vec::<String>::new());
        assert_eq!(entries[1].changed_files, 1);
        assert_eq!(entries[1].insertions, 10);
    }
}
