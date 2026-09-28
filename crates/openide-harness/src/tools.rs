use crate::AgentError;
use async_trait::async_trait;
use openide_core::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_READ_BYTES: u64 = 256 * 1024;
const MAX_LIST_ENTRIES: usize = 200;
const MAX_WRITE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub cancel: CancellationToken,
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError>;
}

/// Names of the native tools that can be independently exposed and executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalTool {
    ReadFile,
    ListFiles,
    SearchFiles,
    WriteFile,
    RunCommand,
}

impl LocalTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListFiles => "list_files",
            Self::SearchFiles => "search_files",
            Self::WriteFile => "write_file",
            Self::RunCommand => "run_command",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "read_file" => Some(Self::ReadFile),
            "list_files" => Some(Self::ListFiles),
            "search_files" => Some(Self::SearchFiles),
            "write_file" => Some(Self::WriteFile),
            "run_command" => Some(Self::RunCommand),
            _ => None,
        }
    }
}

/// Policy is checked both when advertising tools to a model and at execution time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPermissions {
    pub read_file: bool,
    pub list_files: bool,
    pub search_files: bool,
    pub write_file: bool,
    pub run_command: bool,
}

impl Default for ToolPermissions {
    fn default() -> Self {
        Self {
            read_file: true,
            list_files: true,
            search_files: true,
            write_file: false,
            run_command: false,
        }
    }
}

impl ToolPermissions {
    pub fn allows(self, tool: LocalTool) -> bool {
        match tool {
            LocalTool::ReadFile => self.read_file,
            LocalTool::ListFiles => self.list_files,
            LocalTool::SearchFiles => self.search_files,
            LocalTool::WriteFile => self.write_file,
            LocalTool::RunCommand => self.run_command,
        }
    }

    pub fn set(&mut self, tool: LocalTool, allowed: bool) {
        match tool {
            LocalTool::ReadFile => self.read_file = allowed,
            LocalTool::ListFiles => self.list_files = allowed,
            LocalTool::SearchFiles => self.search_files = allowed,
            LocalTool::WriteFile => self.write_file = allowed,
            LocalTool::RunCommand => self.run_command = allowed,
        }
    }
}

/// Small local tool set with workspace containment for its file tools.
#[derive(Default)]
pub struct LocalTools {
    permissions: ToolPermissions,
}

impl LocalTools {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_permissions(mut self, permissions: ToolPermissions) -> Self {
        self.permissions = permissions;
        self
    }

    pub fn with_tool_permission(mut self, tool: LocalTool, allowed: bool) -> Self {
        self.permissions.set(tool, allowed);
        self
    }

    pub fn permissions(&self) -> ToolPermissions {
        self.permissions
    }

    /// Compatibility shorthand for the current UI: controls write_file and run_command together.
    pub fn with_write_access(mut self, enabled: bool) -> Self {
        self.permissions.write_file = enabled;
        self.permissions.run_command = enabled;
        self
    }
}

#[async_trait]
impl ToolExecutor for LocalTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = vec![
            ToolDefinition {
                name: "read_file".into(),
                description: "Read a UTF-8 text file inside the active workspace.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "list_files".into(),
                description: "List direct children of a workspace directory.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "search_files".into(),
                description: "Search UTF-8 workspace files with a regular expression.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"},
                        "path": {"type": "string"}
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            },
        ];
        if self.permissions.write_file {
            tools.push(ToolDefinition {
                name: "write_file".into(),
                description: "Write a UTF-8 text file inside the active workspace.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            });
        }
        if self.permissions.run_command {
            tools.push(ToolDefinition {
                name: "run_command".into(),
                description: "Run a command in the workspace with arguments, no implicit shell."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "args": {"type": "array", "items": {"type": "string"}},
                        "timeout_ms": {"type": "integer", "minimum": 1000, "maximum": 120000}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
            });
        }
        tools.retain(|definition| {
            LocalTool::from_name(&definition.name).is_some_and(|tool| self.permissions.allows(tool))
        });
        tools
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        if context.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let Some(tool) = LocalTool::from_name(&call.name) else {
            return Ok(ToolOutput::error(format!("unknown tool: {}", call.name)));
        };
        if !self.permissions.allows(tool) {
            return Ok(ToolOutput::error(format!("disabled tool: {}", call.name)));
        }
        match call.name.as_str() {
            "read_file" => {
                let path = required_string(&call.arguments, "path")?;
                let path = existing_workspace_path(&context.workspace, path)?;
                let metadata = fs::metadata(&path).map_err(tool_io_error)?;
                if !metadata.is_file() {
                    return Ok(ToolOutput::error("path is not a file"));
                }
                if metadata.len() > MAX_READ_BYTES {
                    return Ok(ToolOutput::error(format!(
                        "file exceeds the {MAX_READ_BYTES} byte read limit"
                    )));
                }
                let text = fs::read_to_string(path).map_err(tool_io_error)?;
                Ok(ToolOutput::success(text))
            }
            "list_files" => {
                let path = call
                    .arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(".");
                let path = existing_workspace_path(&context.workspace, path)?;
                if !path.is_dir() {
                    return Ok(ToolOutput::error("path is not a directory"));
                }
                let mut entries = Vec::new();
                for entry in fs::read_dir(path).map_err(tool_io_error)? {
                    if context.cancel.is_cancelled() {
                        return Err(AgentError::Cancelled);
                    }
                    let entry = entry.map_err(tool_io_error)?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == ".git" {
                        continue;
                    }
                    let kind = if entry.file_type().map_err(tool_io_error)?.is_dir() {
                        "dir"
                    } else {
                        "file"
                    };
                    entries.push(format!("{kind}\t{name}"));
                    if entries.len() >= MAX_LIST_ENTRIES {
                        entries.push("... list truncated".into());
                        break;
                    }
                }
                entries.sort();
                Ok(ToolOutput::success(entries.join("\n")))
            }
            "search_files" => crate::search::search_files(call, context).await,
            "write_file" => {
                let path = required_string(&call.arguments, "path")?;
                let content = required_string(&call.arguments, "content")?;
                if content.len() > MAX_WRITE_BYTES {
                    return Ok(ToolOutput::error(format!(
                        "content exceeds the {MAX_WRITE_BYTES} byte write limit"
                    )));
                }
                let path = writable_workspace_path(&context.workspace, path)?;
                atomic_write(&path, content.as_bytes())?;
                Ok(ToolOutput::success("file written"))
            }
            "run_command" => crate::command::run_command(call, context).await,
            _ => unreachable!("all known local tools are handled above"),
        }
    }
}

pub(crate) fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, AgentError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Tool(format!("missing string argument: {key}")))
}

pub(crate) fn existing_workspace_path(
    workspace: &Path,
    requested: &str,
) -> Result<PathBuf, AgentError> {
    validate_relative_path(requested)?;
    let root = workspace.canonicalize().map_err(tool_io_error)?;
    let path = root.join(requested).canonicalize().map_err(tool_io_error)?;
    if !path.starts_with(&root) {
        return Err(AgentError::Tool("path is outside the workspace".into()));
    }
    Ok(path)
}

fn writable_workspace_path(workspace: &Path, requested: &str) -> Result<PathBuf, AgentError> {
    validate_relative_path(requested)?;
    let root = workspace.canonicalize().map_err(tool_io_error)?;
    let candidate = root.join(requested);
    let parent = candidate
        .parent()
        .ok_or_else(|| AgentError::Tool("path has no parent directory".into()))?
        .canonicalize()
        .map_err(tool_io_error)?;
    if !parent.starts_with(&root) {
        return Err(AgentError::Tool("path is outside the workspace".into()));
    }
    let name = candidate
        .file_name()
        .ok_or_else(|| AgentError::Tool("path has no file name".into()))?;
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AgentError::Tool(
            "writing through a symlink is not allowed".into(),
        )),
        Ok(metadata) if metadata.is_dir() => Err(AgentError::Tool("path is a directory".into())),
        Ok(_) => Ok(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(target),
        Err(error) => Err(tool_io_error(error)),
    }
}

fn validate_relative_path(requested: &str) -> Result<(), AgentError> {
    let path = Path::new(requested);
    if requested.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(AgentError::Tool(
            "tool paths must be relative to the workspace".into(),
        ));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AgentError> {
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Tool("path has no parent directory".into()))?;
    let temporary = parent.join(format!(".openide-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<(), AgentError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(tool_io_error)?;
        file.write_all(bytes).map_err(tool_io_error)?;
        file.sync_all().map_err(tool_io_error)?;
        drop(file);
        fs::rename(&temporary, path).map_err(tool_io_error)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn tool_io_error(error: std::io::Error) -> AgentError {
    AgentError::Tool(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_compatibility_permissions() {
        let defaults = LocalTools::new();
        let names = defaults
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "list_files", "search_files"]);
        let expanded = LocalTools::new().with_write_access(true);
        assert!(expanded.permissions().allows(LocalTool::WriteFile));
        assert!(expanded.permissions().allows(LocalTool::RunCommand));
        assert_eq!(expanded.definitions().len(), 5);
    }

    #[tokio::test]
    async fn independent_tool_permissions_hide_and_block_disabled_calls() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.txt"), "hello").unwrap();
        let tool = LocalTools::new()
            .with_tool_permission(LocalTool::ReadFile, false)
            .with_tool_permission(LocalTool::WriteFile, true);
        let names = tool
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["list_files", "search_files", "write_file"]);
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let read = ToolCall {
            id: "read".into(),
            name: "read_file".into(),
            arguments: json!({"path":"sample.txt"}),
        };
        let denied = tool.execute(&read, &context).await.unwrap();
        assert!(denied.is_error);
        assert!(denied.content.contains("disabled tool"));
        let command = ToolCall {
            id: "command".into(),
            name: "run_command".into(),
            arguments: json!({"command":"cmd.exe","args":["/C","echo","unexpected"]}),
        };
        assert!(tool.execute(&command, &context).await.unwrap().is_error);
    }

    #[test]
    fn partial_permission_config_uses_safe_defaults() {
        let permissions: ToolPermissions = serde_json::from_str("{\"write_file\":true}").unwrap();
        assert!(permissions.read_file);
        assert!(permissions.write_file);
        assert!(!permissions.run_command);
    }

    #[tokio::test]
    async fn rejects_workspace_escape() {
        let dir = tempfile::tempdir().unwrap();
        let tool = LocalTools::new();
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let call = ToolCall {
            id: "1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"../outside"}),
        };
        assert!(tool.execute(&call, &context).await.is_err());
    }

    #[tokio::test]
    async fn reads_workspace_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();
        let tool = LocalTools::new();
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let call = ToolCall {
            id: "1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"hello.txt"}),
        };
        assert_eq!(
            tool.execute(&call, &context).await.unwrap().content,
            "hello"
        );
    }

    #[tokio::test]
    async fn write_mode_replaces_file_and_rejects_parent_paths() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "old").unwrap();
        fs::write(outside.path().join("private.txt"), "private").unwrap();
        let tool = LocalTools::new().with_write_access(true);
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let update = ToolCall {
            id: "write-1".into(),
            name: "write_file".into(),
            arguments: json!({"path":"notes.txt","content":"new"}),
        };
        tool.execute(&update, &context).await.unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "new"
        );

        let escape = ToolCall {
            id: "write-2".into(),
            name: "write_file".into(),
            arguments: json!({"path":"../private.txt","content":"changed"}),
        };
        assert!(tool.execute(&escape, &context).await.is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("private.txt")).unwrap(),
            "private"
        );
    }

    #[tokio::test]
    async fn write_mode_rejects_symlink_target_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let private = outside.path().join("private.txt");
        fs::write(&private, "private").unwrap();
        let link = dir.path().join("link.txt");
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(&private, &link);
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&private, &link);
        if created.is_err() {
            return;
        }
        let tool = LocalTools::new().with_write_access(true);
        let call = ToolCall {
            id: "write-link".into(),
            name: "write_file".into(),
            arguments: json!({"path":"link.txt","content":"changed"}),
        };
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        assert!(tool.execute(&call, &context).await.is_err());
        assert_eq!(fs::read_to_string(private).unwrap(), "private");
    }
}
