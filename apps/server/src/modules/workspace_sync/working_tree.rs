use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use futures_util::stream::{self, StreamExt};

use crate::platform::path::validate_workspace_path;

use super::{
    diff::{DiffChangeKind, DiffSummary, diff_file_maps, line_hunks},
    manifest::{
        ManifestNode, ManifestRoot, NodeKind, hash_dir_node, hash_file_node, is_text_bytes,
    },
};

#[cfg(unix)]
use super::manifest::file_mode;

const MANIFEST_READ_CONCURRENCY: usize = 64;

struct GitTreeState {
    head: String,
    managed: BTreeSet<String>,
    changed: BTreeSet<String>,
}

impl GitTreeState {
    fn read(root: &Path) -> anyhow::Result<Self> {
        let head = git_text(root, &["rev-parse", "HEAD"])?;
        let managed = git_paths(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let mut changed = git_paths(root, &["diff", "--name-only", "-z", "HEAD", "--"])?;
        changed.extend(git_paths(
            root,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?);
        Ok(Self {
            head,
            managed,
            changed,
        })
    }
}

pub fn seed_session_from_main(main: &Path, session: &Path) -> anyhow::Result<Vec<String>> {
    let mut changed = git_paths(
        main,
        &["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"],
    )?;
    changed.extend(git_paths(
        main,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?);
    let changed_paths = changed.iter().cloned().collect();
    for path in changed {
        let rel = validate_workspace_path(&path)?;
        let source = main.join(&rel);
        let target = session.join(&rel);
        let source_metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                remove_path(&target)?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        if source_metadata.file_type().is_symlink() {
            copy_symlink(&source, &target)?;
        } else if source_metadata.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            remove_path(&target)?;
            std::fs::copy(&source, &target)
                .with_context(|| format!("copy managed path {} into Session", path))?;
        } else if source_metadata.is_dir() {
            // Git may collapse a fully-untracked subtree into a single
            // directory entry (a trailing-slash path). Recursively mirror the
            // subtree rather than bailing — keeps Session seeding resilient
            // to any leftover untracked dir regardless of .gitignore state.
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            remove_path(&target)?;
            copy_dir_tree(&source, &target)
                .with_context(|| format!("copy managed tree {} into Session", path))?;
        }
    }
    Ok(changed_paths)
}

pub async fn diff_working_trees(session: &Path, main: &Path) -> anyhow::Result<DiffSummary> {
    let session_state = GitTreeState::read(session)?;
    let main_state = GitTreeState::read(main)?;

    let mut candidates: BTreeSet<String> = session_state
        .managed
        .symmetric_difference(&main_state.managed)
        .cloned()
        .collect();
    candidates.extend(session_state.changed.iter().cloned());
    candidates.extend(main_state.changed.iter().cloned());
    if session_state.head != main_state.head {
        candidates.extend(git_paths(
            session,
            &[
                "diff",
                "--name-only",
                "-z",
                &session_state.head,
                &main_state.head,
                "--",
            ],
        )?);
    }

    let mut session_hashes = BTreeMap::new();
    let mut main_hashes = BTreeMap::new();
    let mut session_bytes = BTreeMap::new();
    let mut main_bytes = BTreeMap::new();

    for path in candidates {
        if let Some(file) =
            read_managed_file(session, &path, session_state.managed.contains(&path)).await?
        {
            session_hashes.insert(path.clone(), file.hash);
            session_bytes.insert(path.clone(), file.bytes);
        }
        if let Some(file) =
            read_managed_file(main, &path, main_state.managed.contains(&path)).await?
        {
            main_hashes.insert(path.clone(), file.hash);
            main_bytes.insert(path, file.bytes);
        }
    }

    let mut summary = diff_file_maps(&session_hashes, &main_hashes);
    for entry in &mut summary.paths {
        let session_content = match entry.kind {
            DiffChangeKind::Added | DiffChangeKind::Modified => session_bytes.get(&entry.path),
            DiffChangeKind::Deleted => None,
        };
        let main_content = match entry.kind {
            DiffChangeKind::Deleted | DiffChangeKind::Modified => main_bytes.get(&entry.path),
            DiffChangeKind::Added => None,
        };
        let (hunks, binary) = line_hunks(
            session_content.map(Vec::as_slice).unwrap_or_default(),
            main_content.map(Vec::as_slice).unwrap_or_default(),
        );
        entry.hunks = hunks;
        entry.binary = binary;
    }
    Ok(summary)
}

pub async fn hash_working_tree(root: &Path) -> anyhow::Result<ManifestRoot> {
    let paths = git_paths(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let files = stream::iter(paths.into_iter().map(|path| {
        let root = root.to_path_buf();
        async move { read_manifest_file(&root, path).await }
    }))
    .buffer_unordered(MANIFEST_READ_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut tree = ManifestTree::default();
    for file in files {
        if let Some((path, node)) = file? {
            tree.insert(&path, node)?;
        }
    }
    let (root_hash, nodes) = tree.finish("");
    Ok(ManifestRoot { root_hash, nodes })
}

pub async fn rehash_working_tree_paths(
    root: &Path,
    base: &ManifestRoot,
    changed_paths: &[String],
) -> anyhow::Result<ManifestRoot> {
    let mut nodes = base.nodes.clone();
    for path in changed_paths {
        let rel = validate_workspace_path(path)?;
        if root.join(&rel).is_dir() {
            return hash_working_tree(root).await;
        }
        nodes.remove(path);
        if let Some((path, node)) = read_manifest_file(root, path.clone()).await? {
            nodes.insert(path, node);
        }
    }

    let mut tree = ManifestTree::default();
    for (path, node) in nodes {
        if matches!(node.kind, NodeKind::File) {
            tree.insert(&path, node)?;
        }
    }
    let (root_hash, nodes) = tree.finish("");
    Ok(ManifestRoot { root_hash, nodes })
}

pub fn git_head(root: &Path) -> anyhow::Result<String> {
    git_text(root, &["rev-parse", "HEAD"])
}

struct ManagedFile {
    bytes: Vec<u8>,
    hash: String,
}

async fn read_manifest_file(
    root: &Path,
    path: String,
) -> anyhow::Result<Option<(String, ManifestNode)>> {
    let rel = validate_workspace_path(&path)?;
    let absolute = root.join(rel);
    let Some((bytes, mode)) = read_file(&absolute).await? else {
        return Ok(None);
    };
    let is_text = is_text_bytes(&bytes);
    let node_hash = hash_file_node(mode, &bytes, is_text);
    Ok(Some((
        path,
        ManifestNode {
            kind: NodeKind::File,
            mode,
            byte_len: bytes.len() as u64,
            blob_sha: None,
            node_hash,
            is_text,
        },
    )))
}

#[derive(Default)]
struct ManifestTree {
    files: BTreeMap<String, ManifestNode>,
    dirs: BTreeMap<String, ManifestTree>,
}

impl ManifestTree {
    fn insert(&mut self, path: &str, node: ManifestNode) -> anyhow::Result<()> {
        let mut components = path.split('/').peekable();
        let mut current = self;
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                current.files.insert(component.to_owned(), node);
                return Ok(());
            }
            current = current.dirs.entry(component.to_owned()).or_default();
        }
        bail!("managed path is empty")
    }

    fn finish(self, prefix: &str) -> (String, BTreeMap<String, ManifestNode>) {
        let mut children = BTreeMap::new();
        let mut nodes = BTreeMap::new();

        for (name, dir) in self.dirs {
            let path = join_path(prefix, &name);
            let (node_hash, nested) = dir.finish(&path);
            children.insert(name, (NodeKind::Dir, node_hash.clone()));
            nodes.insert(
                path,
                ManifestNode {
                    kind: NodeKind::Dir,
                    mode: 0o040755,
                    byte_len: 0,
                    blob_sha: None,
                    node_hash,
                    is_text: false,
                },
            );
            nodes.extend(nested);
        }
        for (name, node) in self.files {
            children.insert(name.clone(), (NodeKind::File, node.node_hash.clone()));
            nodes.insert(join_path(prefix, &name), node);
        }

        (hash_dir_node(&children), nodes)
    }
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

async fn read_managed_file(
    root: &Path,
    path: &str,
    managed: bool,
) -> anyhow::Result<Option<ManagedFile>> {
    if !managed {
        return Ok(None);
    }
    let rel = validate_workspace_path(path)?;
    let absolute = root.join(rel);
    let Some((bytes, mode)) = read_file(&absolute).await? else {
        return Ok(None);
    };
    let hash = hash_file_node(mode, &bytes, is_text_bytes(&bytes));
    Ok(Some(ManagedFile { bytes, hash }))
}

#[cfg(unix)]
async fn read_file(path: &Path) -> anyhow::Result<Option<(Vec<u8>, u32)>> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let bytes = tokio::fs::read(path).await?;
    Ok(Some((bytes, file_mode(&metadata))))
}

#[cfg(not(unix))]
async fn read_file(path: &Path) -> anyhow::Result<Option<(Vec<u8>, u32)>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some((bytes, 0o100644))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::IsADirectory => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn git_text(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = git_output(root, args)?;
    String::from_utf8(output)
        .map(|value| value.trim().to_owned())
        .map_err(|_| anyhow!("Git returned a non-UTF-8 value"))
}

fn git_paths(root: &Path, args: &[&str]) -> anyhow::Result<BTreeSet<String>> {
    // Janus creates `.janus-dev/` inside a project's Main clone to hold its own
    // workspaces (other Sessions' worktrees, etc.). On Windows `git ls-files
    // --others` collapses a fully-untracked subtree into a single directory
    // entry (trailing slash), which downstream `seed`/`read_manifest_file` then
    // treat as a file and bail on. Globally exclude `.janus-dev/` so a project
    // repo that imports Janus itself doesn't poison every new Session.
    let exclude_janus_dev = args.contains(&"--exclude-standard");
    let mut effective: Vec<&str> = args.to_vec();
    if exclude_janus_dev {
        effective.push("--exclude=.janus-dev/");
    }
    let output = git_output(root, &effective)?;
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec()).map_err(|_| anyhow!("Git path is not valid UTF-8"))
        })
        .collect()
}

fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .with_context(|| format!("run git in {}", root.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_path(target)?;
    symlink(std::fs::read_link(source)?, target)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    remove_path(target)?;
    let link = std::fs::read_link(source)?;
    if source.is_dir() {
        symlink_dir(link, target)?;
    } else {
        symlink_file(link, target)?;
    }
    Ok(())
}

/// Recursively mirror a directory subtree from `source` to `target`. Symlinks
/// are reproduced as symlinks (via `copy_symlink`); plain files are byte-copied;
/// nested directories recurse. `target` is wiped first so the mirror is exact.
fn copy_dir_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let name = entry.file_name();
        let to = target.join(&name);
        let metadata = entry.metadata()?;
        if metadata.file_type().is_symlink() {
            copy_symlink(&from, &to)?;
        } else if metadata.is_file() {
            remove_path(&to)?;
            std::fs::copy(&from, &to)?;
        } else if metadata.is_dir() {
            copy_dir_tree(&from, &to)?;
        }
    }
    Ok(())
}
