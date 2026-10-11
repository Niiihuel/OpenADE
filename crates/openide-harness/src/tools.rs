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
pub(crate) use apply_patch::{is_non_deleting_workspace_patch, is_workspace_relative_edit_path};
#[cfg(test)]
#[path = "skill_reference_tool_tests.rs"]
mod skill_reference_tool_tests;
#[path = "persistent_command_tools.rs"]
pub(crate) mod persistent_command_tools;
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

    /// Trusted assertion that this implementation performs a nonmutating
    /// built-in read in Plan. Does not authorize concurrent scheduling.
    fn certifies_plan_read(&self, name: &str) -> bool {
        self.certifies_parallel_read(name)
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError>;

    /// Optional bounded live observations; the returned result remains the
    /// only authoritative tool output. Older executors need no adaptation.
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        _output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        self.execute(call, context).await
    }
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
    read_only_roots: Vec<PathBuf>,
    command_isolation: crate::CommandIsolation,
    allow_elevated_commands: bool,
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

    /// Trusted host roots for installed skill references. Only `read_file`
    /// accepts absolute paths beneath these roots; directory tools, writes and
    /// commands keep their existing workspace policy. Roots are revalidated
    /// on every read, including the opened file's final location.
    pub fn with_read_only_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.read_only_roots = roots.into_iter().filter(|root| root.is_absolute()).take(128).collect();
        self
    }

    fn readable_path(&self, workspace: &Path, requested: &str) -> Result<(PathBuf, PathBuf), AgentError> {
        match existing_workspace_path(workspace, requested) {
            Ok(path) => Ok((workspace.canonicalize().map_err(tool_io_error)?, path)),
            Err(workspace_error) => {
                let requested_path = Path::new(requested);
                if requested_path.is_absolute() {
                    for root in &self.read_only_roots {
                        if !requested_path.starts_with(root) {
                            continue;
                        }
                        let metadata = fs::symlink_metadata(root).map_err(tool_io_error)?;
                        if !metadata.is_dir() || is_link_or_reparse(&metadata)
                            || root.canonicalize().map_err(tool_io_error)? != *root
                        {
                            return Err(AgentError::Tool("installed skill directory changed".into()));
                        }
                        let path = existing_workspace_path(root, requested)?;
                        return Ok((root.clone(), path));
                    }
                }
                Err(workspace_error)
            }
        }
    }

    /// Choose command isolation from trusted host settings, never tool arguments.
    pub fn with_command_isolation(mut self, isolation: crate::CommandIsolation) -> Self {
        self.command_isolation = isolation;
        self
    }

    pub fn command_isolation(&self) -> crate::CommandIsolation {
        self.command_isolation
    }

    /// Trusted host opt-in after an explicit administrator-access confirmation.
    /// Each elevated command still uses Linux Polkit authentication; this does
    /// not elevate the application, file tools, or persistent commands.
    pub fn with_elevated_commands(mut self, allowed: bool) -> Self {
        self.allow_elevated_commands = allowed;
        self
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
            && self.command_isolation.is_available_on_host()
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
    async fn execute_streaming(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        output: crate::ToolOutputSender,
    ) -> Result<ToolOutput, AgentError> {
        if call.name != "run_command" || !self.permissions.run_command {
            return self.execute(call, context).await;
        }
        crate::command::run_command_streaming(
            call,
            context,
            self.command_isolation,
            self.allow_elevated_commands,
            Some(output),
        )
        .await
    }
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
                    "properties": {"path": {"type": "string", "minLength": 1, "description": "File path relative to the active workspace (for example src/main.rs), or an absolute path inside it."}},
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            ToolDefinition {
                name: "list_files".into(),
                description: "List direct children of a workspace directory.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "default": ".", "description": "Directory inside the active workspace. Omit path or use . to list the workspace root; an empty string also means the root. Relative paths are preferred; absolute paths must stay inside the workspace."}},
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
                        "path": {"type": "string", "default": ".", "description": "File or directory inside the active workspace. Omit path or use . to search the workspace root; an empty string also means the root. Relative paths are preferred; absolute paths must stay inside the workspace."}
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
                        "path": {"type": "string", "minLength": 1, "description": "File path relative to the active workspace, or an absolute path inside it. The parent directory must already exist. Paths containing .. or links are not allowed."},
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
            let mut definition = ToolDefinition {
                name: "run_command".into(),
                description: format!(
                    "{}{}",
                    self.command_isolation.run_command_description(),
                    crate::command_line::COMMAND_RULE
                ),
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
            };
            if self.allow_elevated_commands
                && self.command_isolation == crate::CommandIsolation::HostUnrestricted
                && cfg!(target_os = "linux")
            {
                definition.parameters["properties"]["elevated"] = json!({
                    "type": "boolean",
                    "description": "Execute this command as root using the desktop's Polkit authentication dialog. Only when administration is needed; no secrets or interactive input."
                });
                definition.description.push_str(" Set elevated=true only for administrator operations; Linux will request authorization through its graphical Polkit agent. Elevated commands are bounded to 120 seconds and cannot read stdin.");
            }
            tools.push(definition);
        }
        if !self.read_only_roots.is_empty() {
            if let Some(read) = tools.iter_mut().find(|definition| definition.name == "read_file") {
                read.description.push_str(" Also accepts absolute reference-file paths inside the installed skill directories provided in your instructions (read-only).");
                read.parameters["properties"]["path"]["description"] = json!("File path relative to the active workspace, absolute inside it, or an absolute reference-file path under an installed skill directory supplied in the instructions. Parent traversal and links are not allowed.");
            }
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
                let (root, path) = self.readable_path(&context.workspace, path)?;
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
                let requested = directory_path_argument(&call.arguments);
                let path = existing_workspace_path(&context.workspace, requested)?;
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
                crate::command::run_command_with_policy(
                    call,
                    context,
                    self.command_isolation,
                    self.allow_elevated_commands,
                )
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

/// Directory tools treat a missing or empty path as the workspace root. Do
/// not trim other strings: leading/trailing spaces can be real file names.
pub(crate) fn directory_path_argument(arguments: &Value) -> &str {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .unwrap_or(".")
}

pub(crate) fn existing_workspace_path(
    workspace: &Path,
    requested: &str,
) -> Result<PathBuf, AgentError> {
    let root = workspace.canonicalize().map_err(tool_io_error)?;
    let relative = workspace_relative_path(workspace, &root, requested)?;
    reject_reparse_components(&root, &relative)?;
    let path = root.join(relative).canonicalize().map_err(tool_io_error)?;
    if !path.starts_with(&root) {
        return Err(AgentError::Tool("path is outside the workspace".into()));
    }
    Ok(path)
}

/// Absolute paths are only another spelling of a path beneath the trusted
/// workspace. Strip a component-aware workspace prefix *before* resolving
/// links; an arbitrary outside alias pointing inward is still outside scope.
pub(crate) fn workspace_relative_path(
    workspace: &Path,
    root: &Path,
    requested: &str,
) -> Result<PathBuf, AgentError> {
    let path = Path::new(requested);
    if requested.is_empty()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(AgentError::Tool("path must name a location inside the workspace without parent traversal (..); use . for the workspace root".into()));
    }
    let relative = if path.is_absolute() {
        // Windows canonicalize uses verbatim prefixes; a user-facing absolute
        // path can instead start with the original workspace spelling.
        let original = std::path::absolute(workspace).map_err(tool_io_error)?;
        path.strip_prefix(root)
            .or_else(|_| path.strip_prefix(&original))
            .map_err(|_| AgentError::Tool("path is outside the workspace".into()))?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(AgentError::Tool(
            "path must be relative to the workspace or absolute inside it".into(),
        ));
    }
    Ok(if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    })
}

fn reject_reparse_components(root: &Path, relative: &Path) -> Result<(), AgentError> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(tool_io_error)?;
        if is_link_or_reparse(&metadata) {
            return Err(AgentError::Tool(
                "access through a link or reparse point is not allowed".into(),
            ));
        }
    }
    Ok(())
}

fn writable_workspace_path(workspace: &Path, requested: &str) -> Result<PathBuf, AgentError> {
    let root = workspace.canonicalize().map_err(tool_io_error)?;
    let relative = workspace_relative_path(workspace, &root, requested)?;
    if !relative
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
    {
        return Err(AgentError::Tool(
            "path must name a file inside the workspace".into(),
        ));
    }
    let relative_parent = relative.parent().unwrap_or_else(|| Path::new("."));
    reject_reparse_components(&root, relative_parent)?;
    let candidate = root.join(&relative);
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
        Ok(metadata) if is_link_or_reparse(&metadata) => Err(AgentError::Tool(
            "writing through a link or reparse point is not allowed".into(),
        )),
        Ok(metadata) if metadata.is_dir() => Err(AgentError::Tool("path is a directory".into())),
        Ok(_) => Ok(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(target),
        Err(error) => Err(tool_io_error(error)),
    }
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

#[cfg(target_os = "linux")]
fn verify_opened_file_in_workspace(file: &File, root: &Path) -> Result<(), AgentError> {
    use std::os::fd::AsRawFd;
    let opened = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(tool_io_error)?;
    if !opened.starts_with(root) {
        return Err(AgentError::Tool("opened file is outside the allowed directory".into()));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_opened_file_in_workspace(file: &File, root: &Path) -> Result<(), AgentError> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
    let mut buffer = [0u8; libc::MAXPATHLEN as usize];
    // F_GETPATH writes at most MAXPATHLEN bytes into this owned buffer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return Err(tool_io_error(std::io::Error::last_os_error()));
    }
    let end = buffer.iter().position(|byte| *byte == 0).ok_or_else(|| AgentError::Tool("cannot verify opened file location".into()))?;
    if !Path::new(std::ffi::OsStr::from_bytes(&buffer[..end])).starts_with(root) {
        return Err(AgentError::Tool("opened file is outside the allowed directory".into()));
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
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
    #[tokio::test]
    async fn directory_tools_accept_omitted_empty_dot_and_absolute_workspace_roots() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("README.txt"), "project needle").unwrap();
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let tool = LocalTools::new();
        for path in [
            None,
            Some(String::new()),
            Some(".".into()),
            Some("./".into()),
            Some(dir.path().to_string_lossy().into_owned()),
            Some(
                dir.path()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        ] {
            for name in ["list_files", "search_files"] {
                let mut arguments = json!({});
                if let Some(path) = &path {
                    arguments["path"] = json!(path);
                }
                if name == "search_files" {
                    arguments["pattern"] = json!("needle");
                }
                let result = tool
                    .execute(
                        &ToolCall {
                            id: "root".into(),
                            name: name.into(),
                            arguments,
                        },
                        &context,
                    )
                    .await
                    .unwrap();
                assert!(!result.is_error, "{name} {path:?}: {}", result.content);
                assert!(
                    result.content.contains("README.txt"),
                    "{name} {path:?}: {}",
                    result.content
                );
            }
        }
    }

    #[tokio::test]
    async fn absolute_in_workspace_files_can_be_read_written_and_searched() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        let file = dir.path().join("src").join("a file.txt");
        let context = ToolContext {
            workspace: dir.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let tools = LocalTools::new().with_write_access(true);
        tools
            .execute(
                &ToolCall {
                    id: "write-absolute".into(),
                    name: "write_file".into(),
                    arguments: json!({"path":file,"content":"new needle"}),
                },
                &context,
            )
            .await
            .unwrap();
        let read = ToolCall {
            id: "read-absolute".into(),
            name: "read_file".into(),
            arguments: json!({"path":file}),
        };
        assert_eq!(
            tools.execute(&read, &context).await.unwrap().content,
            "new needle"
        );
        let searched = tools
            .execute(
                &ToolCall {
                    id: "search-absolute".into(),
                    name: "search_files".into(),
                    arguments: json!({"path":file,"pattern":"needle"}),
                },
                &context,
            )
            .await
            .unwrap();
        assert!(searched.content.contains("a file.txt:1: new needle"));
        let disabled = LocalTools::new().with_tool_permission(LocalTool::ReadFile, false);
        assert!(disabled.execute(&read, &context).await.unwrap().is_error);
        for name in ["read_file", "write_file"] {
            assert!(tools
                .execute(
                    &ToolCall {
                        id: "empty-file".into(),
                        name: name.into(),
                        arguments: json!({"path":"", "content":"no"})
                    },
                    &context
                )
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn absolute_siblings_and_parent_traversal_stay_outside_file_tool_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("workspace-sibling");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        let secret = outside.join("private.txt");
        fs::write(&secret, "outside needle").unwrap();
        fs::write(root.join("inside.txt"), "inside").unwrap();
        let context = ToolContext {
            workspace: root.clone(),
            cancel: CancellationToken::new(),
        };
        // Full host command access does not change the file-tool boundary.
        let tools = LocalTools::new()
            .with_write_access(true)
            .with_command_isolation(crate::CommandIsolation::HostUnrestricted);
        for name in ["read_file", "list_files", "search_files", "write_file"] {
            for path in [
                secret.to_string_lossy().into_owned(),
                "../workspace-sibling/private.txt".into(),
                "sub/../inside.txt".into(),
                root.join("sub/../inside.txt")
                    .to_string_lossy()
                    .into_owned(),
            ] {
                let result = tools.execute(&ToolCall { id: "outside".into(), name: name.into(), arguments: json!({"path":path,"pattern":"needle","content":"overwritten"}) }, &context).await;
                assert!(result.is_err(), "{name} accepted {path:?}");
            }
        }
        assert_eq!(fs::read_to_string(&secret).unwrap(), "outside needle");
        assert_eq!(
            fs::read_to_string(root.join("inside.txt")).unwrap(),
            "inside"
        );
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn absolute_paths_do_not_bypass_link_component_checks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("private.txt"), "outside needle").unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        fs::write(root.path().join("real/inside.txt"), "inside needle").unwrap();
        for (target, link) in [
            (
                outside.path().to_path_buf(),
                root.path().join("outside-link"),
            ),
            (root.path().join("real"), root.path().join("inside-link")),
        ] {
            #[cfg(unix)]
            let created = std::os::unix::fs::symlink(&target, &link);
            #[cfg(windows)]
            let created = std::os::windows::fs::symlink_dir(&target, &link);
            if created.is_err() {
                return;
            }
        }
        let tools = LocalTools::new().with_write_access(true);
        let context = ToolContext {
            workspace: root.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        for (link, file) in [
            ("outside-link", "private.txt"),
            ("inside-link", "inside.txt"),
        ] {
            for name in ["read_file", "list_files", "search_files", "write_file"] {
                let path = if matches!(name, "list_files" | "search_files") {
                    root.path().join(link)
                } else {
                    root.path().join(link).join(file)
                };
                let result = tools.execute(&ToolCall { id: "absolute-link".into(), name: name.into(), arguments: json!({"path":path,"pattern":"needle","content":"overwritten"}) }, &context).await;
                assert!(
                    result.is_err(),
                    "{name} accepted linked absolute path {path:?}"
                );
            }
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("private.txt")).unwrap(),
            "outside needle"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("real/inside.txt")).unwrap(),
            "inside needle"
        );
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn selected_workspace_alias_accepts_its_own_absolute_spelling() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("readme.txt"), "project").unwrap();
        let alias = temp.path().join("workspace");
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&real, &alias);
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&real, &alias);
        if created.is_err() {
            return;
        }
        let context = ToolContext {
            workspace: alias.clone(),
            cancel: CancellationToken::new(),
        };
        let result = LocalTools::new()
            .execute(
                &ToolCall {
                    id: "workspace-alias".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":alias.join("readme.txt")}),
                },
                &context,
            )
            .await
            .unwrap();
        assert_eq!(result.content, "project");
        // A separate outside alias must not become an accepted entry point.
        let other_alias = temp.path().join("unselected-alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &other_alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real, &other_alias).unwrap();
        assert!(LocalTools::new()
            .execute(
                &ToolCall {
                    id: "outside-alias".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":other_alias.join("readme.txt")})
                },
                &context
            )
            .await
            .is_err());
    }
}
