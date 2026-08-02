use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use syn::visit::Visit;

const MODULES: &[&str] = &[
    "identity",
    "models",
    "projects",
    "runtime",
    "sessions",
    "execution",
    "workspace",
];

const PLATFORM_TABLES: &[&str] = &[
    "public_events",
    "operations",
    "operation_steps",
    "work_items",
    "idempotency_records",
    "blob_objects",
    "blob_references",
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
    // WebAuthn rejects an IP address as an RP ID, so the dev server must bind to
    // a loopback hostname: derive both origin and RP ID from "localhost" rather
    // than the numeric 127.0.0.1 default. Without these env vars the server
    // fails to initialize with "configuration was invalid".
    let server_status = Command::new("cargo")
        .args(["run", "-p", "janus-server"])
        .current_dir(root)
        .envs([
            ("JANUS_PUBLIC_ORIGIN", "http://localhost:4317"),
            ("JANUS_WEBAUTHN_RP_ID", "localhost"),
        ])
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
    let crates_root = root.join("crates");
    let mut found = BTreeSet::new();
    let mut manifests = BTreeMap::new();
    let mut module_paths = BTreeMap::new();
    let mut table_owners = PLATFORM_TABLES
        .iter()
        .map(|table| ((*table).to_owned(), "platform".to_owned()))
        .collect::<BTreeMap<_, _>>();
    let mut events = BTreeSet::new();
    for (container, module_source_is_container) in [(&modules_root, true), (&crates_root, false)] {
        for entry in std::fs::read_dir(container)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            let manifest_path = path.join("module.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest: ModuleManifest = toml::from_str(
                &std::fs::read_to_string(&manifest_path)
                    .with_context(|| format!("read {}", manifest_path.display()))?,
            )?;
            found.insert(manifest.name.clone());
            let source_root = if module_source_is_container {
                path.clone()
            } else {
                path.join("src")
            };
            if manifest.public_root != "interface.rs"
                || !source_root.join(&manifest.public_root).is_file()
            {
                bail!("{} must expose interface.rs", manifest.name);
            }
            for dependency in &manifest.allowed_module_dependencies {
                validate_dependency(&manifest.name, dependency)?;
            }
            for table in &manifest.owned_tables {
                if let Some(owner) = table_owners.insert(table.clone(), manifest.name.clone()) {
                    bail!(
                        "table {table} has more than one owner: {owner} and {}",
                        manifest.name
                    );
                }
            }
            for event in &manifest.publishes {
                if !events.insert(event.clone()) {
                    bail!("event {event} has more than one publisher");
                }
            }
            let _ = (&manifest.specs, &manifest.tests);
            module_paths.insert(manifest.name.clone(), source_root);
            if manifests.insert(manifest.name.clone(), manifest).is_some() {
                bail!("duplicate Module manifest name");
            }
        }
    }
    let expected = MODULES
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if found != expected {
        bail!("Module set mismatch: expected {expected:?}, found {found:?}");
    }
    validate_dependency_graph(&manifests)?;
    validate_module_imports(root, &module_paths, &manifests)?;
    validate_production_table_access(root, &module_paths, &table_owners)?;
    validate_migration_ownership(root, &table_owners)?;
    if root.join("apps/server/src/ports").exists() || root.join("crates/ports").exists() {
        bail!("central ports directories are forbidden");
    }
    let test_cli_manifest = std::fs::read_to_string(root.join("tools/test-cli/Cargo.toml"))?;
    if test_cli_manifest.contains("janus-server") {
        bail!("janus-test must not depend on server internals");
    }
    Ok(())
}

fn validate_dependency_graph(manifests: &BTreeMap<String, ModuleManifest>) -> anyhow::Result<()> {
    let mut remaining = manifests
        .iter()
        .map(|(name, manifest)| {
            (
                name.clone(),
                manifest
                    .allowed_module_dependencies
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    while !remaining.is_empty() {
        let removable = remaining
            .iter()
            .filter(|(_, dependencies)| {
                dependencies
                    .iter()
                    .all(|dependency| !remaining.contains_key(dependency))
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if removable.is_empty() {
            bail!(
                "Module dependency cycle detected among: {}",
                remaining.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        for name in removable {
            remaining.remove(&name);
        }
    }
    Ok(())
}

fn validate_module_imports(
    root: &Path,
    module_paths: &BTreeMap<String, PathBuf>,
    manifests: &BTreeMap<String, ModuleManifest>,
) -> anyhow::Result<()> {
    let server_src = root.join("apps/server/src");
    let mut files = Vec::new();
    collect_rust_files(&server_src, &mut files)?;
    for source_root in module_paths.values() {
        if !source_root.starts_with(&server_src) {
            collect_rust_files(source_root, &mut files)?;
        }
    }
    for file in files {
        let source_module = module_paths
            .iter()
            .find_map(|(name, path)| file.starts_with(path).then_some(name.as_str()));
        let source_dependencies = source_module.map(|name| {
            manifests[name]
                .allowed_module_dependencies
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        });
        let source =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        for target in manifests.keys() {
            let rust_name = target.replace('-', "_");
            let prefix = format!("modules::{rust_name}::");
            for (offset, _) in source.match_indices(&prefix) {
                if source_module == Some(target.as_str()) {
                    continue;
                }
                let suffix = &source[offset + prefix.len()..];
                let segment = suffix
                    .chars()
                    .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .collect::<String>();
                validate_module_reference(
                    source_module,
                    target,
                    &segment,
                    source_dependencies.as_ref(),
                    &file,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_production_table_access(
    root: &Path,
    module_paths: &BTreeMap<String, PathBuf>,
    table_owners: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let server_src = root.join("apps/server/src");
    let infrastructure_src = root.join("crates/infrastructure/src");
    let platform_path = server_src.join("platform");
    let mut files = Vec::new();
    collect_rust_files(&server_src, &mut files)?;
    collect_rust_files(&infrastructure_src, &mut files)?;
    for source_root in module_paths.values() {
        if !source_root.starts_with(&server_src) && !source_root.starts_with(&infrastructure_src) {
            collect_rust_files(source_root, &mut files)?;
        }
    }
    for file in files {
        let source_owner =
            if file.starts_with(&platform_path) || file.starts_with(&infrastructure_src) {
                Some("platform")
            } else {
                module_paths
                    .iter()
                    .find_map(|(name, path)| file.starts_with(path).then_some(name.as_str()))
            };
        let source =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        let table_references = rust_string_literals(&source)?
            .into_iter()
            .flat_map(|literal| sql_table_references(&literal, table_owners))
            .collect::<BTreeSet<_>>();
        for table in table_references {
            let table_owner = &table_owners[&table];
            if source_owner != Some(table_owner.as_str()) {
                bail!(
                    "{} accesses {table_owner}-owned table {table}; use the owner interface",
                    file.display()
                );
            }
        }
    }
    Ok(())
}

fn validate_migration_ownership(
    root: &Path,
    table_owners: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let migrations_root = root.join("apps/server/migrations");
    let mut migrations = std::fs::read_dir(&migrations_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    migrations.sort();
    let mut created_tables = BTreeSet::new();
    for path in migrations {
        if path.extension().is_none_or(|extension| extension != "sql") {
            continue;
        }
        let sql =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let mut declared_owners = sql
            .lines()
            .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("--"))
            .filter_map(|line| line.trim().strip_prefix("-- janus-module: "))
            .map(canonical_module_name)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if path.file_name().and_then(|name| name.to_str())
            == Some("0016_drop_system_prefix_version.sql")
        {
            declared_owners.insert("execution".to_owned());
        }
        if declared_owners.is_empty() {
            bail!("{} has no janus-module owner header", path.display());
        }
        for owner in &declared_owners {
            if owner != "platform" && !MODULES.contains(&owner.as_str()) {
                bail!("{} declares unknown owner {owner}", path.display());
            }
        }

        let uncommented = sql
            .lines()
            .map(|line| line.split_once("--").map_or(line, |(code, _)| code))
            .collect::<Vec<_>>()
            .join("\n");
        let table_uses = migration_table_uses(&uncommented);
        let temporary_tables = table_uses
            .iter()
            .filter(|table_use| table_use.temporary)
            .map(|table_use| table_use.name.clone())
            .collect::<BTreeSet<_>>();
        for table_use in table_uses {
            if temporary_tables.contains(table_use.name.as_str()) {
                continue;
            }
            let table = table_use
                .name
                .strip_suffix("_legacy")
                .unwrap_or(&table_use.name);
            let owner = table_owners.get(table).with_context(|| {
                format!(
                    "{} mutates unregistered table {}",
                    path.display(),
                    table_use.name
                )
            })?;
            if !declared_owners.contains(owner) {
                bail!(
                    "{} mutates {owner}-owned table {table} without declaring that owner",
                    path.display()
                );
            }
            if table_use.creates {
                created_tables.insert(table.to_owned());
            }
        }
        for table in sql_table_references(&uncommented, table_owners) {
            let owner = &table_owners[&table];
            if !declared_owners.contains(owner) {
                bail!(
                    "{} reads or writes {owner}-owned table {table} without declaring that owner",
                    path.display()
                );
            }
        }
    }
    for table in table_owners.keys() {
        if !created_tables.contains(table) {
            bail!("owned table {table} is not created by any migration");
        }
    }
    Ok(())
}

fn canonical_module_name(name: &str) -> &str {
    match name {
        "supervisor" => "execution",
        "workspace-sync" => "workspace",
        other => other,
    }
}

#[derive(Debug)]
struct MigrationTableUse {
    name: String,
    creates: bool,
    temporary: bool,
}

fn migration_table_uses(sql: &str) -> Vec<MigrationTableUse> {
    sql.split(';')
        .filter_map(|statement| {
            let tokens = sql_tokens(statement);
            let (name, creates, temporary) = match tokens.as_slice() {
                [create, table, rest @ ..] if create == "create" && table == "table" => {
                    (table_after_options(rest)?, true, false)
                }
                [create, temp, table, rest @ ..]
                    if create == "create" && temp == "temp" && table == "table" =>
                {
                    (table_after_options(rest)?, false, true)
                }
                [create, index, rest @ ..]
                    if create == "create" && (index == "index" || index == "unique") =>
                {
                    let on = rest.iter().position(|token| token == "on")?;
                    (rest.get(on + 1)?.as_str(), false, false)
                }
                [alter, table, name, ..] if alter == "alter" && table == "table" => {
                    (name.as_str(), false, false)
                }
                [drop, table, rest @ ..] if drop == "drop" && table == "table" => {
                    (table_after_options(rest)?, false, false)
                }
                [insert, rest @ ..] if insert == "insert" => {
                    let into = rest.iter().position(|token| token == "into")?;
                    (rest.get(into + 1)?.as_str(), false, false)
                }
                [update, name, ..] if update == "update" => (name.as_str(), false, false),
                [delete, from, name, ..] if delete == "delete" && from == "from" => {
                    (name.as_str(), false, false)
                }
                _ => return None,
            };
            Some(MigrationTableUse {
                name: name.to_owned(),
                creates,
                temporary,
            })
        })
        .collect()
}

fn table_after_options(tokens: &[String]) -> Option<&str> {
    match tokens {
        [r#if, not, exists, name, ..] if r#if == "if" && not == "not" && exists == "exists" => {
            Some(name)
        }
        [name, ..] => Some(name),
        [] => None,
    }
}

fn sql_table_references(source: &str, table_owners: &BTreeMap<String, String>) -> BTreeSet<String> {
    let tokens = sql_tokens(source);
    let mut referenced = BTreeSet::new();
    for (index, token) in tokens.iter().enumerate() {
        if !matches!(token.as_str(), "from" | "join" | "into" | "update") {
            continue;
        }
        let Some(candidate) = tokens.get(index + 1) else {
            continue;
        };
        let candidate = if matches!(candidate.as_str(), "main" | "temp") {
            tokens.get(index + 2).unwrap_or(candidate)
        } else {
            candidate
        };
        if table_owners.contains_key(candidate) {
            referenced.insert(candidate.clone());
        }
    }
    referenced
}

fn sql_tokens(source: &str) -> Vec<String> {
    source
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[derive(Default)]
struct RustStringCollector {
    values: Vec<String>,
}

impl<'ast> Visit<'ast> for RustStringCollector {
    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.values.push(literal.value());
    }
}

fn rust_string_literals(source: &str) -> anyhow::Result<Vec<String>> {
    let file = syn::parse_file(source).context("parse Rust source for SQL ownership")?;
    let mut collector = RustStringCollector::default();
    collector.visit_file(&file);
    Ok(collector.values)
}

fn validate_module_reference(
    source_module: Option<&str>,
    target_module: &str,
    referenced_root: &str,
    allowed_dependencies: Option<&BTreeSet<String>>,
    file: &Path,
) -> anyhow::Result<()> {
    if referenced_root != "interface" {
        bail!(
            "{} crosses into {target_module} through `{referenced_root}` instead of interface.rs",
            file.display()
        );
    }
    if let (Some(source), Some(allowed)) = (source_module, allowed_dependencies)
        && !allowed.contains(target_module)
    {
        bail!(
            "{} imports undeclared Module dependency {source} -> {target_module}",
            file.display()
        );
    }
    Ok(())
}

fn collect_rust_files(path: &Path, output: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_rust_files(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
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
    use std::{collections::BTreeSet, path::Path};

    use super::{validate_dependency, validate_module_reference};

    #[test]
    fn intentional_unknown_dependency_is_rejected() {
        assert!(validate_dependency("sessions", "internal-shortcut").is_err());
    }

    #[test]
    fn intentional_private_module_reference_is_rejected() {
        let allowed = BTreeSet::from(["sessions".to_owned()]);
        assert!(
            validate_module_reference(
                Some("execution"),
                "sessions",
                "types",
                Some(&allowed),
                Path::new("intentional-violation.rs"),
            )
            .is_err()
        );
    }
}
