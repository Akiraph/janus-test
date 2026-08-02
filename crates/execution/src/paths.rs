//! Session tool path safety: relative, no `..`, no `.git`, no Main handle.

use std::path::{Path, PathBuf};

use janus_workspace::interface::{PathError, validate_workspace_path};

/// Resolve a tool-supplied relative path under the Session repo root.
/// Rejects absolute / traversal / NUL via `validate_workspace_path`, and
/// refuses any path whose first component is `.git`.
pub fn resolve_session_path(repo_root: &Path, raw: &str) -> Result<PathBuf, PathError> {
    let rel = if raw.is_empty() || raw == "." {
        PathBuf::new()
    } else {
        validate_workspace_path(raw)?
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
        assert!(resolve_session_path(root, "../x").is_err());
        assert!(resolve_session_path(root, ".git/config").is_err());
        assert!(resolve_session_path(root, "src/../.git/x").is_err());
    }

    #[test]
    fn accepts_normal() {
        let root = Path::new("/tmp/session/repo");
        let p = resolve_session_path(root, "src/lib.rs").expect("valid path");
        assert!(p.ends_with("src/lib.rs") || p.ends_with(r"src\lib.rs"));
    }
}
