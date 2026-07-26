//! Path-level Diff summary between Session and Main manifests.
//!
//! M3 only: file-level added / modified / deleted counts + path lists.
//! No three-way merge, no Apply conditions (`apply_enabled: false` at the HTTP layer).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use utoipa::ToSchema;

use super::manifest::{ManifestRoot, file_content_index, is_text_bytes, split_lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffChangeKind {
    Added,
    Modified,
    Deleted,
}

/// One rendered line inside a file hunk. `old_no` / `new_no` are 1-based;
/// either may be absent for pure additions / deletions.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_no: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
    /// Collapsed run of unchanged lines between hunks. `text` holds a short label
    /// like "⋯ 12 lines" and line numbers are omitted.
    Skip,
}

/// One contiguous change region. Unchanged spans longer than the context window
/// become a single `Skip` line instead of being listed out.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffHunk {
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffPathEntry {
    pub path: String,
    pub kind: DiffChangeKind,
    /// Line-level hunks when available. Empty for binary / oversized / pure path
    /// classification without content. Frontend collapses by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<DiffHunk>,
    /// True when content is binary or too large for line-level display.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffSummary {
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
    pub paths: Vec<DiffPathEntry>,
    /// Always false in M3; Apply/Sync land in M5.
    pub apply_enabled: bool,
}

/// Compare two manifests at the file-path level using leaf node hashes.
pub fn diff_manifests(session: &ManifestRoot, main: &ManifestRoot) -> DiffSummary {
    let session_files = file_content_index(session);
    let main_files = file_content_index(main);
    diff_file_maps(&session_files, &main_files)
}

/// Session vs Main content-hash maps (path → file node_hash).
pub fn diff_file_maps(
    session: &BTreeMap<String, String>,
    main: &BTreeMap<String, String>,
) -> DiffSummary {
    let mut paths = Vec::new();
    let all: BTreeSet<&String> = session.keys().chain(main.keys()).collect();
    let mut added = 0u32;
    let mut modified = 0u32;
    let mut deleted = 0u32;

    for path in all {
        match (session.get(path), main.get(path)) {
            (Some(s), Some(m)) if s != m => {
                modified += 1;
                paths.push(DiffPathEntry {
                    path: path.clone(),
                    kind: DiffChangeKind::Modified,
                    hunks: Vec::new(),
                    binary: false,
                });
            }
            (Some(_), None) => {
                added += 1;
                paths.push(DiffPathEntry {
                    path: path.clone(),
                    kind: DiffChangeKind::Added,
                    hunks: Vec::new(),
                    binary: false,
                });
            }
            (None, Some(_)) => {
                deleted += 1;
                paths.push(DiffPathEntry {
                    path: path.clone(),
                    kind: DiffChangeKind::Deleted,
                    hunks: Vec::new(),
                    binary: false,
                });
            }
            _ => {}
        }
    }

    DiffSummary {
        added,
        modified,
        deleted,
        paths,
        apply_enabled: false,
    }
}

/// Cap for line-level diff. Larger files still appear as path entries with
/// `binary: true` so the UI can show "too large / binary" instead of hanging.
const MAX_DIFF_BYTES: usize = 200 * 1024;
const MAX_DIFF_LINES: usize = 4000;
/// Unchanged lines kept on each side of a change region.
const CONTEXT_LINES: usize = 3;
/// Unchanged runs longer than this collapse into a single `Skip` line.
const COLLAPSE_THRESHOLD: usize = CONTEXT_LINES * 2 + 1;

/// Build line-level hunks for one file. Returns `(hunks, binary)`.
///
/// - Binary / oversized → empty hunks + `binary: true`.
/// - Text → one or more hunks with context, long equal spans collapsed via `Skip`.
pub fn line_hunks(session_bytes: &[u8], main_bytes: &[u8]) -> (Vec<DiffHunk>, bool) {
    if session_bytes.len() > MAX_DIFF_BYTES || main_bytes.len() > MAX_DIFF_BYTES {
        return (Vec::new(), true);
    }
    let session_text = is_text_bytes(session_bytes);
    let main_text = is_text_bytes(main_bytes);
    if !session_text || !main_text {
        return (Vec::new(), true);
    }

    let session_lines: Vec<&[u8]> = split_lines(session_bytes).collect();
    let main_lines: Vec<&[u8]> = split_lines(main_bytes).collect();
    if session_lines.len() > MAX_DIFF_LINES || main_lines.len() > MAX_DIFF_LINES {
        return (Vec::new(), true);
    }

    // Myers-style LCS via DP on line equality. Fine for the size caps above.
    let ops = lcs_ops(&main_lines, &session_lines);
    let hunks = collapse_ops(&ops, &main_lines, &session_lines);
    (hunks, false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// LCS-based edit script from `old` (main) → `new` (session).
fn lcs_ops(old: &[&[u8]], new: &[&[u8]]) -> Vec<Op> {
    let n = old.len();
    let m = new.len();
    // dp[i][j] = LCS length of old[i..] / new[j..]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let mut i = 0usize;
    let mut j = 0usize;
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Equal);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete);
            i += 1;
        } else {
            ops.push(Op::Insert);
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete);
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert);
        j += 1;
    }
    ops
}

/// Walk the edit script, keep CONTEXT_LINES around changes, collapse long equal
/// runs into a single `Skip` line. Output is one hunk covering the whole file
/// (skip markers already provide visual separation).
fn collapse_ops(ops: &[Op], old: &[&[u8]], new: &[&[u8]]) -> Vec<DiffHunk> {
    if ops.is_empty() {
        return Vec::new();
    }
    // Mark which equal ops are "interesting" (near a change).
    let mut keep = vec![false; ops.len()];
    let mut has_change = false;
    for (idx, op) in ops.iter().enumerate() {
        if matches!(op, Op::Delete | Op::Insert) {
            has_change = true;
            // Keep the change itself and CONTEXT_LINES of surrounding equals.
            keep[idx] = true;
            // walk left
            let mut left = 0usize;
            let mut k = idx;
            while k > 0 && left < CONTEXT_LINES {
                k -= 1;
                if ops[k] == Op::Equal {
                    keep[k] = true;
                    left += 1;
                } else {
                    // stop at previous change region; already marked.
                    break;
                }
            }
            // walk right
            let mut right = 0usize;
            let mut k = idx + 1;
            while k < ops.len() && right < CONTEXT_LINES {
                if ops[k] == Op::Equal {
                    keep[k] = true;
                    right += 1;
                    k += 1;
                } else {
                    break;
                }
            }
        }
    }
    if !has_change {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut old_no: u32 = 1;
    let mut new_no: u32 = 1;
    let mut oi = 0usize;
    let mut ni = 0usize;
    let mut i = 0usize;
    while i < ops.len() {
        if keep[i] {
            match ops[i] {
                Op::Equal => {
                    let text = line_to_string(old[oi]);
                    lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text,
                    });
                    oi += 1;
                    ni += 1;
                    old_no += 1;
                    new_no += 1;
                }
                Op::Delete => {
                    let text = line_to_string(old[oi]);
                    lines.push(DiffLine {
                        kind: DiffLineKind::Delete,
                        old_no: Some(old_no),
                        new_no: None,
                        text,
                    });
                    oi += 1;
                    old_no += 1;
                }
                Op::Insert => {
                    let text = line_to_string(new[ni]);
                    lines.push(DiffLine {
                        kind: DiffLineKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text,
                    });
                    ni += 1;
                    new_no += 1;
                }
            }
            i += 1;
        } else {
            // Collapsed equal run. Count how many equals we skip.
            let mut skipped = 0u32;
            while i < ops.len() && !keep[i] {
                debug_assert_eq!(ops[i], Op::Equal);
                oi += 1;
                ni += 1;
                old_no += 1;
                new_no += 1;
                skipped += 1;
                i += 1;
            }
            if skipped > 0 {
                // Only emit Skip when the collapsed region is meaningfully long.
                // (Always true here because keep marks CONTEXT_LINES; remaining
                // equals are at least COLLAPSE_THRESHOLD - 2*CONTEXT long, but we
                // still emit for leading/trailing collapsed spans of any size.)
                let _ = COLLAPSE_THRESHOLD;
                lines.push(DiffLine {
                    kind: DiffLineKind::Skip,
                    old_no: None,
                    new_no: None,
                    text: format!("⋯ {skipped} lines"),
                });
            }
        }
    }

    if lines.is_empty() {
        Vec::new()
    } else {
        vec![DiffHunk { lines }]
    }
}

fn line_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_added_modified_deleted() {
        let mut session = BTreeMap::new();
        session.insert("a.txt".into(), "h1".into());
        session.insert("b.txt".into(), "h2".into());
        session.insert("c.txt".into(), "h3".into());
        let mut main = BTreeMap::new();
        main.insert("a.txt".into(), "h1".into()); // same
        main.insert("b.txt".into(), "hX".into()); // modified
        main.insert("d.txt".into(), "h4".into()); // deleted from session

        let summary = diff_file_maps(&session, &main);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.modified, 1);
        assert_eq!(summary.deleted, 1);
        assert!(!summary.apply_enabled);
    }

    #[test]
    fn line_hunks_marks_change_and_collapses_common() {
        // 20 shared lines, one middle change — must collapse the bulk.
        let mut main = String::new();
        let mut sess = String::new();
        for i in 0..20 {
            main.push_str(&format!("line {i}\n"));
            if i == 10 {
                sess.push_str("CHANGED\n");
            } else {
                sess.push_str(&format!("line {i}\n"));
            }
        }
        let (hunks, binary) = line_hunks(sess.as_bytes(), main.as_bytes());
        assert!(!binary);
        assert_eq!(hunks.len(), 1);
        let lines = &hunks[0].lines;
        // Must contain a Skip marker and exactly one Delete + one Add.
        assert!(
            lines.iter().any(|l| l.kind == DiffLineKind::Skip),
            "expected collapsed common lines, got {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.kind == DiffLineKind::Delete)
                .count(),
            1
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.kind == DiffLineKind::Add)
                .count(),
            1
        );
        let add = lines
            .iter()
            .find(|l| l.kind == DiffLineKind::Add)
            .unwrap();
        assert_eq!(add.text, "CHANGED");
    }

    #[test]
    fn line_hunks_identical_files_empty() {
        let (hunks, binary) = line_hunks(b"a\nb\n", b"a\nb\n");
        assert!(!binary);
        assert!(hunks.is_empty());
    }

    #[test]
    fn line_hunks_binary_flagged() {
        let (hunks, binary) = line_hunks(b"a\x00b", b"a\x00c");
        assert!(binary);
        assert!(hunks.is_empty());
    }
}
