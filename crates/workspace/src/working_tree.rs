use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use futures_util::stream::{self, StreamExt};

use super::manifest::{
    ManifestNode, ManifestRoot, NodeKind, hash_dir_node, hash_file_node, is_link_or_reparse,
    is_text_bytes, is_workspace_internal_path,
};
use super::path::validate_workspace_path;

use super::manifest::file_mode;

const MANIFEST_READ_CONCURRENCY: usize = 64;

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
    // Bound file reads so a large repository does not create one task per path.
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

async fn read_manifest_file(
    root: &Path,
    path: String,
) -> anyhow::Result<Option<(String, ManifestNode)>> {
    let rel = validate_workspace_path(&path)?;
    let Some((bytes, mode)) = read_file(root, &rel).await? else {
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

async fn read_file(root: &Path, rel: &Path) -> anyhow::Result<Option<(Vec<u8>, u32)>> {
    let mut current = root.to_path_buf();
    for component in rel.components() {
        current.push(component.as_os_str());
        let metadata = match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if is_link_or_reparse(&metadata) {
            return Ok(None);
        }
    }

    let metadata = match tokio::fs::symlink_metadata(&current).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let bytes = tokio::fs::read(&current).await?;
    Ok(Some((bytes, file_mode(&metadata))))
}

pub(super) fn git_command(root: &Path) -> Command {
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

fn git_paths(root: &Path, args: &[&str]) -> anyhow::Result<BTreeSet<String>> {
    // Janus creates `.janus-dev/` inside a managed clone. Exclude it so a
    // project repo that imports Janus itself does not enter its own manifest.
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
        .filter(|path| match path {
            Ok(path) => !is_ignored_workspace_path(path),
            Err(_) => true,
        })
        .collect()
}

fn is_ignored_workspace_path(path: &str) -> bool {
    is_workspace_internal_path(path)
}

fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = git_command(root)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn git_path_scan_ignores_runtime_and_host_placeholder_paths() {
        assert!(super::is_ignored_workspace_path(
            "%SystemDrive%/ProgramData/Microsoft/Windows/Caches/cversions.2.db"
        ));
        assert!(super::is_ignored_workspace_path(".janus-tmp/output.log"));
        assert!(super::is_ignored_workspace_path(".janus-runtime-test.pid"));
        assert!(!super::is_ignored_workspace_path("apps/server/src/main.rs"));
    }

    #[tokio::test]
    async fn read_file_does_not_follow_external_directory_links() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let link = root.path().join("external");
        let outside_file = outside.path().join("secret.txt");
        std::fs::write(&outside_file, b"outside").expect("outside file");

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).expect("create directory link");
        #[cfg(windows)]
        {
            let result = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    link.to_str().expect("link path"),
                    outside.path().to_str().expect("outside path"),
                ])
                .output()
                .expect("create directory junction");
            assert!(
                result.status.success(),
                "mklink failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }

        let followed = super::read_file(root.path(), Path::new("external/secret.txt"))
            .await
            .expect("read link path");
        assert!(followed.is_none(), "external link target must not be read");
    }

    #[cfg(unix)]
    #[test]
    fn directory_copy_preserves_symlinks() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        let target = tempfile::tempdir().expect("target");
        std::fs::write(source.path().join("file.txt"), b"content").expect("write file");
        symlink("file.txt", source.path().join("link.txt")).expect("create link");

        super::copy_dir_tree(source.path(), target.path().join("copy").as_path())
            .expect("copy directory");

        assert!(
            std::fs::symlink_metadata(target.path().join("copy/link.txt"))
                .expect("copied link")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn git_command_allows_a_different_repository_owner() {
        use std::process::Command;

        let repo = tempfile::tempdir().expect("temporary repository");
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

        let output = super::git_command(repo.path())
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
