//! Public Source Control port.
//!
//! The port deliberately knows about Git protocol values and repository paths,
//! but not about Projects tables, operations, HTTP, or the system process. The
//! server composition root supplies an adapter and capability code persists
//! the resulting projections and conflicts.

use std::{future::Future, path::Path, pin::Pin};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Boxed future used by the object-safe Git port.
pub type GitFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Normalized Git failure mapped by the transport to stable `GIT_*` codes.
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

/// A credential passed to clone/fetch/push. Implementations must keep the
/// password out of Git configuration and process arguments.
#[derive(Debug, Clone)]
pub enum GitCredential {
    None,
    /// `(username, password)` for HTTPS basic auth. The password is the PAT.
    HttpsBasic {
        username: String,
        password: String,
    },
}

/// Three-layer status projection (`git/status`).
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
    /// Return the process arguments for the system adapter.
    pub fn args(self) -> &'static [&'static str] {
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
    pub committed_at: String,
    pub message: String,
    pub changed_files: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// A path that collides between local edits and an incoming remote update.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateConflictPath {
    pub path: String,
    pub kind: String,
    pub base_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub main_hash: Option<String>,
}

/// Result of a three-way update. Conflict persistence belongs to the caller.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// HEAD advanced to the remote tip; working-tree-only edits were preserved.
    FastForward {
        new_head: String,
        base_tree: String,
        remote_tree: String,
    },
    Failed(GitError),
    /// Main was left unchanged. The caller persists the conflict rows.
    Conflict {
        paths: Vec<UpdateConflictPath>,
        base_tree: String,
        remote_tree: String,
        main_tree: String,
        head_sha: String,
        remote_sha: String,
    },
}

/// Object-safe Git port implemented by the system adapter or a test double.
pub trait GitRunner: Send + Sync {
    fn clone<'a>(
        &'a self,
        url: &'a str,
        branch: Option<&'a str>,
        into: &'a Path,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn status<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<GitStatus, GitError>>;

    fn diff<'a>(
        &'a self,
        repo: &'a Path,
        view: DiffView,
    ) -> GitFuture<'a, Result<String, GitError>>;

    fn log<'a>(
        &'a self,
        repo: &'a Path,
        limit: u32,
    ) -> GitFuture<'a, Result<Vec<GitLogEntry>, GitError>>;

    fn branches<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<Vec<String>, GitError>>;

    fn remotes<'a>(&'a self, repo: &'a Path) -> GitFuture<'a, Result<Vec<String>, GitError>>;

    fn fetch<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn stage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn unstage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn commit<'a>(
        &'a self,
        repo: &'a Path,
        message: &'a str,
    ) -> GitFuture<'a, Result<String, GitError>>;

    fn push<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<(), GitError>>;

    fn update<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> GitFuture<'a, Result<UpdateOutcome, GitError>>;

    fn checkout<'a>(
        &'a self,
        repo: &'a Path,
        branch: &'a str,
    ) -> GitFuture<'a, Result<(), GitError>>;

    /// Apply a resolved conflict choice to the Main working tree.
    fn apply_conflict_choice<'a>(
        &'a self,
        repo: &'a Path,
        path: &'a str,
        choice: &'a str,
        remote_hash: Option<&'a str>,
        main_hash: Option<&'a str>,
        edited_bytes: Option<&'a [u8]>,
    ) -> GitFuture<'a, Result<(), GitError>>;

    /// Fast-forward HEAD/index after the working tree contains the resolution.
    fn complete_fast_forward<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
    ) -> GitFuture<'a, Result<String, GitError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        let cases = [
            (GitError::AuthFailed, "GIT_AUTH_FAILED"),
            (GitError::RemoteUnavailable, "GIT_REMOTE_UNAVAILABLE"),
            (GitError::NonFastForward, "GIT_NON_FAST_FORWARD"),
            (GitError::IndexNotEmpty, "GIT_INDEX_NOT_EMPTY"),
            (GitError::Diverged, "GIT_DIVERGED"),
            (GitError::CheckoutConflict, "GIT_CHECKOUT_CONFLICT"),
            (
                GitError::UpdateConflict { paths: Vec::new() },
                "GIT_UPDATE_CONFLICT",
            ),
            (GitError::CommandFailed("exit 1".into()), "INTERNAL_ERROR"),
            (GitError::BadOutput("invalid".into()), "INTERNAL_ERROR"),
        ];

        for (error, code) in cases {
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn diff_views_map_to_read_only_git_arguments() {
        assert_eq!(DiffView::WorkingVsIndex.args(), ["diff"]);
        assert_eq!(DiffView::IndexVsHead.args(), ["diff", "--cached"]);
        assert_eq!(DiffView::WorkingVsHead.args(), ["diff", "HEAD"]);
    }
}
