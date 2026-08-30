//! System-`git` implementation of the `GitRunner` port.
//!
//! The DTOs and `GitRunner` trait are owned by `crate::interface`; this module
//! implements the system adapter against them. Commands run with
//! `GIT_OPTIONAL_LOCKS=0` and read-only arguments so `status` does not try to
//! refresh the index and fail when a reader races a writer. The server
//! composition root injects `system_runner()` into the capability interfaces.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use rand::RngCore;
use tokio::process::Command;
use url::Url;

use crate::interface::{
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
            Err(classify_failure(&failure_output(
                &output.stderr,
                &output.stdout,
            )))
        }
    }
}

/// A `github.com`-hosted repository identified from a clone URL.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GithubRepo {
    owner: String,
    repo: String,
}

/// Parse a repository URL into its GitHub owner/repo, or `None` when it is not
/// hosted on GitHub. Accepts https/http, the scp-like `git@github.com:o/r`
/// form, ssh/git schemes, and the bare `github.com/o/r` form.
fn github_repo(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    if let Some(path) = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("git@www.github.com:"))
        .or_else(|| url.strip_prefix("github.com/"))
        .or_else(|| url.strip_prefix("www.github.com/"))
    {
        let (owner, repo) = path.split_once('/')?;
        return github_pair(owner, repo);
    }
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.host_str(), Some("github.com") | Some("www.github.com")) {
        return None;
    }
    let mut segments = parsed.path_segments()?;
    let owner = segments.next()?;
    let repo = segments.next()?;
    github_pair(owner, repo)
}

fn github_pair(owner: &str, repo: &str) -> Option<GithubRepo> {
    let owner = owner.trim_end_matches('/');
    let repo = repo.trim_end_matches('/').trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

/// Whether the `aria2c` downloader is on PATH; when missing, the caller falls
/// back to the plain `git clone` path.
async fn aria2c_available() -> bool {
    Command::new("aria2c")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Clone a GitHub repository by downloading its tarball with `aria2c -x16`
/// (16 parallel connections bypass the per-connection cap / stream cut that
/// makes `git clone` fail with fatal 128) and extracting it, then initialize
/// the extracted tree as a fresh git repo so the status/diff/log/commit
/// surface keeps working. The import commit carries no upstream history;
/// fetch/push/update against the remote remain possible but on divergent
/// history.
async fn clone_github_tarball(
    repo: &GithubRepo,
    branch: Option<&str>,
    url: &str,
    into: &Path,
    credential: &GitCredential,
) -> Result<(), GitError> {
    let branch = branch.filter(|value| !value.is_empty());
    let nonce = random_hex();
    let dir = std::env::temp_dir();
    let file_name = format!("janus-github-{nonce}.tar.gz");
    let tarball = dir.join(&file_name);
    let parent = into
        .parent()
        .ok_or_else(|| GitError::BadOutput("clone target has no parent dir".into()))?;
    let extract = parent.join(format!(".janus-github-extract-{nonce}"));
    let scratch = CloneScratch {
        into: into.to_path_buf(),
        extras: vec![tarball.clone(), extract.clone()],
    };

    // Download the branch tarball; HEAD resolves to the default branch.
    let ref_path = match branch {
        Some(branch) => {
            let encoded = encode_branch(branch);
            format!("refs/heads/{encoded}")
        }
        None => "HEAD".to_owned(),
    };
    let owner = repo.owner.as_str();
    let repo_name = repo.repo.as_str();
    // codeload is the redirect target of the github.com/archive URL; hitting it
    // directly keeps basic auth on the right host for private repositories.
    let archive_url = format!("https://codeload.github.com/{owner}/{repo_name}/tar.gz/{ref_path}");

    let mut download = Command::new("aria2c");
    download
        .arg("-x16")
        .arg("-s16")
        .arg("-q")
        .arg("--summary-interval=0")
        .arg("--file-allocation=none")
        .arg("-d")
        .arg(&dir)
        .arg("-o")
        .arg(&file_name);
    if let GitCredential::HttpsBasic { password, .. } = credential {
        // codeload's basic-auth convention is username = PAT, password = the
        // literal "x-oauth-basic"; janus stores the pair the other way around
        // for git's smart-HTTP endpoints, so swap them for the download.
        download.arg("--http-user").arg(password);
        download.arg("--http-passwd").arg("x-oauth-basic");
    }
    download.arg(&archive_url);
    let output = download
        .output()
        .await
        .map_err(|error| GitError::CommandFailed(format!("aria2c: {error}")))?;
    if !output.status.success() {
        return Err(classify_download_failure(&failure_output(
            &output.stderr,
            &output.stdout,
        )));
    }

    std::fs::create_dir_all(&extract).map_err(|error| GitError::CommandFailed(error.to_string()))?;
    let extracted = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&extract)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| GitError::CommandFailed(error.to_string()))?;
    if !extracted.status.success() {
        return Err(classify_download_failure(&failure_output(
            &extracted.stderr,
            &extracted.stdout,
        )));
    }

    // GitHub archives are a single `<repo>-<branch>` directory; its name gives
    // the branch when HEAD was downloaded.
    let top_level = top_level_dir(&extract).await?;
    let branch_name = match branch {
        Some(branch) => branch.to_owned(),
        None => branch_from_folder(&top_level, repo_name).unwrap_or_else(|| "main".to_owned()),
    };

    remove_best_effort(into);
    std::fs::rename(extract.join(&top_level), into)
        .map_err(|error| GitError::CommandFailed(format!("move extracted tree: {error}")))?;

    initialize_import_repo(into, url, &branch_name).await?;
    scratch.disarm();
    Ok(())
}

/// Turn the extracted tree into a working git repo: a default branch holding
/// the full tree as one import commit, with `origin` pointing at the source URL
/// so the existing git surface (status/diff/log/commit) behaves as after a
/// clone. The import identity is set inline so the commit never depends on a
/// global git config being present.
async fn initialize_import_repo(repo_dir: &Path, url: &str, branch: &str) -> Result<(), GitError> {
    let mut init = SystemGit::base(repo_dir);
    init.arg("init").arg("-b").arg(branch);
    SystemGit::run(&mut init).await?;

    let mut remote = SystemGit::base(repo_dir);
    remote.arg("remote").arg("add").arg("origin").arg(url);
    SystemGit::run(&mut remote).await?;

    let mut add = SystemGit::base(repo_dir);
    add.arg("add").arg("-A");
    SystemGit::run(&mut add).await?;

    let mut commit = SystemGit::base(repo_dir);
    commit
        .arg("-c")
        .arg("user.name=Janus")
        .arg("-c")
        .arg("user.email=janus@local.invalid")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("commit")
        .arg("-m")
        .arg(format!("Import from {url}@{branch}"));
    SystemGit::run(&mut commit).await?;
    Ok(())
}

/// Return the single top-level entry name of an extracted GitHub archive.
async fn top_level_dir(extract: &Path) -> Result<String, GitError> {
    let mut entries = tokio::fs::read_dir(extract)
        .await
        .map_err(|error| GitError::CommandFailed(error.to_string()))?;
    let mut names = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| GitError::CommandFailed(error.to_string()))?
    {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    if names.len() != 1 {
        return Err(GitError::BadOutput(format!(
            "GitHub tarball did not contain a single top-level directory (found {names:?})"
        )));
    }
    Ok(names.remove(0))
}

/// GitHub tarball folders are named `<repo>-<branch>`; recover the branch.
fn branch_from_folder(folder: &str, repo: &str) -> Option<String> {
    let branch = folder.strip_prefix(&format!("{repo}-"))?;
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_owned())
    }
}

/// Percent-encode the branch for a URL path segment (a `/` becomes `%2F`).
fn encode_branch(branch: &str) -> String {
    utf8_percent_encode(branch, NON_ALPHANUMERIC).to_string()
}

/// Map an aria2c/tar failure to a `GitError`. GitHub answers 404 for a missing
/// repo and for a repo the credential cannot see (never 403), so that maps to
/// `RepositoryNotFound` like the git adapter reports it.
fn classify_download_failure(output: &str) -> GitError {
    let lower = output.to_ascii_lowercase();
    if lower.contains("404") || lower.contains("not found") || lower.contains("that's an error") {
        GitError::RepositoryNotFound
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("authentication")
        || lower.contains("authorization")
    {
        GitError::AuthFailed
    } else if lower.contains("could not resolve")
        || lower.contains("timed out")
        || lower.contains("connection")
        || lower.contains("dns")
    {
        GitError::RemoteUnavailable
    } else {
        GitError::CommandFailed(output.trim().to_owned())
    }
}

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Remove a file or directory, ignoring "not found" and kind-mismatch errors.
fn remove_best_effort(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
    let _ = std::fs::remove_file(path);
}

/// Removes clone scratch paths on drop. `into` is removed only on failure so a
/// retried clone starts from a clean directory; `disarm` keeps it on success.
struct CloneScratch {
    into: Option<PathBuf>,
    extras: Vec<PathBuf>,
}

impl CloneScratch {
    fn disarm(mut self) {
        self.into = None;
    }
}

impl Drop for CloneScratch {
    fn drop(&mut self) {
        if let Some(into) = &self.into {
            remove_best_effort(into);
        }
        for extra in &self.extras {
            remove_best_effort(extra);
        }
    }
}

/// Pick the stream that carries the reason a git command failed. `git commit`
/// prints "nothing to commit" on stdout and leaves stderr empty, so a
/// stderr-only view classifies the most common commit failure as an opaque
/// process error with no detail at all.
fn failure_output(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(stdout).trim().to_owned()
    } else {
        stderr
    }
}

/// Map a git stderr blob to a normalized `GitError`. Covers the failure modes
/// the public API exposes; unknown failures become `CommandFailed`, which the
/// transport reports as an opaque internal error, so every failure a user can
/// act on needs an arm here.
///
/// Order matters: a failed `git commit` reprints the whole branch summary, which
/// can mention "diverged", so the commit-specific arms must be tested first.
fn classify_failure(output: &str) -> GitError {
    let lower = output.to_ascii_lowercase();
    if lower.contains("index.lock") || lower.contains("another git process seems to be running") {
        GitError::RepositoryLocked
    } else if lower.contains("please tell me who you are")
        || lower.contains("unable to auto-detect email address")
        || lower.contains("empty ident name")
    {
        GitError::IdentityUnset
    } else if lower.contains("nothing to commit")
        || lower.contains("nothing added to commit")
        || lower.contains("no changes added to commit")
    {
        GitError::NothingToCommit
    } else if lower.contains("repository not found") {
        GitError::RepositoryNotFound
    } else if lower.contains("does not appear to be a git repository")
        || lower.contains("no such remote")
    {
        GitError::RemoteNotFound
    } else if lower.contains("couldn't find remote ref")
        || lower.contains("not found in upstream")
        || lower.contains("does not match any")
        || lower.contains("did not match any")
        || lower.contains("unknown revision or path not in the working tree")
    {
        GitError::RefNotFound
    } else if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("permission denied")
        || lower.contains("denied to")
        || lower.contains("403 forbidden")
        || lower.contains("returned error: 403")
        || lower.contains("returned error: 401")
    {
        GitError::AuthFailed
    } else if lower.contains("could not resolve host")
        || lower.contains("connection timed out")
        || lower.contains("failed to connect")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
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
        GitError::CommandFailed(output.trim().to_owned())
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
            // GitHub-hosted repos are fetched as a tarball via `aria2c -x16`:
            // a single `git clone` HTTP stream is cut on the deployment host
            // (fatal 128 / early EOF), while 16 parallel connections complete
            // reliably. When aria2c is unavailable the plain git path runs.
            if let Some(repo) = github_repo(url) {
                if aria2c_available().await {
                    return clone_github_tarball(&repo, branch, url, into, credential).await;
                }
            }
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
                Err(classify_failure(&failure_output(
                    &output.stderr,
                    &output.stdout,
                )))
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
    use super::branch_from_folder;
    use super::classify_failure;
    use super::failure_output;
    use super::github_repo;
    use super::parse_git_log;
    use super::parse_porcelain_v2;
    use crate::interface::GitError;

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

    /// `git commit` with an empty index exits non-zero, prints its reason on
    /// stdout, and leaves stderr empty. The branch summary it reprints can
    /// mention "diverged", which must not outrank the real reason.
    #[test]
    fn classifies_commit_failures_reported_on_stdout() {
        let stdout = concat!(
            "On branch main\n",
            "Your branch and 'origin/main' have diverged.\n",
            "nothing to commit, working tree clean\n",
        );
        assert!(matches!(
            classify_failure(&failure_output(b"", stdout.as_bytes())),
            GitError::NothingToCommit
        ));
        assert!(matches!(
            classify_failure("Author identity unknown\n*** Please tell me who you are."),
            GitError::IdentityUnset
        ));
    }

    #[test]
    fn classifies_recoverable_remote_and_ref_failures() {
        assert!(matches!(
            classify_failure("fatal: 'upstream' does not appear to be a git repository"),
            GitError::RemoteNotFound
        ));
        assert!(matches!(
            classify_failure(
                "remote: Repository not found.\nfatal: repository 'https://host/x' not found"
            ),
            GitError::RepositoryNotFound
        ));
        assert!(matches!(
            classify_failure("fatal: couldn't find remote ref release"),
            GitError::RefNotFound
        ));
        assert!(matches!(
            classify_failure("error: src refspec release does not match any"),
            GitError::RefNotFound
        ));
        assert!(matches!(
            classify_failure("fatal: Unable to create '/repo/.git/index.lock': File exists."),
            GitError::RepositoryLocked
        ));
    }

    /// The auth arm used to match a bare "403", so any sha or path containing
    /// those digits was reported to the user as an authentication failure.
    #[test]
    fn digits_in_a_sha_are_not_an_authentication_failure() {
        assert!(matches!(
            classify_failure("error: could not apply 403f9ab... rework"),
            GitError::CommandFailed(_)
        ));
        assert!(matches!(
            classify_failure("fatal: unable to access 'https://host/x': error: 403 Forbidden"),
            GitError::AuthFailed
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

    #[test]
    fn parses_github_repository_urls() {
        for url in [
            "https://github.com/octo/hello",
            "https://github.com/octo/hello.git",
            "http://www.github.com/octo/hello/",
            "git@github.com:octo/hello.git",
            "ssh://git@github.com/octo/hello",
            "github.com/octo/hello",
        ] {
            let repo = github_repo(url).unwrap_or_else(|| panic!("expected {url} to parse"));
            assert_eq!(repo.owner, "octo");
            assert_eq!(repo.repo, "hello");
        }
    }

    #[test]
    fn rejects_non_github_repository_urls() {
        for url in [
            "https://gitlab.com/octo/hello",
            "https://example.com/octo/hello.git",
            "git@gitlab.com:octo/hello.git",
            "https://github.com/",
            "file:///tmp/repo",
            "not a url",
        ] {
            assert!(github_repo(url).is_none(), "expected {url} to be rejected");
        }
    }

    #[test]
    fn extracts_branch_from_github_archive_folder() {
        assert_eq!(branch_from_folder("hello-main", "hello").as_deref(), Some("main"));
        assert_eq!(branch_from_folder("hello-feature/x", "hello").as_deref(), Some("feature/x"));
        assert_eq!(branch_from_folder("hello", "hello"), None);
        assert_eq!(branch_from_folder("world-main", "hello"), None);
    }
}
