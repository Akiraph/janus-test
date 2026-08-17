//! GitRunner adapter: the system-`git` implementation of the port defined in
//! `janus-source-control`. The implementation lives in the source-control
//! crate; this module is the server's single injection point for the system
//! git executable.

pub use janus_source_control::git::{SystemGit, system_runner};
pub use janus_source_control::{
    DiffView, GitCredential, GitError, GitLogEntry, GitRunner, GitStatus, UpdateConflictPath,
    UpdateOutcome,
};
