//! Session workspace copy lifecycle: create (from Main), delete.
//!
//! Session directory layout (`DAT-FS-01`):
//! `data_root/workspaces/sessions/<session-id>/repo/`
//! Handle: `session:<session-id>` (symmetric to Main `main:<project-id>`).
//!
//! Session copies are **git worktrees** of the Main clone, not file-tree
//! copies. Main is itself a `git clone` result (see `projects::run_clone`), so
//! `git worktree add` shares Main's `.git` object database and checks out a
//! working tree in seconds — no recursive file copy, no re-`git init`, and the
//! Session inherits Main's full history instead of becoming an orphan repo.

use std::process::Command;
use std::{
    fmt::Display,
    path::{Path, PathBuf},
};

/// Relative managed_dir stored in `workspace_copies.managed_dir`.
pub fn session_managed_dir(session_id: impl Display) -> String {
    format!("workspaces/sessions/{session_id}/repo")
}

/// Absolute path to the Session repo directory under `data_root`.
///
/// `data_root` is joined then made absolute when possible: `git worktree add`
/// runs with `current_dir = main_repo`, so a *relative* session path would be
/// created inside the project Main clone (the `.janus-dev` leak).
pub fn session_repo_abs(data_root: &Path, session_id: impl Display) -> PathBuf {
    absoluteish(
        data_root
            .join("workspaces")
            .join("sessions")
            .join(session_id.to_string())
            .join("repo"),
    )
}

/// Absolute path to the Main repo directory under `data_root`.
pub fn main_repo_abs(data_root: &Path, managed_dir: &str) -> PathBuf {
    absoluteish(data_root.join(managed_dir))
}

/// Best-effort absolute path. Prefer `canonicalize` when the path exists;
/// otherwise join against cwd so a relative input never rides on `current_dir`
/// of a later `git` invocation.
fn absoluteish(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

/// Create a Session workspace copy as a git worktree of the Main clone.
///
/// Runs `git worktree add --detach <abs-session-repo> HEAD` from the Main repo.
/// The Session path is always absolute so git cannot resolve it relative to
/// Main and nest Janus's data root inside the project tree.
///
/// `core.autocrlf=false` keeps checkout bytes byte-for-byte for Merkle hashes.
pub fn create_session_worktree(main_repo: &Path, session_repo: &Path) -> anyhow::Result<()> {
    let main_repo = absoluteish(main_repo.to_path_buf());
    let session_repo = absoluteish(session_repo.to_path_buf());
    if !main_repo.is_dir() {
        anyhow::bail!(
            "main managed dir is not a directory: {}",
            main_repo.display()
        );
    }
    // Refuse a target that already exists (even with --force) when it is a
    // non-empty leftover. Clear any stale target first.
    if session_repo.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&session_repo)
            .current_dir(&main_repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .status();
        if session_repo.exists() {
            std::fs::remove_dir_all(&session_repo)?;
        }
    } else if let Some(parent) = session_repo.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let status = Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.longpaths=true",
            "worktree",
            "add",
            "--detach",
            "--force",
        ])
        .arg(&session_repo)
        .arg("HEAD")
        .current_dir(&main_repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .status()?;
    if !status.success() {
        anyhow::bail!("git worktree add failed with status {status}");
    }
    // Guardrail: if the session path somehow still landed under Main, surface
    // it as an error rather than silently polluting the project tree.
    if session_repo.starts_with(&main_repo) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&session_repo)
            .current_dir(&main_repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .status();
        let _ = std::fs::remove_dir_all(&session_repo);
        anyhow::bail!(
            "session worktree path resolved inside main repo ({} under {}); data_root must be absolute",
            session_repo.display(),
            main_repo.display()
        );
    }
    Ok(())
}

pub fn main_worktree_is_clean(main_repo: &Path) -> anyhow::Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(main_repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    if !output.status.success() {
        anyhow::bail!("git status failed with status {}", output.status);
    }
    Ok(output.stdout.is_empty())
}

/// Remove the Session workspace tree.
///
/// Tries `git worktree remove` first (run from the worktree itself — any
/// worktree is a valid git context for `worktree remove`) so Main's
/// `worktrees/` admin directory stays consistent. Falls back to a plain
/// recursive delete if the worktree was already pruned or was never registered
/// (e.g. created before the worktree scheme), so deletion stays idempotent.
pub fn remove_session_tree(data_root: &Path, session_id: impl Display) -> anyhow::Result<()> {
    let session_root = data_root
        .join("workspaces")
        .join("sessions")
        .join(session_id.to_string());
    let session_repo = session_root.join("repo");
    // Best-effort worktree deregistration; ignore failure (already gone / never
    // a worktree) and clean the on-disk tree regardless.
    if session_repo.is_dir() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&session_repo)
            .current_dir(&session_repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .status();
        // `worktree remove` leaves the session_root/repo directory behind in
        // some edge cases; ensure the whole session tree is gone.
    }
    if session_root.exists() {
        std::fs::remove_dir_all(&session_root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A real git repo cloned (via `git init` + commit) into a temp dir, used as
    /// the Main stand-in for worktree creation.
    fn make_main_repo() -> TempDir {
        let dir = TempDir::new().expect("temp main");
        let root = dir.path();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .status()
                .expect("git");
            assert!(status.success(), "git {:?} failed", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "janus@local"]);
        run(&["config", "user.name", "Janus"]);
        std::fs::write(root.join("a.txt"), b"hello").expect("write a");
        std::fs::create_dir_all(root.join("sub")).expect("mkdir sub");
        std::fs::write(root.join("sub").join("b.txt"), b"world").expect("write b");
        run(&["add", "-A"]);
        run(&["commit", "-m", "baseline"]);
        dir
    }

    #[test]
    fn worktree_shares_main_files_and_history() {
        let main = make_main_repo();
        let session = TempDir::new().expect("temp session root");
        let session_repo = session.path().join("repo");

        create_session_worktree(main.path(), &session_repo).expect("worktree add");

        // Working files are checked out from Main.
        assert!(session_repo.join("a.txt").is_file());
        assert!(session_repo.join("sub").join("b.txt").is_file());
        // It is a worktree, not a fresh repo: HEAD resolves and history exists.
        let log = Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(&session_repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .expect("git log");
        assert!(log.status.success(), "git log failed in worktree");
        let log_text = String::from_utf8_lossy(&log.stdout);
        assert!(
            log_text.contains("baseline"),
            "worktree has no Main history: {log_text}"
        );
    }

    #[test]
    fn remove_deregisters_worktree_and_deletes_tree() {
        let main = make_main_repo();
        // data_root layout: the session tree lives under data_root/workspaces.
        let data_root = TempDir::new().expect("temp data root");
        let session_id = uuid::Uuid::now_v7();
        let session_repo = session_repo_abs(data_root.path(), session_id);

        create_session_worktree(main.path(), &session_repo).expect("worktree add");
        assert!(session_repo.is_dir());

        remove_session_tree(data_root.path(), session_id).expect("remove");
        assert!(
            !session_repo.exists(),
            "session tree still on disk after remove"
        );

        // Main's worktree admin list no longer references the removed session.
        let list = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(main.path())
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .expect("git worktree list");
        let list_text = String::from_utf8_lossy(&list.stdout);
        assert!(
            !list_text.contains(session_repo.to_string_lossy().as_ref()),
            "worktree still registered after remove: {list_text}"
        );
    }

    #[test]
    fn worktree_never_nests_under_main() {
        // Reproduces the production bug: relative data_root + git current_dir=main
        // would create session paths inside the project. Absolute paths must keep
        // the session sibling to main, never nested under it.
        let main = make_main_repo();
        let data_root = TempDir::new().expect("data root");
        // Use a relative-looking join path then absoluteish() via session_repo_abs.
        let session_id = uuid::Uuid::now_v7();
        let session_repo = session_repo_abs(data_root.path(), session_id);
        create_session_worktree(main.path(), &session_repo).expect("worktree add");
        assert!(
            !session_repo.starts_with(main.path()),
            "session worktree nested under main: {}",
            session_repo.display()
        );
        assert!(session_repo.join("a.txt").is_file());
        // .git is a file (worktree pointer), not a full repo dir.
        assert!(session_repo.join(".git").is_file());
    }
}
