use std::path::Path;
use std::process::Command;

mod diff;
pub mod interface;
mod manifest;
mod path;
mod session_copy;
mod working_tree;

pub(crate) fn git_command(root: &Path) -> Command {
    let safe_directory = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg(format!("safe.directory={safe_directory}"));
    command
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::git_command;

    #[test]
    fn git_command_allows_a_different_repository_owner() {
        let repo = TempDir::new().expect("temporary repository");
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .env("GIT_OPTIONAL_LOCKS", "0")
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        };

        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "janus@local"]);
        run(&["config", "user.name", "Janus"]);
        std::fs::write(repo.path().join("README.md"), b"# test\n").expect("write");
        run(&["add", "README.md"]);
        run(&["commit", "-m", "baseline"]);

        let output = git_command(repo.path())
            .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
            .arg("rev-parse")
            .arg("HEAD")
            .current_dir(repo.path())
            .output()
            .expect("git rev-parse");
        assert!(
            output.status.success(),
            "git rejected the scoped safe.directory: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
