//! Workspace path resolution.
//!
//! `DAT-FS-01`: the database stores only managed handles relative to the data
//! root, never absolute paths. Opening a workspace path starts from a trusted
//! root, then resolves a relative UTF-8 path beneath it. A naive `canonicalize`
//! followed by a plain open is not enough because it cannot stop a check-time
//! vs use-time symlink swap. M2 enforces the simpler, sufficient guarantee for
//! the Main Workspace: reject absolute paths, `..` traversal, and NUL, and join
//! only beneath the workspace root.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathError {
    #[error("path must be relative and use '/' separators")]
    NotRelative,
    #[error("path must not contain '..' traversal")]
    Traversal,
    #[error("path must not be empty or a device name")]
    Invalid,
    #[error("path contains a NUL byte")]
    Nul,
}

/// Validate a client-supplied UTF-8 workspace-relative path.
///
/// The path must be non-empty, relative, `/` separated, with no leading `/`, no
/// `..` components, no NUL, and no Windows drive letters or leading `\\`.
pub fn validate_workspace_path(raw: &str) -> Result<PathBuf, PathError> {
    if raw.is_empty() {
        return Err(PathError::Invalid);
    }
    if raw.contains('\0') {
        return Err(PathError::Nul);
    }
    if raw.starts_with('/') {
        return Err(PathError::NotRelative);
    }
    // Reject Windows drive prefixes and UNC roots.
    if raw.len() >= 2 && raw.as_bytes()[1] == b':' {
        return Err(PathError::NotRelative);
    }
    if raw.starts_with('\\') {
        return Err(PathError::NotRelative);
    }
    let mut parts: Vec<&str> = Vec::new();
    for component in raw.split('/') {
        match component {
            "" => continue, // tolerate doubled/leading-internal slashes
            "." => continue,
            ".." => return Err(PathError::Traversal),
            other => {
                // Reject path components that escape via Windows reserved behavior
                // is not modeled here beyond the checks above; UTF-8 is required
                // and enforced by the &str type.
                parts.push(other);
            }
        }
    }
    if parts.is_empty() {
        return Err(PathError::Invalid);
    }
    let mut joined = PathBuf::new();
    for part in parts {
        joined.push(part);
    }
    Ok(joined)
}

/// Resolve a validated relative path beneath an absolute workspace root.
pub fn resolve_under(root: &Path, relative: &Path) -> PathBuf {
    root.join(relative)
}

#[cfg(test)]
mod tests {
    use super::{PathError, validate_workspace_path};

    #[test]
    fn simple_relative_paths_are_accepted() {
        assert_eq!(
            validate_workspace_path("src/main.rs").expect("valid path"),
            std::path::PathBuf::from("src/main.rs")
        );
        assert_eq!(
            validate_workspace_path("a/b/c").expect("valid path"),
            std::path::PathBuf::from("a/b/c")
        );
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        assert!(matches!(
            validate_workspace_path("../escape"),
            Err(PathError::Traversal)
        ));
        assert!(matches!(
            validate_workspace_path("a/../../b"),
            Err(PathError::Traversal)
        ));
        assert!(matches!(
            validate_workspace_path("/etc/passwd"),
            Err(PathError::NotRelative)
        ));
    }

    #[test]
    fn empty_nul_and_drive_are_rejected() {
        assert!(matches!(
            validate_workspace_path(""),
            Err(PathError::Invalid)
        ));
        assert!(matches!(
            validate_workspace_path("a\0b"),
            Err(PathError::Nul)
        ));
        assert!(matches!(
            validate_workspace_path("C:/x"),
            Err(PathError::NotRelative)
        ));
    }

    #[test]
    fn dot_components_are_tolerated() {
        assert_eq!(
            validate_workspace_path("./src/./main.rs").expect("valid path"),
            std::path::PathBuf::from("src/main.rs")
        );
        assert_eq!(
            validate_workspace_path("src//main.rs").expect("valid path"),
            std::path::PathBuf::from("src/main.rs")
        );
    }
}
