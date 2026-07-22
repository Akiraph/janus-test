use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;

const MODULES: &[&str] = &[
    "identity",
    "models",
    "projects",
    "runtime",
    "sessions",
    "supervisor",
    "workspace-sync",
];

#[derive(Debug, Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    Setup,
    Dev,
    Check {
        #[command(subcommand)]
        check: Option<CheckTask>,
    },
    Build,
    Generate,
}

#[derive(Debug, Subcommand)]
enum CheckTask {
    Architecture,
}

#[derive(Debug, Deserialize)]
struct ModuleManifest {
    name: String,
    public_root: String,
    owned_tables: Vec<String>,
    publishes: Vec<String>,
    allowed_module_dependencies: Vec<String>,
    specs: Vec<String>,
    tests: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let root = workspace_root()?;
    match Cli::parse().command {
        Task::Setup => setup(&root),
        Task::Dev => dev(&root),
        Task::Check {
            check: Some(CheckTask::Architecture),
        } => check_architecture(&root),
        Task::Check { check: None } => check(&root),
        Task::Build => build(&root),
        Task::Generate => generate(&root),
    }
}

fn setup(root: &Path) -> anyhow::Result<()> {
    for (program, argument) in [
        ("rustc", "--version"),
        ("cargo", "--version"),
        ("bun", "--version"),
        ("git", "--version"),
    ] {
        run(root, program, &[argument])?;
    }
    run(
        root,
        "bun",
        &["install", "--cwd", "apps/web", "--frozen-lockfile"],
    )
}

fn dev(root: &Path) -> anyhow::Result<()> {
    let mut web = Command::new("bun")
        .args(["run", "--cwd", "apps/web", "dev"])
        .current_dir(root)
        .spawn()
        .context("start Vite development server")?;
    let server_status = Command::new("cargo")
        .args(["run", "-p", "janus-server"])
        .current_dir(root)
        .status()
        .context("start Janus server")?;
    let _ = web.kill();
    if !server_status.success() {
        bail!("Janus server exited with {server_status}");
    }
    Ok(())
}

fn check(root: &Path) -> anyhow::Result<()> {
    check_architecture(root)?;
    generate(root)?;
    run(root, "cargo", &["fmt", "--all", "--", "--check"])?;
    run(
        root,
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run(root, "cargo", &["test", "--workspace"])?;
    run(root, "bun", &["run", "--cwd", "apps/web", "typecheck"])?;
    run(root, "bun", &["run", "--cwd", "apps/web", "lint"])?;
    run(root, "bun", &["run", "--cwd", "apps/web", "build"])
}

fn build(root: &Path) -> anyhow::Result<()> {
    generate(root)?;
    run(root, "cargo", &["build", "--workspace", "--release"])?;
    run(root, "bun", &["run", "--cwd", "apps/web", "build"])
}

fn generate(root: &Path) -> anyhow::Result<()> {
    let output = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "-p",
            "janus-server",
            "--bin",
            "generate_openapi",
        ])
        .current_dir(root)
        .output()
        .context("generate OpenAPI from Rust routes")?;
    if !output.status.success() {
        bail!(
            "OpenAPI generation failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let generated = root.join("generated");
    std::fs::create_dir_all(&generated)?;
    std::fs::write(generated.join("openapi.json"), output.stdout)?;
    run(root, "bun", &["run", "--cwd", "apps/web", "generate:types"])
}

fn check_architecture(root: &Path) -> anyhow::Result<()> {
    let modules_root = root.join("apps/server/src/modules");
    let mut found = BTreeSet::new();
    let mut tables = BTreeSet::new();
    let mut events = BTreeSet::new();
    for entry in std::fs::read_dir(&modules_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let manifest_path = path.join("module.toml");
        let manifest: ModuleManifest = toml::from_str(
            &std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("read {}", manifest_path.display()))?,
        )?;
        found.insert(manifest.name.clone());
        if manifest.public_root != "interface.rs" || !path.join(&manifest.public_root).is_file() {
            bail!("{} must expose interface.rs", manifest.name);
        }
        for dependency in &manifest.allowed_module_dependencies {
            validate_dependency(&manifest.name, dependency)?;
        }
        for table in &manifest.owned_tables {
            if !tables.insert(table.clone()) {
                bail!("table {table} has more than one owner");
            }
        }
        for event in &manifest.publishes {
            if !events.insert(event.clone()) {
                bail!("event {event} has more than one publisher");
            }
        }
        let _ = (&manifest.specs, &manifest.tests);
    }
    let expected = MODULES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if found != expected {
        bail!("Module set mismatch: expected {expected:?}, found {found:?}");
    }
    if root.join("apps/server/src/ports").exists() || root.join("crates/ports").exists() {
        bail!("central ports directories are forbidden");
    }

    let migration = std::fs::read_to_string(root.join("apps/server/migrations/0001_platform.sql"))?;
    if !migration
        .lines()
        .take(4)
        .any(|line| line == "-- janus-module: platform")
    {
        bail!("migration is missing its janus-module owner header");
    }
    let test_cli_manifest = std::fs::read_to_string(root.join("tools/test-cli/Cargo.toml"))?;
    if test_cli_manifest.contains("janus-server") {
        bail!("janus-test must not depend on server internals");
    }
    Ok(())
}

fn run(root: &Path, program: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("run {program} {}", args.join(" ")))?;
    if !status.success() {
        bail!("{program} {} exited with {status}", args.join(" "));
    }
    Ok(())
}

fn validate_dependency(module: &str, dependency: &str) -> anyhow::Result<()> {
    if !MODULES.contains(&dependency) {
        bail!("{module} declares unknown Module dependency {dependency}");
    }
    Ok(())
}

fn workspace_root() -> anyhow::Result<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("xtask must live at tools/xtask")?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::validate_dependency;

    #[test]
    fn intentional_unknown_dependency_is_rejected() {
        assert!(validate_dependency("sessions", "internal-shortcut").is_err());
    }
}
