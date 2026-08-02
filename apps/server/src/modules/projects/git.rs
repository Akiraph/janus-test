//! Git port: the seam through which the Projects domain drives a `git` process.
//!
//! The trait and its DTOs live here in the domain (L2) so that
//! `modules::projects` never depends on `adapters::git` (L4). The system
//! adapter in `adapters::git` implements `GitRunner`; tests substitute a fake
//! runner for crash simulation. Only the port is visible upward.
//!
//! Commands run with `GIT_OPTIONAL_LOCKS=0` and read-only arguments so `status`
//! does not try to refresh the index and fail when a reader races a writer.

use std::path::Path;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    pub(crate) fn args(self) -> &'static [&'static str] {
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
/// substitute a fake for crash simulation only. Methods return boxed futures so
/// the trait is object-safe and the domain can hold a `dyn GitRunner`.
pub trait GitRunner: Send + Sync {
    fn clone<'a>(
        &'a self,
        url: &'a str,
        branch: Option<&'a str>,
        into: &'a Path,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>>;

    fn status<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<GitStatus, GitError>>;

    fn diff<'a>(
        &'a self,
        repo: &'a Path,
        view: DiffView,
    ) -> BoxFuture<'a, Result<String, GitError>>;

    fn log<'a>(
        &'a self,
        repo: &'a Path,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<GitLogEntry>, GitError>>;

    fn branches<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<Vec<String>, GitError>>;

    fn remotes<'a>(&'a self, repo: &'a Path) -> BoxFuture<'a, Result<Vec<String>, GitError>>;

    fn fetch<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>>;

    fn stage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> BoxFuture<'a, Result<(), GitError>>;

    fn unstage<'a>(
        &'a self,
        repo: &'a Path,
        paths: &'a [String],
    ) -> BoxFuture<'a, Result<(), GitError>>;

    fn commit<'a>(
        &'a self,
        repo: &'a Path,
        message: &'a str,
    ) -> BoxFuture<'a, Result<String, GitError>>;

    fn push<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<(), GitError>>;

    fn update<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
        credential: &'a GitCredential,
    ) -> BoxFuture<'a, Result<UpdateOutcome, GitError>>;

    fn checkout<'a>(
        &'a self,
        repo: &'a Path,
        branch: &'a str,
    ) -> BoxFuture<'a, Result<(), GitError>>;

    /// Apply a resolved path choice onto the Main working tree. Used by the
    /// conflict-resolve completion step after all paths have a choice.
    fn apply_conflict_choice<'a>(
        &'a self,
        repo: &'a Path,
        path: &'a str,
        choice: &'a str,
        remote_hash: Option<&'a str>,
        main_hash: Option<&'a str>,
        edited_bytes: Option<&'a [u8]>,
    ) -> BoxFuture<'a, Result<(), GitError>>;

    /// Fast-forward HEAD/index to remote after conflicts are resolved in the
    /// working tree. Caller must ensure the working tree already holds the
    /// merged result and the index is empty or will be reset.
    fn complete_fast_forward<'a>(
        &'a self,
        repo: &'a Path,
        remote: &'a str,
        branch: &'a str,
    ) -> BoxFuture<'a, Result<String, GitError>>;
}
