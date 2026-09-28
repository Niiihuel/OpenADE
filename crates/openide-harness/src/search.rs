use crate::tools::{existing_workspace_path, required_string};
use crate::{AgentError, ToolContext, ToolOutput};
use openide_core::ToolCall;
use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::Path;

const MAX_FILES: usize = 5_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_MATCHES: usize = 200;

pub(crate) async fn search_files(
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    let pattern = required_string(&call.arguments, "pattern")?;
    if pattern.is_empty() {
        return Ok(ToolOutput::error("search pattern is empty"));
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
    let mut matches = Vec::new();
    while let Some(path) = stack.pop() {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if path.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(&path).map_err(tool_io_error)? {
                let entry = entry.map_err(tool_io_error)?;
                if entry.file_type().map_err(tool_io_error)?.is_symlink() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if matches!(name.as_str(), ".git" | "node_modules" | "target" | ".next") {
                    continue;
                }
                children.push(entry.path());
            }
            children.sort();
            stack.extend(children.into_iter().rev());
            continue;
        }
        if !path.is_file() {
            continue;
        }
        scanned += 1;
        if scanned > MAX_FILES {
            matches.push(format!("[search stopped after {MAX_FILES} files]"));
            break;
        }
        let metadata = fs::metadata(&path).map_err(tool_io_error)?;
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
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
}
