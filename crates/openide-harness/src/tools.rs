use crate::AgentError;
use async_trait::async_trait;
use openide_core::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[path = "apply_patch.rs"]
mod apply_patch;
#[path = "persistent_command_tools.rs"]
mod persistent_command_tools;
pub(crate) use persistent_command_tools::legacy_start_command_definition;

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

    /// Trusted host assertion that this implementation handles the named
    /// built-in tool without a mutating effect and permits overlapping reads.
    /// Composite and third-party executors stay exclusive until they opt in
    /// with equivalent implementation-level evidence.
    fn certifies_parallel_read(&self, _name: &str) -> bool {
        false
    }

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
            "write_file" | "apply_patch" => Some(Self::WriteFile),
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
    command_isolation: crate::CommandIsolation,
    persistent_commands: Option<Arc<crate::PersistentCommandManager>>,
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

    /// Choose command isolation from trusted host settings, never tool arguments.
    pub fn with_command_isolation(mut self, isolation: crate::CommandIsolation) -> Self {
        self.command_isolation = isolation;
        self
    }

    pub fn command_isolation(&self) -> crate::CommandIsolation {
        self.command_isolation
    }

    /// Attach a manager owned by this native session. This does not enable
    /// persistent tools unless run_command is allowed and the manager's
    /// isolation matches this tool executor.
    pub fn with_persistent_commands(
        mut self,
        manager: Arc<crate::PersistentCommandManager>,
    ) -> Self {
        self.persistent_commands = Some(manager);
        self
    }

    fn persistent_commands_enabled(&self) -> bool {
        self.permissions.run_command
            && (self.command_isolation == crate::CommandIsolation::HostUnrestricted
                || cfg!(windows))
            && self
                .persistent_commands
                .as_ref()
                .is_some_and(|manager| manager.isolation() == self.command_isolation)
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
    fn certifies_parallel_read(&self, name: &str) -> bool {
        matches!(
            LocalTool::from_name(name),
            Some(LocalTool::ReadFile | LocalTool::ListFiles | LocalTool::SearchFiles)
        ) && LocalTool::from_name(name).is_some_and(|tool| self.permissions.allows(tool))
    }

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
                        "pattern": {"type": "string", "maxLength": 16384},
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
            tools.push(ToolDefinition {
                name: "apply_patch".into(),
                description: "Apply a bounded, context-verified text patch to files inside the workspace. Supports Add, Delete, and Update hunks. The whole patch is validated before changes begin; Move to is not supported.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "patch": {"type": "string", "minLength": 1, "maxLength": 262144}
                    },
                    "required": ["patch"],
                    "additionalProperties": false
                }),
            });
        }
        if self.permissions.run_command {
            tools.push(ToolDefinition {
                name: "run_command".into(),
                description: match self.command_isolation {
                    crate::CommandIsolation::HostUnrestricted => "Run a command with the host user's filesystem and network rights; no implicit shell.",
                    crate::CommandIsolation::WorkspaceWriteOffline => "Run a command inside a Windows workspace-write/offline OS sandbox; no implicit shell.",
                }
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
        if self.persistent_commands_enabled() {
            let manager = self
                .persistent_commands
                .as_ref()
                .expect("enabled persistent commands have a manager");
            tools.extend(persistent_command_tools::definitions(manager));
        }
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
        if persistent_command_tools::is_persistent_tool(&call.name) {
            if !self.persistent_commands_enabled() {
                return Ok(ToolOutput::error(format!("disabled tool: {}", call.name)));
            }
            let manager = self
                .persistent_commands
                .as_ref()
                .expect("enabled persistent commands have a manager");
            return persistent_command_tools::execute(manager, call, context).await;
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
                let root = context.workspace.canonicalize().map_err(tool_io_error)?;
                reject_reparse_components(&root, path)?;
                let path = existing_workspace_path(&root, path)?;
                let metadata = fs::symlink_metadata(&path).map_err(tool_io_error)?;
                if is_link_or_reparse(&metadata) {
                    return Ok(ToolOutput::error("reading through a link is not allowed"));
                }
                if !metadata.is_file() {
                    return Ok(ToolOutput::error("path is not a file"));
                }
                if metadata.len() > MAX_READ_BYTES {
                    return Ok(ToolOutput::error(format!(
                        "file exceeds the {MAX_READ_BYTES} byte read limit"
                    )));
                }
                let file = open_no_follow(&path).map_err(tool_io_error)?;
                let opened = file.metadata().map_err(tool_io_error)?;
                if !opened.is_file() || is_link_or_reparse(&opened) {
                    return Ok(ToolOutput::error("reading through a link is not allowed"));
                }
                verify_opened_file_in_workspace(&file, &root)?;
                let mut bytes = Vec::new();
                file.take(MAX_READ_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(tool_io_error)?;
                if bytes.len() as u64 > MAX_READ_BYTES {
                    return Ok(ToolOutput::error(format!(
                        "file exceeds the {MAX_READ_BYTES} byte read limit"
                    )));
                }
                let text = String::from_utf8(bytes)
                    .map_err(|error| AgentError::Tool(error.to_string()))?;
                Ok(ToolOutput::success(text))
            }
            "list_files" => {
                let path = call
                    .arguments
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(".");
                let root = context.workspace.canonicalize().map_err(tool_io_error)?;
                reject_reparse_components(&root, path)?;
                let path = existing_workspace_path(&root, path)?;
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
            "apply_patch" => {
                let patch = required_string(&call.arguments, "patch")?;
                apply_patch::apply_patch(&context.workspace, patch, &context.cancel)
            }
            "run_command" => {
                crate::command::run_command_with_isolation(call, context, self.command_isolation)
                    .await
            }
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

fn reject_reparse_components(root: &Path, requested: &str) -> Result<(), AgentError> {
    validate_relative_path(requested)?;
    let mut path = root.to_path_buf();
    for component in Path::new(requested).components() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(tool_io_error)?;
        if is_link_or_reparse(&metadata) {
            return Err(AgentError::Tool(
                "reading through a link or reparse point is not allowed".into(),
            ));
        }
    }
    Ok(())
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

fn open_no_follow(path: &Path) -> std::io::Result<File> {
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

#[cfg(windows)]
fn verify_opened_file_in_workspace(file: &File, root: &Path) -> Result<(), AgentError> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let handle = file.as_raw_handle();
    let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if needed == 0 || needed > 32_768 {
        return Err(AgentError::Tool(
            "cannot verify opened file location".into(),
        ));
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    let written =
        unsafe { GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) };
    if written == 0 || written as usize >= buffer.len() {
        return Err(AgentError::Tool(
            "cannot verify opened file location".into(),
        ));
    }
    let opened = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..written as usize]));
    if !opened.starts_with(root) {
        return Err(AgentError::Tool("path is outside the workspace".into()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn verify_opened_file_in_workspace(_file: &File, _workspace: &Path) -> Result<(), AgentError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_compatibility_permissions() {
        let defaults = LocalTools::new();
        assert_eq!(
            defaults.command_isolation(),
            crate::CommandIsolation::HostUnrestricted
        );
        let names = defaults
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["read_file", "list_files", "search_files"]);
        let expanded = LocalTools::new().with_write_access(true);
        assert!(expanded.permissions().allows(LocalTool::WriteFile));
        assert!(expanded.permissions().allows(LocalTool::RunCommand));
        assert_eq!(expanded.definitions().len(), 6);
        assert!(expanded
            .definitions()
            .iter()
            .any(|tool| tool.name == "apply_patch"));
        assert_eq!(
            LocalTool::from_name("apply_patch"),
            Some(LocalTool::WriteFile)
        );
    }

    #[test]
    fn only_enabled_builtin_reads_certify_parallel_execution() {
        let defaults = LocalTools::new();
        for name in ["read_file", "list_files", "search_files"] {
            assert!(defaults.certifies_parallel_read(name));
        }
        for name in [
            "write_file",
            "apply_patch",
            "run_command",
            "mcp__server__read",
            "unknown",
        ] {
            assert!(!defaults.certifies_parallel_read(name));
        }
        let disabled = defaults.with_tool_permission(LocalTool::ReadFile, false);
        assert!(!disabled.certifies_parallel_read("read_file"));
        assert!(disabled.certifies_parallel_read("list_files"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn command_isolation_choice_reaches_executor() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let probe = crate::command::build_sandbox_probe(&workspace);
        let outside = dir.path().join("outside.txt");
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_command_isolation(crate::CommandIsolation::WorkspaceWriteOffline);
        assert!(tools
            .definitions()
            .iter()
            .any(|definition| definition.name == "run_command"
                && definition.description.contains("offline OS sandbox")));
        let call = ToolCall {
            id: "sandbox-dispatch".into(),
            name: "run_command".into(),
            arguments: json!({"command":probe,"args":["--write",outside]}),
        };
        let output = tools
            .execute(
                &call,
                &ToolContext {
                    workspace,
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("write=Err("), "{}", output.content);
        assert!(!dir.path().join("outside.txt").exists());
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
        assert_eq!(
            names,
            vec!["list_files", "search_files", "write_file", "apply_patch"]
        );
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
    async fn read_file_rejects_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("huge.txt"),
            vec![b'x'; MAX_READ_BYTES as usize + 1],
        )
        .unwrap();
        let output = LocalTools::new()
            .execute(
                &ToolCall {
                    id: "large-read".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"huge.txt"}),
                },
                &ToolContext {
                    workspace: dir.path().to_path_buf(),
                    cancel: CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("read limit"));
    }

    #[tokio::test]
    async fn read_and_list_reject_linked_workspace_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), "inside").unwrap();
        fs::create_dir(dir.path().join("real-dir")).unwrap();
        #[cfg(windows)]
        let linked_file = std::os::windows::fs::symlink_file(
            dir.path().join("real.txt"),
            dir.path().join("linked.txt"),
        );
        #[cfg(unix)]
        let linked_file =
            std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("linked.txt"));
        #[cfg(windows)]
        let linked_dir = std::os::windows::fs::symlink_dir(
            dir.path().join("real-dir"),
            dir.path().join("linked-dir"),
        );
        #[cfg(unix)]
        let linked_dir =
            std::os::unix::fs::symlink(dir.path().join("real-dir"), dir.path().join("linked-dir"));
        if linked_file.is_err() || linked_dir.is_err() {
            return;
        }
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        for (name, path) in [("read_file", "linked.txt"), ("list_files", "linked-dir")] {
            let result = LocalTools::new()
                .execute(
                    &ToolCall {
                        id: path.into(),
                        name: name.into(),
                        arguments: json!({"path":path}),
                    },
                    &context,
                )
                .await;
            assert!(result.is_err(), "{name} unexpectedly followed {path}");
        }
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
    async fn apply_patch_dispatch_uses_write_permission() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "old\n").unwrap();
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let call = ToolCall {
            id: "patch-1".into(),
            name: "apply_patch".into(),
            arguments: json!({"patch":"*** Begin Patch\n*** Update File: notes.txt\n@@\n-old\n+new\n*** End Patch"}),
        };
        let disabled = LocalTools::new().execute(&call, &context).await.unwrap();
        assert!(disabled.is_error);
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "old\n"
        );
        let enabled = LocalTools::new()
            .with_tool_permission(LocalTool::WriteFile, true)
            .execute(&call, &context)
            .await
            .unwrap();
        assert!(!enabled.is_error);
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "new\n"
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
