use crate::tools::{existing_workspace_path, required_string};
use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;
use regex::Regex;
use serde_json::Value;
use std::fs::{self, Metadata, OpenOptions};
use std::io::Read;
use std::path::Path;

const MAX_FILES: usize = 5_000;
const MAX_PATHS: usize = 20_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_MATCHES: usize = 200;
const MAX_PATTERN_BYTES: usize = 16 * 1024;

pub(crate) async fn search_files(
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    let pattern = required_string(&call.arguments, "pattern")?;
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_BYTES {
        return Ok(ToolOutput::error(
            "search pattern must contain 1..16384 bytes",
        ));
    }
    let regex = Regex::new(pattern)
        .map_err(|error| AgentError::Tool(format!("invalid search pattern: {error}")))?;
    let start = call
        .arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let root = context.workspace.canonicalize().map_err(tool_io_error)?;
    let start = existing_workspace_path(&root, start)?;
    let mut stack = vec![start];
    let mut scanned = 0usize;
    let mut visited = 0usize;
    let mut matches = Vec::new();
    while let Some(path) = stack.pop() {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        visited += 1;
        if visited > MAX_PATHS {
            matches.push(format!("[search stopped after {MAX_PATHS} paths]"));
            break;
        }
        // Re-resolve queued entries because a directory may have been
        // replaced by a junction after it was listed.
        let current = path.canonicalize().map_err(tool_io_error)?;
        if !current.starts_with(&root) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(tool_io_error)?;
        if is_link_or_reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(&path).map_err(tool_io_error)? {
                let entry = entry.map_err(tool_io_error)?;
                let child_path = entry.path();
                let child_metadata = fs::symlink_metadata(&child_path).map_err(tool_io_error)?;
                if is_link_or_reparse(&child_metadata) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git" | "node_modules" | "target" | ".next") {
                    continue;
                }
                let child_path = child_path.canonicalize().map_err(tool_io_error)?;
                if !child_path.starts_with(&root) {
                    continue;
                }
                children.push(child_path);
            }
            children.sort();
            stack.extend(children.into_iter().rev());
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            matches.push(format!("[search stopped after {MAX_FILES} files]"));
            break;
        }
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(file) = open_no_follow(&path) else {
            continue;
        };
        let Ok(open_metadata) = file.metadata() else {
            continue;
        };
        if !open_metadata.is_file() || is_link_or_reparse(&open_metadata) {
            continue;
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_FILE_BYTES
        {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let relative = relative_path(&path, &root);
        for (index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(format!(
                    "{}:{}: {}",
                    relative,
                    index + 1,
                    line.chars().take(240).collect::<String>()
                ));
                if matches.len() >= MAX_MATCHES {
                    matches.push(format!("[results truncated at {MAX_MATCHES} matches]"));
                    return Ok(ToolOutput::success(matches.join("\n")));
                }
            }
        }
    }
    if matches.is_empty() {
        Ok(ToolOutput::success("no matches"))
    } else {
        Ok(ToolOutput::success(matches.join("\n")))
    }
}

fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn relative_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn tool_io_error(error: std::io::Error) -> AgentError {
    AgentError::Tool(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn finds_lines_inside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.rs"), "one\nneedle here\nthree").unwrap();
        let call = ToolCall {
            id: "search-1".into(),
            name: "search_files".into(),
            arguments: json!({"pattern":"needle"}),
        };
        let result = search_files(
            &call,
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "sample.rs:2: needle here");
    }

    #[tokio::test]
    async fn skips_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        bytes[..6].copy_from_slice(b"needle");
        fs::write(dir.path().join("huge.txt"), bytes).unwrap();
        let result = search_files(
            &ToolCall {
                id: "search-large".into(),
                name: "search_files".into(),
                arguments: json!({"pattern":"needle"}),
            },
            &ToolContext {
                workspace: dir.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "no matches");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_linked_paths_outside_workspace() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "needle").unwrap();
        symlink(outside.path(), workspace.path().join("linked")).unwrap();
        let result = search_files(
            &ToolCall {
                id: "search-link".into(),
                name: "search_files".into(),
                arguments: json!({"pattern":"needle"}),
            },
            &ToolContext {
                workspace: workspace.path().to_path_buf(),
                cancel: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.content, "no matches");
    }
}
