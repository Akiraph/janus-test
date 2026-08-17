//! Workspace tool path safety: relative, no `..`, and no `.git` paths.

use std::path::{Path, PathBuf};

use janus_workspace::interface::{PathError, validate_workspace_path};

/// Normalize a tool path into the workspace-relative representation used by
/// the mutation API. `/workspace` is a logical absolute alias; every other
/// absolute path remains invalid.
pub fn normalize_workspace_path(raw: &str) -> Result<String, PathError> {
    let relative = match raw.strip_prefix("/workspace") {
        Some("") => ".",
        Some(rest) if rest.starts_with('/') => rest.strip_prefix('/').unwrap_or("."),
        _ => raw,
    };
    if relative.is_empty() || relative == "." {
        return Ok(".".into());
    }
    let path = validate_workspace_path(relative)?;
    Ok(path.to_string_lossy().replace('\\', "/"))
}

/// Resolve a tool-supplied path under the workspace root. Rejects every
/// absolute path except the logical `/workspace` alias, traversal, NUL, and
/// `.git` paths.
pub fn resolve_workspace_path(repo_root: &Path, raw: &str) -> Result<PathBuf, PathError> {
    let normalized = normalize_workspace_path(raw)?;
    let rel = if normalized == "." {
        PathBuf::new()
    } else {
        PathBuf::from(normalized)
    };
    if is_git_component(&rel) {
        return Err(PathError::Invalid);
    }
    Ok(repo_root.join(rel))
}

fn is_git_component(rel: &Path) -> bool {
    rel.components().any(|c| {
        matches!(c.as_os_str().to_str(), Some(".git") | Some(".git\0")) || c.as_os_str() == ".git"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rejects_dotdot_and_git() {
        let root = Path::new("/tmp/session/repo");
        assert!(resolve_workspace_path(root, "../x").is_err());
        assert!(resolve_workspace_path(root, ".git/config").is_err());
        assert!(resolve_workspace_path(root, "src/../.git/x").is_err());
    }

    #[test]
    fn accepts_normal() {
        let root = Path::new("/tmp/session/repo");
        let p = resolve_workspace_path(root, "src/lib.rs").expect("valid path");
        assert!(p.ends_with("src/lib.rs") || p.ends_with(r"src\lib.rs"));
    }

    #[test]
    fn accepts_the_logical_workspace_absolute_prefix() {
        let root = Path::new("/tmp/session/repo");
        let p = resolve_workspace_path(root, "/workspace/src/lib.rs").expect("valid path");
        assert!(p.ends_with("src/lib.rs") || p.ends_with(r"src\lib.rs"));
        assert_eq!(
            normalize_workspace_path("/workspace/src/lib.rs").expect("valid workspace path"),
            "src/lib.rs"
        );
        assert!(resolve_workspace_path(root, "/workspace/../outside").is_err());
        assert!(resolve_workspace_path(root, "/etc/passwd").is_err());
    }
}
