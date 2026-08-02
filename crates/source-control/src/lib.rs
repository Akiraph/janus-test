//! Source Control capability contracts.
//!
//! This crate owns the narrow port used to drive Git. Project metadata,
//! conflict persistence, and operation scheduling remain in their owning
//! capabilities until those boundaries move in a later extraction.

pub mod interface;

pub use interface::{
    DiffView, GitCredential, GitError, GitFuture, GitLogEntry, GitRunner, GitStatus,
    UpdateConflictPath, UpdateOutcome,
};
