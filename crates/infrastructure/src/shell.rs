use std::{ffi::OsString, path::PathBuf};

pub struct DecodedProcessOutput {
    pub text: String,
    pub original_bytes: usize,
    pub truncated: bool,
    pub encoding: &'static str,
}

pub fn decode_process_output(bytes: &[u8], max_bytes: usize) -> DecodedProcessOutput {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let mut end = text.len().min(max_bytes);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            DecodedProcessOutput {
                text: text[..end].to_owned(),
                original_bytes: bytes.len(),
                truncated: end < text.len(),
                encoding: "utf-8",
            }
        }
        Err(_) => {
            let preview_len = bytes.len().min(max_bytes.min(256));
            let preview = bytes[..preview_len]
                .iter()
                .flat_map(|byte| std::ascii::escape_default(*byte))
                .map(char::from)
                .collect::<String>();
            DecodedProcessOutput {
                text: format!(
                    "[non-UTF-8 process output: {} bytes]\n{}",
                    bytes.len(),
                    preview
                ),
                original_bytes: bytes.len(),
                truncated: preview_len < bytes.len(),
                encoding: "binary-escaped",
            }
        }
    }
}

pub fn bash_program() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path) {
                let candidate = directory.join("bash.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        let output = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        for ancestor in exec_path.ancestors() {
            for relative in ["bin/bash.exe", "usr/bin/bash.exe"] {
                let candidate = ancestor.join(relative);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    #[cfg(not(windows))]
    {
        let path = PathBuf::from("/bin/bash");
        path.is_file().then_some(path)
    }
}

/// Return a PATH that can execute Git Bash's bundled POSIX utilities as well
/// as the host's normal commands. Windows `bash.exe` does not reliably add its
/// own `usr/bin` when started directly by a Rust process with a scrubbed env.
pub fn bash_search_path() -> Option<OsString> {
    let current = std::env::var_os("PATH")?;
    #[cfg(windows)]
    {
        let program = bash_program()?;
        let bin_dir = program.parent()?;
        let parent = bin_dir.parent()?;
        let git_root = if parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("usr"))
        {
            parent.parent()?
        } else {
            parent
        };
        let mut entries = vec![
            git_root.join("cmd"),
            git_root.join("bin"),
            git_root.join("usr").join("bin"),
        ];
        entries.extend(std::env::split_paths(&current));
        std::env::join_paths(entries).ok()
    }
    #[cfg(not(windows))]
    {
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::{bash_program, bash_search_path, decode_process_output};

    #[cfg(windows)]
    #[test]
    fn windows_bash_provider_resolves_git_bash() {
        let program = bash_program().expect("Git Bash must be discoverable on a supported host");
        assert_eq!(
            program
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("bash.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_bash_path_includes_git_utilities() {
        let path = bash_search_path().expect("Git Bash PATH must be available");
        let entries = std::env::split_paths(&path).collect::<Vec<_>>();
        assert!(entries.iter().any(|entry| entry.ends_with("usr\\bin")));
        assert!(entries.iter().any(|entry| entry.ends_with("cmd")));
    }

    #[test]
    fn process_output_truncates_on_utf8_boundaries() {
        let decoded = decode_process_output("你好世界".as_bytes(), 5);
        assert_eq!(decoded.text, "你");
        assert!(decoded.truncated);
        assert_eq!(decoded.encoding, "utf-8");
    }

    #[test]
    fn invalid_utf8_is_explicitly_escaped() {
        let decoded = decode_process_output(&[0xff, b'A', 0], 16);
        assert_eq!(decoded.encoding, "binary-escaped");
        assert!(decoded.text.contains("\\xffA\\x00"));
    }
}
