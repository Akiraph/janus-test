//! Merkle manifest collection for managed workspace content.
//!
//! Deterministic, version-domain-separated, length-prefixed encoding:
//! - file node: domain `janus-file-v2` + type + mode + normalized_len + content_hash -> node_hash
//! - dir node:  domain `janus-dir-v1` + sorted (name_len, name, node_type, node_hash)* -> node_hash
//!
//! `content_hash` is computed over **line-normalized** text: CRLF/CR are
//! treated equivalently to LF so a checked-out working tree (LF) and a Main
//! clone checked out with `core.autocrlf=true` (CRLF on disk) hash identically.
//! Binary files (any NUL byte) hash their raw bytes unchanged. This is what
//! stops a clean working tree from showing up as "every file modified" purely
//! because one side normalized line endings and the other did not.
//! `blob_sha` still stores the **raw** bytes in CAS (unmodified) - only the
//! content identity used for diffing is normalized.
//!
//! Walks the working tree, skipping VCS administration and Janus-owned runtime
//! entries. Janus's own data root must not appear in project trees in the first
//! place, but runtime-created scratch entries are also excluded defensively.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use janus_infrastructure::managed_storage::{BlobReference, BlobStore};

/// Relative path using `/` separators (workspace-root relative).
pub type RelPath = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Dir,
}

impl NodeKind {
    fn as_tag(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
        }
    }
}

/// One leaf or internal node after collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestNode {
    pub kind: NodeKind,
    pub mode: u32,
    pub byte_len: u64,
    pub blob_sha: Option<String>,
    /// SHA-256 over line-normalized text (raw bytes for binary). Drives diffing.
    pub node_hash: String,
    /// True when the file was classified as text (no NUL bytes). Lets the diff
    /// layer decide whether line-level hunks make sense.
    pub is_text: bool,
}

/// Full tree walk result: root hash + every relative path -> node metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRoot {
    pub root_hash: String,
    /// All non-root nodes keyed by relative path (`a/b.txt`). Root itself is not keyed.
    pub nodes: BTreeMap<RelPath, ManifestNode>,
}

/// Collect a full Merkle manifest under `root`, writing file bytes into `blobs`.
///
/// Skips the `.git` directory/file only. Other ignore rules (gitignore) are not
/// applied. Only `.git` is skipped, so callers must provide an appropriate
/// workspace root rather than relying on Git ignore rules.
pub async fn collect_manifest(
    root: &Path,
    blobs: &BlobStore,
    blob_owner_id: &str,
) -> anyhow::Result<ManifestRoot> {
    let (root_hash, nodes) = collect_dir(root, Path::new(""), blobs, blob_owner_id).await?;
    Ok(ManifestRoot { root_hash, nodes })
}

/// Return true for filesystem entries that can redirect traversal outside the
/// workspace root. Windows junctions are reparse points even when they are not
/// reported as symbolic links by the standard library.
pub(crate) fn is_link_or_reparse(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

async fn collect_dir(
    abs_root: &Path,
    rel: &Path,
    blobs: &BlobStore,
    blob_owner_id: &str,
) -> anyhow::Result<(String, BTreeMap<RelPath, ManifestNode>)> {
    let abs = if rel.as_os_str().is_empty() {
        abs_root.to_path_buf()
    } else {
        abs_root.join(rel)
    };

    let mut children: BTreeMap<String, (NodeKind, String)> = BTreeMap::new();
    let mut all_nodes: BTreeMap<RelPath, ManifestNode> = BTreeMap::new();

    if abs.is_dir() {
        let mut entries = tokio::fs::read_dir(&abs).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" || is_workspace_internal_component(&name) {
                continue;
            }
            let child_rel = if rel.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                rel.join(&name)
            };
            let child_abs = entry.path();
            let meta = tokio::fs::symlink_metadata(&child_abs).await?;
            if is_link_or_reparse(&meta) {
                continue;
            }
            if meta.is_dir() {
                let (child_hash, nested) =
                    Box::pin(collect_dir(abs_root, &child_rel, blobs, blob_owner_id)).await?;
                let rel_key = path_to_rel(&child_rel);
                all_nodes.insert(
                    rel_key.clone(),
                    ManifestNode {
                        kind: NodeKind::Dir,
                        mode: 0o040755,
                        byte_len: 0,
                        blob_sha: None,
                        node_hash: child_hash.clone(),
                        is_text: false,
                    },
                );
                all_nodes.extend(nested);
                children.insert(name, (NodeKind::Dir, child_hash));
            } else if meta.is_file() {
                let bytes = tokio::fs::read(&child_abs).await?;
                let mode = file_mode(&meta);
                let len = bytes.len() as u64;
                let blob_ref =
                    BlobReference::new("workspace", "manifest_file", blob_owner_id, "content");
                let blob_sha = blobs.write(&bytes, blob_ref).await?;
                // node_hash is over line-normalized text so CRLF/LF do not differ.
                let is_text = is_text_bytes(&bytes);
                let node_hash = hash_file_node(mode, &bytes, is_text);
                let rel_key = path_to_rel(&child_rel);
                all_nodes.insert(
                    rel_key,
                    ManifestNode {
                        kind: NodeKind::File,
                        mode,
                        byte_len: len,
                        blob_sha: Some(blob_sha.to_string()),
                        node_hash: node_hash.clone(),
                        is_text,
                    },
                );
                children.insert(name, (NodeKind::File, node_hash));
            }
            // Symlinks and other special types are skipped rather than followed.
        }
    }

    let root_hash = hash_dir_node(&children);
    Ok((root_hash, all_nodes))
}

pub(crate) fn is_workspace_internal_component(name: &str) -> bool {
    name == ".janus-dev"
        || name == ".janus-tmp"
        || name.starts_with(".janus-runtime-")
        || (name.len() > 2 && name.starts_with('%') && name.ends_with('%'))
}

pub(crate) fn is_workspace_internal_path(path: &str) -> bool {
    path.split(['/', '\\']).any(is_workspace_internal_component)
}

fn path_to_rel(path: &Path) -> RelPath {
    path.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn file_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o100644
    }
}

/// File node: `janus-file-v2` domain + type + mode + content_hash.
///
/// For text files `content_hash` is the SHA-256 of the line-normalized bytes
/// (CRLF/CR collapsed to LF); for binary files it is the SHA-256 of the raw
/// bytes. `mode` is part of identity so a chmod still registers as a change.
pub fn hash_file_node(mode: u32, bytes: &[u8], is_text: bool) -> String {
    let mut hasher = Sha256::new();
    write_domain(&mut hasher, b"janus-file-v2");
    write_str(&mut hasher, "file");
    hasher.update(mode.to_be_bytes());
    if is_text {
        hash_normalized_text(&mut hasher, bytes);
    } else {
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}

/// Decide text vs binary the same way git does: any NUL byte -> binary.
pub fn is_text_bytes(bytes: &[u8]) -> bool {
    !bytes.contains(&0u8)
}

/// Feed line-normalized text into the hasher: split on any of \n / \r\n / \r,
/// then write each line payload followed by a single LF. So "a\r\nb" and
/// "a\nb" hash identically, while "a\n" differs from "a" (extra trailing line).
fn hash_normalized_text(hasher: &mut Sha256, bytes: &[u8]) {
    for line in split_lines(bytes) {
        hasher.update(line);
        hasher.update(b"\n");
    }
}

/// Iterator yielding each logical line's payload (terminator excluded),
/// treating `\r\n`, `\n`, and `\r` as equivalent line breaks.
///
/// A trailing terminator produces a final empty line so `"a\n"` and `"a"`
/// remain distinct. `"a\nb\n"` -> `["a", "b", ""]`; `"a\nb"` -> `["a", "b"]`.
pub fn split_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    SplitLines {
        bytes,
        pending_empty: false,
        done: false,
    }
}

struct SplitLines<'a> {
    bytes: &'a [u8],
    /// Emit one empty line after a terminator that ended the input.
    pending_empty: bool,
    done: bool,
}

impl<'a> Iterator for SplitLines<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.done {
            return None;
        }
        if self.pending_empty {
            self.pending_empty = false;
            self.done = true;
            return Some(&[]);
        }
        if self.bytes.is_empty() {
            self.done = true;
            return None;
        }
        // Find the next line terminator among the three forms.
        match self.bytes.iter().position(|&b| b == b'\n' || b == b'\r') {
            Some(idx) => {
                let b = self.bytes[idx];
                // Collapse \r\n into a single break.
                let term_len = if b == b'\r' && self.bytes.get(idx + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                let line = &self.bytes[..idx];
                self.bytes = &self.bytes[idx + term_len..];
                // Trailing terminator -> final empty line so "a\n" != "a".
                if self.bytes.is_empty() {
                    self.pending_empty = true;
                }
                Some(line)
            }
            None => {
                // Tail line with no terminator.
                let line = self.bytes;
                self.bytes = &[];
                self.done = true;
                Some(line)
            }
        }
    }
}

/// Dir node: `janus-dir-v1` + sorted (name_len, name, node_type, node_hash).
/// `children` is a BTreeMap so iteration is already UTF-8-byte sorted by name.
pub fn hash_dir_node(children: &BTreeMap<String, (NodeKind, String)>) -> String {
    let mut hasher = Sha256::new();
    write_domain(&mut hasher, b"janus-dir-v1");
    for (name, (kind, node_hash)) in children {
        let name_bytes = name.as_bytes();
        hasher.update((name_bytes.len() as u32).to_be_bytes());
        hasher.update(name_bytes);
        write_str(&mut hasher, kind.as_tag());
        write_str(&mut hasher, node_hash);
    }
    format!("{:x}", hasher.finalize())
}

fn write_domain(hasher: &mut Sha256, domain: &[u8]) {
    hasher.update((domain.len() as u32).to_be_bytes());
    hasher.update(domain);
}

fn write_str(hasher: &mut Sha256, value: &str) {
    let bytes = value.as_bytes();
    hasher.update((bytes.len() as u32).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hash_is_stable() {
        let a = hash_file_node(0o100644, b"abc", true);
        let b = hash_file_node(0o100644, b"abc", true);
        let c = hash_file_node(0o100644, b"abcd", true);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn text_hash_ignores_line_ending_flavor() {
        // CRLF, LF, and lone CR over the same lines must hash identically.
        let crlf = hash_file_node(0o100644, b"a\nb\nc", true);
        let lf = hash_file_node(0o100644, b"a\r\nb\r\nc", true);
        let cr = hash_file_node(0o100644, b"a\rb\rc", true);
        assert_eq!(crlf, lf);
        assert_eq!(crlf, cr);
    }

    #[test]
    fn text_hash_distinguishes_trailing_newline_and_mode() {
        let no_nl = hash_file_node(0o100644, b"a\nb", true);
        let with_nl = hash_file_node(0o100644, b"a\nb\n", true);
        assert_ne!(no_nl, with_nl, "trailing newline must matter");
        let exec = hash_file_node(0o100755, b"a\nb", true);
        assert_ne!(no_nl, exec, "mode must matter");
    }

    #[test]
    fn binary_hash_is_over_raw_bytes() {
        // A NUL byte flips the file to binary; content hash is raw bytes, so
        // two files differing only in line endings differ (no normalization).
        let bin_lf = hash_file_node(0o100644, b"a\n\x00\nb", false);
        let bin_crlf = hash_file_node(0o100644, b"a\r\n\x00\r\nb", false);
        assert_ne!(bin_lf, bin_crlf);
        assert!(is_text_bytes(b"plain text"));
        assert!(!is_text_bytes(b"has\x00nul"));
    }

    #[test]
    fn split_lines_handles_all_terminators() {
        fn collect(s: &[u8]) -> Vec<Vec<u8>> {
            split_lines(s).map(|l| l.to_vec()).collect()
        }
        assert_eq!(
            collect(b"a\nb\n"),
            vec![b"a".to_vec(), b"b".to_vec(), b"".to_vec()]
        );
        assert_eq!(
            collect(b"a\r\nb\r\n"),
            vec![b"a".to_vec(), b"b".to_vec(), b"".to_vec()]
        );
        assert_eq!(
            collect(b"a\rb\r"),
            vec![b"a".to_vec(), b"b".to_vec(), b"".to_vec()]
        );
        assert_eq!(collect(b"a\nb"), vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(collect(b""), Vec::<Vec<u8>>::new());
        assert_eq!(collect(b"\n"), vec![b"".to_vec(), b"".to_vec()]);
        assert_eq!(collect(b"a"), vec![b"a".to_vec()]);
    }

    #[test]
    fn dir_hash_orders_by_name() {
        let mut kids = BTreeMap::new();
        kids.insert("b".into(), (NodeKind::File, "h1".into()));
        kids.insert("a".into(), (NodeKind::File, "h2".into()));
        let h = hash_dir_node(&kids);
        let mut kids2 = BTreeMap::new();
        kids2.insert("a".into(), (NodeKind::File, "h2".into()));
        kids2.insert("b".into(), (NodeKind::File, "h1".into()));
        assert_eq!(h, hash_dir_node(&kids2));
    }

    #[test]
    fn empty_dir_has_stable_hash() {
        let empty = BTreeMap::new();
        assert_eq!(hash_dir_node(&empty), hash_dir_node(&BTreeMap::new()));
    }

    #[tokio::test]
    async fn collect_manifest_does_not_follow_directory_links() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let link = root.path().join("external");

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

        let db = janus_infrastructure::testing::TestDb::open()
            .await
            .expect("test database");
        let blobs = janus_infrastructure::managed_storage::BlobStore::new(
            db.database().clone(),
            root.path(),
        )
        .expect("blob store");
        let manifest = collect_manifest(root.path(), &blobs, "manifest-test")
            .await
            .expect("collect manifest");

        assert!(!manifest.nodes.contains_key("external"));
    }
}
