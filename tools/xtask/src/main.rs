use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use syn::{Expr, punctuated::Punctuated, visit::Visit};

const MODULES: &[&str] = &[
    "identity",
    "models",
    "projects",
    "source-control",
    "runtime",
    "sessions",
    "execution",
    "workspace",
    "notifications",
];

/// Collection methods that mutate documents. A `.collection("...")` chain that
/// ends in one of these is a write access: the calling module must own that
/// collection (platform and the infrastructure crate are exempt).
const WRITE_COLLECTION_METHODS: &[&str] = &[
    "insert_one",
    "insert_many",
    "update_one",
    "update_many",
    "replace_one",
    "delete_one",
    "delete_many",
    "find_one_and_update",
    "find_one_and_delete",
    "find_one_and_replace",
    "create_indexes",
    "drop",
];

/// Collection methods that only read. Cross-module read-only access is the
/// documented data boundary (无 Cargo 依赖); only writes are ownership checks.
const READ_COLLECTION_METHODS: &[&str] = &[
    "find",
    "find_one",
    "count_documents",
    "distinct",
    "aggregate",
];

/// Helper call names inside `schema.rs::index_specs`. The first argument of
/// each call is an index name; these must be globally unique.
const INDEX_HELPERS: &[&str] = &[
    "index",
    "unique_index",
    "partial_index",
    "unique_partial_index",
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
    owned_collections: Vec<String>,
    publishes: Vec<String>,
    allowed_module_dependencies: Vec<String>,
}

/// Collection catalog parsed from `crates/infrastructure/src/schema.rs`. That
/// file is the single source of truth for which collections exist and who owns
/// them; `module.toml` `owned_collections` entries must agree with it exactly.
#[derive(Debug)]
struct SchemaCatalog {
    /// `(collection name, owning module)`. Owner is `platform` for collections
    /// owned by infrastructure + server core.
    collections: Vec<(String, String)>,
    /// Collections with no custom index (their `_id` index suffices).
    indexless: BTreeSet<String>,
    /// Collections that carry at least one custom index.
    indexed: BTreeSet<String>,
    /// Every custom index name declared in `index_specs`.
    index_names: Vec<String>,
}

impl SchemaCatalog {
    fn owners(&self) -> BTreeMap<String, String> {
        self.collections.iter().cloned().collect()
    }
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
    let bind = env::var("JANUS_BIND").unwrap_or_else(|_| "127.0.0.1:4317".into());
    let server_port = bind
        .parse::<std::net::SocketAddr>()
        .map(|address| address.port())
        .unwrap_or(4317);
    let web_port = env::var("JANUS_WEB_PORT").unwrap_or_else(|_| "5173".into());
    let api_target =
        env::var("JANUS_API_TARGET").unwrap_or_else(|_| format!("http://127.0.0.1:{server_port}"));
    let public_origin = env::var("JANUS_PUBLIC_ORIGIN")
        .unwrap_or_else(|_| format!("http://localhost:{server_port}"));
    let webauthn_rp_id = env::var("JANUS_WEBAUTHN_RP_ID").unwrap_or_else(|_| "localhost".into());
    let mut web = Command::new("bun")
        .args(["run", "--cwd", "apps/web", "dev"])
        .current_dir(root)
        .env("JANUS_WEB_PORT", &web_port)
        .env("JANUS_API_TARGET", &api_target)
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
            ("JANUS_BIND", bind.as_str()),
            ("JANUS_PUBLIC_ORIGIN", public_origin.as_str()),
            ("JANUS_WEBAUTHN_RP_ID", webauthn_rp_id.as_str()),
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
    let mut collection_claims = BTreeMap::new();
    let mut events = BTreeSet::new();
    for (container, module_source_is_container) in [(&modules_root, true), (&crates_root, false)] {
        if !container.is_dir() {
            continue;
        }
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
            let public_root = source_root.join(&manifest.public_root);
            let root_valid = matches!(
                manifest.public_root.as_str(),
                "interface.rs" | "interface/mod.rs"
            ) && (public_root.is_file() || public_root.is_dir());
            if !root_valid {
                bail!(
                    "{} must expose interface.rs or interface/mod.rs",
                    manifest.name
                );
            }
            for dependency in &manifest.allowed_module_dependencies {
                validate_dependency(&manifest.name, dependency)?;
            }
            for collection in &manifest.owned_collections {
                if let Some(owner) =
                    collection_claims.insert(collection.clone(), manifest.name.clone())
                {
                    bail!(
                        "collection {collection} has more than one owner: {owner} and {}",
                        manifest.name
                    );
                }
            }
            for event in &manifest.publishes {
                if !events.insert(event.clone()) {
                    bail!("event {event} has more than one publisher");
                }
            }
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
    let catalog = parse_schema_catalog(root)?;
    validate_schema_declarations(&catalog, &collection_claims)?;
    validate_production_collection_access(root, &module_paths, &catalog.owners())?;
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

/// Read the collection catalog from `crates/infrastructure/src/schema.rs`. The
/// `COLLECTIONS`/`INDEXLESS_COLLECTIONS` consts are parsed with syn; the index
/// names and indexed collections are read from the `index_specs()` fn body,
/// whose `vec!` macros wrap the helper calls.
fn parse_schema_catalog(root: &Path) -> anyhow::Result<SchemaCatalog> {
    let path = root.join("crates/infrastructure/src/schema.rs");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let file = syn::parse_file(&source).with_context(|| format!("parse {}", path.display()))?;
    let mut collections = Vec::new();
    let mut indexless = BTreeSet::new();
    let mut indexed = BTreeSet::new();
    let mut index_names = Vec::new();

    for item in &file.items {
        let syn::Item::Const(const_item) = item else {
            continue;
        };
        let ident = const_item.ident.to_string();
        let array = match &const_item.expr {
            Expr::Reference(reference) => match &*reference.expr {
                Expr::Array(array) => array,
                _ => bail!("{}: {ident} must be an array reference", path.display()),
            },
            Expr::Array(array) => array,
            _ => bail!("{}: {ident} must be an array", path.display()),
        };
        match ident.as_str() {
            "COLLECTIONS" => {
                for element in &array.elems {
                    let Expr::Tuple(tuple) = element else {
                        bail!(
                            "{}: COLLECTIONS must be (name, owner) tuples",
                            path.display()
                        );
                    };
                    let name = string_value(&tuple.elems[0], &path)?;
                    let owner = string_value(&tuple.elems[1], &path)?;
                    collections.push((name, owner));
                }
            }
            "INDEXLESS_COLLECTIONS" => {
                for element in &array.elems {
                    indexless.insert(string_value(element, &path)?);
                }
            }
            _ => {}
        }
    }

    let Some(syn::Item::Fn(index_specs)) = file
        .items
        .iter()
        .find(|item| matches!(item, syn::Item::Fn(func) if func.sig.ident == "index_specs"))
    else {
        bail!("{}: missing index_specs()", path.display());
    };
    let Some(syn::Stmt::Expr(top, _)) = index_specs.block.stmts.last() else {
        bail!(
            "{}: index_specs() must end in an expression",
            path.display()
        );
    };
    let Expr::Macro(outer) = top else {
        bail!("{}: index_specs() must return a vec![...]", path.display());
    };
    let collection_tuples = outer
        .mac
        .parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .with_context(|| format!("{}: parse index_specs vec", path.display()))?;
    for tuple_expr in collection_tuples {
        let Expr::Tuple(tuple) = tuple_expr else {
            bail!(
                "{}: index_specs entries must be (collection, vec) tuples",
                path.display()
            );
        };
        let collection = string_value(&tuple.elems[0], &path)?;
        let Expr::Macro(index_vec) = &tuple.elems[1] else {
            bail!(
                "{}: index_specs entry {collection} must map to vec![...]",
                path.display()
            );
        };
        let index_exprs = index_vec
            .mac
            .parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated)
            .with_context(|| format!("{}: parse indexes for {collection}", path.display()))?;
        for index_expr in index_exprs {
            let Expr::Call(call) = index_expr else {
                continue;
            };
            let Expr::Path(func) = &*call.func else {
                continue;
            };
            let Some(helper) = func.path.get_ident().map(|ident| ident.to_string()) else {
                continue;
            };
            if INDEX_HELPERS.contains(&helper.as_str()) {
                index_names.push(string_value(&call.args[0], &path)?);
            }
        }
        indexed.insert(collection);
    }

    Ok(SchemaCatalog {
        collections,
        indexless,
        indexed,
        index_names,
    })
}

/// Cross-check `module.toml` collection claims against the schema catalog.
fn validate_schema_declarations(
    catalog: &SchemaCatalog,
    claims: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for (collection, module) in claims {
        match catalog
            .collections
            .iter()
            .find(|(name, _)| name == collection)
        {
            None => bail!(
                "module {module} claims unknown collection {collection}; not in schema.rs COLLECTIONS"
            ),
            Some((_, owner)) if owner != module => bail!(
                "schema.rs declares collection {collection} owned by {owner} but {module}/module.toml claims it"
            ),
            _ => {}
        }
    }
    for (collection, owner) in &catalog.collections {
        if owner == "platform" {
            if let Some(module) = claims.get(collection) {
                bail!("platform collection {collection} is claimed by {module}/module.toml");
            }
        } else if claims.get(collection).map(String::as_str) != Some(owner.as_str()) {
            bail!(
                "schema.rs declares collection {collection} owned by {owner} but it is unclaimed or claimed by another module"
            );
        }
    }
    let mut seen = BTreeSet::new();
    for name in &catalog.index_names {
        if !seen.insert(name.clone()) {
            bail!("schema.rs declares duplicate index name {name}");
        }
    }
    for (collection, _) in &catalog.collections {
        let is_indexed = catalog.indexed.contains(collection);
        let is_indexless = catalog.indexless.contains(collection);
        if is_indexed == is_indexless {
            bail!("schema.rs collection {collection} must be exactly one of indexed or indexless");
        }
    }
    Ok(())
}

/// Every `.collection("...")` write chain must live in the owning module.
/// Reads across module boundaries remain allowed (the documented data
/// boundary). Collection names must be inline string literals so the ownership
/// pass can trace them; binding a collection handle to a variable defeats that.
fn validate_production_collection_access(
    root: &Path,
    module_paths: &BTreeMap<String, PathBuf>,
    collection_owners: &BTreeMap<String, String>,
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
        let parsed = syn::parse_file(&source)
            .with_context(|| format!("parse {} for collection ownership", file.display()))?;
        let mut collector = CollectionAccessCollector::default();
        collector.visit_file(&parsed);
        if source_owner != Some("platform") {
            for span in &collector.non_literal_spans {
                bail!(
                    "{}:{} collection() must take an inline string literal",
                    file.display(),
                    span.start().line
                );
            }
            for span in &collector.bound_collection_spans {
                bail!(
                    "{}:{} bind a collection handle to a variable; call .collection(\"...\") inline",
                    file.display(),
                    span.start().line
                );
            }
        }
        for (collection, is_write) in collector.accesses {
            let owner = collection_owners.get(&collection).with_context(|| {
                format!(
                    "{} accesses unregistered collection {collection}",
                    file.display()
                )
            })?;
            if is_write && source_owner != Some(owner.as_str()) {
                bail!(
                    "{} writes {owner}-owned collection {collection}; use the owner interface",
                    file.display()
                );
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct CollectionAccessCollector {
    /// `(collection, is_write)` pairs detected from `.collection(...)` chains.
    accesses: BTreeSet<(String, bool)>,
    /// Spans of `.collection(` calls whose argument is not a string literal.
    non_literal_spans: Vec<syn::Span>,
    /// Spans of `let name = ...collection("...")` bindings of a bare handle.
    bound_collection_spans: Vec<syn::Span>,
}

impl<'ast> Visit<'ast> for CollectionAccessCollector {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if has_test_only_cfg(&item.attrs) {
            return;
        }
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if has_test_only_cfg(&item.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, item);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if let syn::Stmt::Local(local) = stmt
            && !matches!(local.pat, syn::Pat::Wild(_))
            && let Some((init, _)) = &local.init
            && is_bare_collection_call(init)
        {
            self.bound_collection_spans.push(local.pat.span());
        }
        syn::visit::visit_stmt(self, stmt);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if method == "collection" {
            if literal_string_arg(&node.args).is_none() {
                self.non_literal_spans.push(node.span());
            }
        } else if let Some(collection) = collection_in_receiver_spine(&node.receiver) {
            if WRITE_COLLECTION_METHODS.contains(&method.as_str()) {
                self.accesses.insert((collection, true));
            } else if READ_COLLECTION_METHODS.contains(&method.as_str()) {
                self.accesses.insert((collection, false));
            }
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// `true` when `expr` is a `.collection("...")` chain with no operation applied
/// to the handle (i.e. the value being bound *is* the collection, not a result).
fn is_bare_collection_call(expr: &Expr) -> bool {
    match expr {
        Expr::Try(try_) => is_bare_collection_call(&try_.expr),
        Expr::Await(await_) => is_bare_collection_call(&await_.base),
        Expr::Paren(paren) => is_bare_collection_call(&paren.expr),
        Expr::Reference(reference) => is_bare_collection_call(&reference.expr),
        Expr::MethodCall(call) => {
            matches!(&call.method, syn::Member::Named(name) if name == "collection")
        }
        _ => false,
    }
}

/// Walk a method receiver spine and return the collection name if the spine
/// ends in a `.collection("literal")` call.
fn collection_in_receiver_spine(expr: &Expr) -> Option<String> {
    match expr {
        Expr::MethodCall(call) => {
            if matches!(&call.method, syn::Member::Named(name) if name == "collection") {
                return literal_string_arg(&call.args);
            }
            collection_in_receiver_spine(&call.receiver)
        }
        Expr::Try(try_) => collection_in_receiver_spine(&try_.expr),
        Expr::Await(await_) => collection_in_receiver_spine(&await_.base),
        Expr::Paren(paren) => collection_in_receiver_spine(&paren.expr),
        Expr::Reference(reference) => collection_in_receiver_spine(&reference.expr),
        _ => None,
    }
}

fn literal_string_arg(args: &syn::punctuated::Punctuated<Expr, syn::Token![,]>) -> Option<String> {
    let first = args.first()?;
    if let Expr::Lit(literal) = first
        && let syn::Lit::Str(value) = &literal.lit
    {
        Some(value.value())
    } else {
        None
    }
}

fn string_value(expr: &Expr, path: &Path) -> anyhow::Result<String> {
    if let Expr::Lit(literal) = expr
        && let syn::Lit::Str(value) = &literal.lit
    {
        Ok(value.value())
    } else {
        bail!("{}: expected a string literal", path.display())
    }
}

fn has_test_only_cfg(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        let Ok(list) = attribute.meta.require_list() else {
            return false;
        };
        let Ok(expressions) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            return false;
        };
        expressions.len() == 1 && cfg_expression_is_test_only(&expressions[0])
    })
}

fn cfg_expression_is_test_only(expression: &syn::Meta) -> bool {
    match expression {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => {
            let Ok(expressions) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            !expressions.is_empty() && expressions.iter().all(cfg_expression_is_test_only)
        }
        _ => false,
    }
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

    use super::{
        CollectionAccessCollector, collection_in_receiver_spine, validate_dependency,
        validate_module_reference,
    };

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

    #[test]
    fn literal_write_chain_is_recorded_as_write() {
        let source = r#"
            async fn touch(pool: &Database) -> anyhow::Result<()> {
                pool.collection::<mongodb::bson::Document>("owners")
                    .update_one(doc! {"_id": "x"}, doc! {"$set": {"a": 1}})
                    .await?;
                Ok(())
            }
        "#;
        let file = syn::parse_file(source).expect("fixture parses");
        let mut collector = CollectionAccessCollector::default();
        collector.visit_file(&file);
        assert!(collector.accesses.contains(&("owners".to_owned(), true)));
        assert!(collector.non_literal_spans.is_empty());
        assert!(collector.bound_collection_spans.is_empty());
    }

    #[test]
    fn non_literal_collection_argument_is_flagged() {
        let source = r#"
            fn get(pool: &Database, name: &str) {
                let _ = pool.collection::<mongodb::bson::Document>(name);
            }
        "#;
        let file = syn::parse_file(source).expect("fixture parses");
        let mut collector = CollectionAccessCollector::default();
        collector.visit_file(&file);
        assert_eq!(collector.non_literal_spans.len(), 1);
    }

    #[test]
    fn bare_collection_handle_binding_is_flagged() {
        let source = r#"
            fn keep(pool: &Database) {
                let coll = pool.collection::<mongodb::bson::Document>("owners");
                let _ = coll;
            }
        "#;
        let file = syn::parse_file(source).expect("fixture parses");
        let mut collector = CollectionAccessCollector::default();
        collector.visit_file(&file);
        assert_eq!(collector.bound_collection_spans.len(), 1);
    }

    #[test]
    fn test_only_collection_chains_are_excluded() {
        let source = r#"
            #[cfg(test)]
            mod tests {
                fn helper(pool: &Database) {
                    let _ = pool.collection::<mongodb::bson::Document>("owners")
                        .insert_one(doc! {})
                        .await;
                }
            }
        "#;
        let file = syn::parse_file(source).expect("fixture parses");
        let mut collector = CollectionAccessCollector::default();
        collector.visit_file(&file);
        assert!(collector.accesses.is_empty());
    }

    #[test]
    fn receiver_spine_finds_collection_through_chaining() {
        let expr = syn::parse_quote!(
            pool.collection::<Document>("sessions")
                .find(doc! {})
                .sort(doc! {})
        );
        assert_eq!(
            collection_in_receiver_spine(&expr),
            Some("sessions".to_owned())
        );
    }
}
