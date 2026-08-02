use std::path::Path;
use std::process::Command;

pub fn init_git_repo(path: &Path) -> anyhow::Result<()> {
    for args in [
        &["init", "--initial-branch=main"][..],
        &["config", "user.email", "janus@local"][..],
        &["config", "user.name", "Janus"][..],
        &["add", "-A"][..],
        &["commit", "-m", "main baseline"][..],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    Ok(())
}
