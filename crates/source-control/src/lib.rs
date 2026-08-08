//! Source Control capability.
//!
//! Owns the Git orchestration port (`GitRunner`) and the git state/conflict
//! tables. Reads Project repo config and GitHub credentials from `projects`
//! (read-only) only when resolving a repo to operate on; Project lifecycle
//! states stay owned by `projects`.

pub mod git;
pub mod interface;

pub use git::{system_runner, SystemGit};
pub use interface::{
    DiffView, GitCredential, GitError, GitFuture, GitLogEntry, GitRunner, GitStatus,
    SourceControlError, SourceControlInterface, UpdateConflictPath, UpdateOutcome,
};
