//! Bounded, ordered discovery of hierarchical workspace AGENTS.md instructions.
//!
//! The caller supplies the authorized workspace and active cwd, optionally
//! selecting a deeper target directory. A nested Git root inside that
//! workspace narrows discovery; traversal never crosses the workspace
//! boundary. This module only loads documents: the caller decides how to
//! combine them with host and user instructions for a model request.

use crate::{AgentError, ToolOutput};
use openide_core::{AgentEvent, AgentEventKind, AgentMessage, ToolCall};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

const ACTIVATION_PREFIX: &str = "OPENIDE_PROJECT_INSTRUCTIONS_V1\n";
pub(crate) const ACTIVATION_HOOK_PREFIX: &str = "openide-project-instructions-v1:";
const MAX_ACTIVE_SCOPE_BYTES: usize = 32 * 1024;
const MAX_ACTIVE_SCOPES: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ScopedInstructions {
    directory: PathBuf,
    root: PathBuf,
    text: String,
    replaces_outer: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActivationBatch {
    version: u16,
    call_digest: String,
    operation_not_executed_retry_required: bool,
    scopes: Vec<ScopedInstructions>,
}

/// Project rules admitted by earlier tool results in the current turn. The
/// journaled tool result is the source of truth after reopening or compaction.
#[derive(Default)]
pub(crate) struct ProjectInstructionLedger {
    scopes: Vec<ScopedInstructions>,
    directories: HashSet<PathBuf>,
}

impl ProjectInstructionLedger {
    pub(crate) fn from_events(
        events: &[AgentEvent],
        turn_start_sequence: u64,
        workspace: &Path,
    ) -> Result<Self, AgentError> {
        let mut ledger = Self::default();
        let mut calls = HashMap::new();
        let mut pending_markers = HashMap::new();
        let mut authenticated_markers = HashMap::new();
        for event in events
            .iter()
            .filter(|event| event.sequence > turn_start_sequence)
        {
            match &event.kind {
                AgentEventKind::ToolStarted { tool_call } => {
                    calls.insert(tool_call.id.as_str(), tool_call);
                }
                AgentEventKind::HookStarted {
                    tool_call_id,
                    hook_id,
                    phase,
                } if phase == "pre_tool_use" && hook_id.starts_with(ACTIVATION_HOOK_PREFIX) => {
                    pending_markers.insert(tool_call_id.as_str(), hook_id.as_str());
                }
                AgentEventKind::HookFinished {
                    tool_call_id,
                    hook_id,
                    phase,
                    decision,
                } if phase == "pre_tool_use"
                    && decision == "allow"
                    && pending_markers.remove(tool_call_id.as_str()) == Some(hook_id.as_str()) =>
                {
                    authenticated_markers.insert(tool_call_id.as_str(), hook_id.as_str());
                }
                AgentEventKind::ToolFinished {
                    tool_call_id,
                    output,
                    is_error,
                } => {
                    let Some(marker) = authenticated_markers.remove(tool_call_id.as_str()) else {
                        // Tool output alone is never promoted to trusted context.
                        continue;
                    };
                    if !is_error
                        || !output.starts_with(ACTIVATION_PREFIX)
                        || activation_hook_id(output).as_deref() != Some(marker)
                    {
                        return Err(AgentError::Runtime(
                            "project instruction snapshot marker is invalid".into(),
                        ));
                    }
                    let call = calls.get(tool_call_id.as_str()).ok_or_else(|| {
                        AgentError::Runtime("project instruction result has no tool call".into())
                    })?;
                    let batch: ActivationBatch = serde_json::from_str(
                        &output[ACTIVATION_PREFIX.len()..],
                    )
                    .map_err(|error| {
                        AgentError::Runtime(format!("project instruction snapshot: {error}"))
                    })?;
                    if batch.version != 1
                        || batch.call_digest != call_digest(call)
                        || !batch.operation_not_executed_retry_required
                        || batch.scopes.is_empty()
                        || !matches!(
                            call.name.as_str(),
                            "read_file"
                                | "write_file"
                                | "list_files"
                                | "search_files"
                                | "apply_patch"
                        )
                    {
                        return Err(AgentError::Runtime(
                            "project instruction snapshot has invalid provenance".into(),
                        ));
                    }
                    ledger.add_scopes(batch.scopes, workspace)?;
                }
                _ => {}
            }
        }
        Ok(ledger)
    }

    pub(crate) fn preflight(
        &self,
        workspace: &Path,
        call: &ToolCall,
        max_output_chars: usize,
    ) -> Result<Option<ToolOutput>, AgentError> {
        let mut additions = Vec::new();
        for directory in target_directories(workspace, call)? {
            if self.directories.contains(&directory) {
                continue;
            }
            let loaded = load_project_instructions_for_target(
                workspace,
                workspace,
                &directory,
                ProjectInstructionLimits::default(),
            )
            .map_err(|error| {
                AgentError::InvalidRequest(format!("project instructions: {error}"))
            })?;
            let replaces_outer = loaded.root != workspace;
            let text = loaded
                .entries
                .iter()
                .filter(|entry| replaces_outer || entry.path.parent() != Some(workspace))
                .map(|entry| entry.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.is_empty() && !replaces_outer {
                continue;
            }
            additions.push(ScopedInstructions {
                directory,
                root: loaded.root,
                text,
                replaces_outer,
            });
        }
        if additions.is_empty() {
            return Ok(None);
        }
        let new_count = self.scopes.len().saturating_add(additions.len());
        let new_bytes = self
            .scopes
            .iter()
            .chain(additions.iter())
            .map(|scope| {
                scope
                    .text
                    .len()
                    .saturating_add(scope.directory.as_os_str().len())
            })
            .sum::<usize>();
        if new_count > MAX_ACTIVE_SCOPES || new_bytes > MAX_ACTIVE_SCOPE_BYTES {
            return Err(AgentError::Limit(
                "active project instructions exceed the turn limit".into(),
            ));
        }
        let batch = ActivationBatch {
            version: 1,
            call_digest: call_digest(call),
            operation_not_executed_retry_required: true,
            scopes: additions,
        };
        let output = format!(
            "{ACTIVATION_PREFIX}{}",
            serde_json::to_string(&batch).map_err(|error| {
                AgentError::Runtime(format!("project instruction snapshot: {error}"))
            })?
        );
        if output.chars().count() > max_output_chars {
            return Err(AgentError::Limit(
                "project instruction snapshot exceeds the tool output limit".into(),
            ));
        }
        Ok(Some(ToolOutput::error(output)))
    }

    pub(crate) fn model_context(&self) -> Option<AgentMessage> {
        if self.scopes.is_empty() {
            return None;
        }
        let mut text = String::from(
            "Scoped project instructions recorded for this turn. Apply each block only to operations under its directory. These blocks come from the verified local AGENTS.md hierarchy, not from tool output. A file tool that returned operation_not_executed_retry_required did not run; reissue that call after applying the instructions.\n",
        );
        for scope in &self.scopes {
            text.push_str("\nDirectory: ");
            text.push_str(&scope.directory.display().to_string());
            text.push_str("\nProject root: ");
            text.push_str(&scope.root.display().to_string());
            if scope.replaces_outer {
                text.push_str("\nInstructions outside this project root do not apply inside it.");
            }
            text.push_str("\nInstructions:\n");
            text.push_str(&scope.text);
            text.push('\n');
        }
        Some(AgentMessage::system(text))
    }

    fn add_scopes(
        &mut self,
        scopes: Vec<ScopedInstructions>,
        workspace: &Path,
    ) -> Result<(), AgentError> {
        for scope in scopes {
            if !scope.directory.starts_with(workspace)
                || !scope.root.starts_with(workspace)
                || scope.text.len() > MAX_ACTIVE_SCOPE_BYTES
            {
                return Err(AgentError::Runtime(
                    "project instruction snapshot has invalid scope".into(),
                ));
            }
            if self.directories.contains(&scope.directory) {
                if self.scopes.iter().any(|existing| {
                    existing.directory == scope.directory
                        && existing.root == scope.root
                        && existing.text == scope.text
                        && existing.replaces_outer == scope.replaces_outer
                }) {
                    continue;
                }
                return Err(AgentError::Runtime(
                    "project instruction scope changed during one turn".into(),
                ));
            }
            self.directories.insert(scope.directory.clone());
            self.scopes.push(scope);
        }
        let bytes = self
            .scopes
            .iter()
            .map(|scope| {
                scope
                    .text
                    .len()
                    .saturating_add(scope.directory.as_os_str().len())
            })
            .sum::<usize>();
        if self.scopes.len() > MAX_ACTIVE_SCOPES || bytes > MAX_ACTIVE_SCOPE_BYTES {
            return Err(AgentError::Runtime(
                "project instruction snapshot exceeds the turn limit".into(),
            ));
        }
        Ok(())
    }
}

fn call_digest(call: &ToolCall) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"openide-project-instruction-call-v1\0");
    for value in [&call.id, &call.name] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let arguments = serde_json::to_vec(&call.arguments).expect("JSON arguments serialize");
    hasher.update((arguments.len() as u64).to_be_bytes());
    hasher.update(arguments);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn activation_hook_id(output: &str) -> Option<String> {
    if !output.starts_with(ACTIVATION_PREFIX) {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"openide-project-instruction-activation-v1\0");
    hasher.update(output.as_bytes());
    Some(format!("{ACTIVATION_HOOK_PREFIX}{:x}", hasher.finalize()))
}

fn target_directories(workspace: &Path, call: &ToolCall) -> Result<Vec<PathBuf>, AgentError> {
    let mut paths = Vec::new();
    match call.name.as_str() {
        "read_file" | "write_file" => {
            if let Some(path) = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            {
                paths.push((path, false));
            }
        }
        "list_files" | "search_files" => {
            paths.push((
                call.arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("."),
                true,
            ));
        }
        "apply_patch" => {
            if let Some(patch) = call
                .arguments
                .get("patch")
                .and_then(serde_json::Value::as_str)
            {
                for line in patch.lines() {
                    if let Some(path) = [
                        "*** Add File: ",
                        "*** Delete File: ",
                        "*** Update File: ",
                        "*** Move to: ",
                    ]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                    {
                        paths.push((path, false));
                    }
                }
            }
        }
        _ => return Ok(Vec::new()),
    }
    let mut directories = Vec::new();
    for (path, directory_hint) in paths {
        let mut target = workspace.to_path_buf();
        let relative = Path::new(path);
        if path.len() > 4096
            || path.chars().any(char::is_control)
            || relative.is_absolute()
            || relative.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            return Err(AgentError::InvalidRequest(
                "project instruction target is not a safe workspace path".into(),
            ));
        }
        target.push(relative);
        let target = if directory_hint {
            match fs::symlink_metadata(&target) {
                Ok(metadata) if is_link_or_reparse(&metadata) => {
                    return Err(AgentError::InvalidRequest(
                        "project instruction target traverses a link".into(),
                    ));
                }
                Ok(metadata) if metadata.is_dir() => target,
                _ => target.parent().unwrap_or(workspace).to_path_buf(),
            }
        } else {
            if let Ok(metadata) = fs::symlink_metadata(&target) {
                if is_link_or_reparse(&metadata) {
                    return Err(AgentError::InvalidRequest(
                        "project instruction target traverses a link".into(),
                    ));
                }
            }
            target.parent().unwrap_or(workspace).to_path_buf()
        };
        let mut existing = target;
        loop {
            match fs::symlink_metadata(&existing) {
                Ok(metadata) if is_link_or_reparse(&metadata) => {
                    return Err(AgentError::InvalidRequest(
                        "project instruction target traverses a link".into(),
                    ));
                }
                Ok(metadata) if metadata.is_dir() => break,
                Ok(_) => {
                    return Err(AgentError::InvalidRequest(
                        "project instruction target parent is not a directory".into(),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    existing = existing
                        .parent()
                        .ok_or_else(|| {
                            AgentError::InvalidRequest("project target escapes workspace".into())
                        })?
                        .to_path_buf();
                }
                Err(error) => {
                    return Err(AgentError::InvalidRequest(format!(
                        "project instruction target: {error}"
                    )));
                }
            }
        }
        if !existing.starts_with(workspace) {
            return Err(AgentError::InvalidRequest(
                "project target escapes workspace".into(),
            ));
        }
        verify_directory_chain(workspace, &existing).map_err(|error| {
            AgentError::InvalidRequest(format!("project instruction target: {error}"))
        })?;
        let canonical = existing
            .canonicalize()
            .map_err(|error| AgentError::InvalidRequest(format!("project target: {error}")))?;
        if !canonical.starts_with(workspace) {
            return Err(AgentError::InvalidRequest(
                "project target escapes workspace".into(),
            ));
        }
        if !directories.contains(&canonical) {
            directories.push(canonical);
        }
    }
    Ok(directories)
}

pub const AGENTS_MD_FILENAME: &str = "AGENTS.md";
pub const AGENTS_OVERRIDE_FILENAME: &str = "AGENTS.override.md";
pub const DEFAULT_PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;
pub const DEFAULT_PROJECT_DOC_MAX_LEVELS: usize = 64;
pub const DEFAULT_PROJECT_DOC_MAX_FILES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectInstructionLimits {
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_levels: usize,
    pub max_files: usize,
}

impl Default for ProjectInstructionLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_PROJECT_DOC_MAX_BYTES,
            max_total_bytes: DEFAULT_PROJECT_DOC_MAX_BYTES,
            max_levels: DEFAULT_PROJECT_DOC_MAX_LEVELS,
            max_files: DEFAULT_PROJECT_DOC_MAX_FILES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectInstructionEntry {
    /// Canonical path of the selected document.
    pub path: PathBuf,
    pub content: String,
    pub sha256_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProjectInstructions {
    /// Nearest `.git` root inside the workspace, or the workspace itself.
    pub root: PathBuf,
    /// Working directory supplied by the caller.
    pub cwd: PathBuf,
    /// Directory whose path determines the instruction hierarchy. Equal to
    /// `cwd` when the compatibility loader is used.
    pub directory: PathBuf,
    /// Nonblank documents in root-to-directory order, at most one per directory.
    pub entries: Vec<ProjectInstructionEntry>,
}

impl LoadedProjectInstructions {
    /// Model-visible project text in root-to-directory order.
    pub fn text(&self) -> String {
        self.entries
            .iter()
            .map(|entry| entry.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[derive(Debug, Error)]
pub enum ProjectInstructionsError {
    #[error("project instruction limits must all be greater than zero")]
    InvalidLimits,
    #[error("workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error("cwd is not a directory: {0}")]
    CwdNotDirectory(PathBuf),
    #[error("cwd {cwd} escapes workspace {workspace}", cwd = .cwd.display(), workspace = .workspace.display())]
    CwdOutsideWorkspace { workspace: PathBuf, cwd: PathBuf },
    #[error("target directory is not a directory: {0}")]
    TargetNotDirectory(PathBuf),
    #[error("target directory {target} escapes cwd {cwd}", target = .target.display(), cwd = .cwd.display())]
    TargetOutsideCwd { cwd: PathBuf, target: PathBuf },
    #[error("project instruction directory path cannot be safely verified: {0}")]
    UnverifiedDirectory(PathBuf),
    #[error("project instruction path {path} escapes directory {directory}", path = .path.display(), directory = .directory.display())]
    EscapesDirectory { directory: PathBuf, path: PathBuf },
    #[error("project instruction path uses a symlink or reparse point: {0}")]
    LinkedPath(PathBuf),
    #[error("project instruction path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("project instructions exceed {max_levels} directory levels")]
    TooManyLevels { max_levels: usize },
    #[error("project instructions exceed {max_files} selected files")]
    TooManyFiles { max_files: usize },
    #[error("project instruction file {path} exceeds {max_bytes} bytes", path = .path.display())]
    FileTooLarge { path: PathBuf, max_bytes: usize },
    #[error("project instructions exceed {max_bytes} total bytes")]
    TotalTooLarge { max_bytes: usize },
    #[error("project instruction file is not UTF-8: {0}")]
    InvalidUtf8(PathBuf),
    #[error("project instruction I/O error at {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Load at most one instruction document per directory, preferring
/// `AGENTS.override.md` over `AGENTS.md`. Existing unsafe or unreadable
/// candidates fail closed; missing candidates are skipped.
pub fn load_project_instructions(
    workspace: &Path,
    cwd: &Path,
    limits: ProjectInstructionLimits,
) -> Result<LoadedProjectInstructions, ProjectInstructionsError> {
    load_project_instructions_inner(workspace, cwd, cwd, limits)
}

/// Load the hierarchy for a target directory beneath `cwd`. This includes
/// instructions between `cwd` and the target, subject to the nearest nested
/// project marker. Pass a file's parent directory, not the file itself.
pub fn load_project_instructions_for_target(
    workspace: &Path,
    cwd: &Path,
    target_directory: &Path,
    limits: ProjectInstructionLimits,
) -> Result<LoadedProjectInstructions, ProjectInstructionsError> {
    load_project_instructions_inner(workspace, cwd, target_directory, limits)
}

fn load_project_instructions_inner(
    workspace: &Path,
    cwd: &Path,
    target_directory: &Path,
    limits: ProjectInstructionLimits,
) -> Result<LoadedProjectInstructions, ProjectInstructionsError> {
    if limits.max_file_bytes == 0
        || limits.max_total_bytes == 0
        || limits.max_levels == 0
        || limits.max_files == 0
    {
        return Err(ProjectInstructionsError::InvalidLimits);
    }
    verify_directory_chain(workspace, workspace)?;
    let cwd_lexically_inside = verify_directory_chain(workspace, cwd)?;
    let target_lexically_inside = verify_directory_chain(workspace, target_directory)?;
    let workspace = canonicalize(workspace)?;
    if !workspace.is_dir() {
        return Err(ProjectInstructionsError::WorkspaceNotDirectory(workspace));
    }
    let cwd = canonicalize(cwd)?;
    if !cwd.is_dir() {
        return Err(ProjectInstructionsError::CwdNotDirectory(cwd));
    }
    if !cwd.starts_with(&workspace) {
        return Err(ProjectInstructionsError::CwdOutsideWorkspace { workspace, cwd });
    }
    if !cwd_lexically_inside {
        return Err(ProjectInstructionsError::UnverifiedDirectory(cwd));
    }
    let directory = canonicalize(target_directory)?;
    if !directory.is_dir() {
        return Err(ProjectInstructionsError::TargetNotDirectory(directory));
    }
    if !directory.starts_with(&cwd) {
        return Err(ProjectInstructionsError::TargetOutsideCwd {
            cwd,
            target: directory,
        });
    }
    if !target_lexically_inside {
        return Err(ProjectInstructionsError::UnverifiedDirectory(directory));
    }

    let root = nearest_repo_root(&workspace, &directory)?;
    let directories = directories_root_to_target(&root, &directory, limits.max_levels)?;
    let mut entries = Vec::new();
    let mut total_bytes = 0usize;
    let mut selected_files = 0usize;
    for directory in directories {
        for name in [AGENTS_OVERRIDE_FILENAME, AGENTS_MD_FILENAME] {
            let path = directory.join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&path, source)),
            };
            if is_link_or_reparse(&metadata) {
                return Err(ProjectInstructionsError::LinkedPath(path));
            }
            if !metadata.is_file() {
                return Err(ProjectInstructionsError::NotRegularFile(path));
            }
            if selected_files == limits.max_files {
                return Err(ProjectInstructionsError::TooManyFiles {
                    max_files: limits.max_files,
                });
            }
            selected_files += 1;
            let canonical = canonicalize(&path)?;
            if canonical.parent() != Some(directory.as_path()) {
                return Err(ProjectInstructionsError::EscapesDirectory {
                    directory,
                    path: canonical,
                });
            }
            let bytes = read_bounded(&path, limits.max_file_bytes)?;
            total_bytes = total_bytes.checked_add(bytes.len()).ok_or(
                ProjectInstructionsError::TotalTooLarge {
                    max_bytes: limits.max_total_bytes,
                },
            )?;
            if total_bytes > limits.max_total_bytes {
                return Err(ProjectInstructionsError::TotalTooLarge {
                    max_bytes: limits.max_total_bytes,
                });
            }
            let content = String::from_utf8(bytes)
                .map_err(|_| ProjectInstructionsError::InvalidUtf8(path.clone()))?;
            if !content.trim().is_empty() {
                entries.push(ProjectInstructionEntry {
                    path: canonical,
                    sha256_hex: format!("{:x}", Sha256::digest(content.as_bytes())),
                    content,
                });
            }
            // An override takes precedence even if it is blank.
            break;
        }
    }
    Ok(LoadedProjectInstructions {
        root,
        cwd,
        directory,
        entries,
    })
}

fn nearest_repo_root(workspace: &Path, cwd: &Path) -> Result<PathBuf, ProjectInstructionsError> {
    let mut directory = cwd;
    loop {
        let marker = directory.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(metadata) if is_link_or_reparse(&metadata) => {
                return Err(ProjectInstructionsError::LinkedPath(marker));
            }
            Ok(metadata) if metadata.is_dir() || metadata.is_file() => {
                return Ok(directory.to_path_buf());
            }
            Ok(_) => return Err(ProjectInstructionsError::NotRegularFile(marker)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&marker, source)),
        }
        if directory == workspace {
            return Ok(workspace.to_path_buf());
        }
        directory =
            directory
                .parent()
                .ok_or_else(|| ProjectInstructionsError::CwdOutsideWorkspace {
                    workspace: workspace.to_path_buf(),
                    cwd: cwd.to_path_buf(),
                })?;
    }
}

/// Inspect every descendant component before canonicalization erases a linked
/// alias. A candidate outside the lexical workspace is checked by the caller
/// against the canonical workspace so ordinary outside paths keep their error.
fn verify_directory_chain(
    workspace: &Path,
    candidate: &Path,
) -> Result<bool, ProjectInstructionsError> {
    use std::path::Component;

    if workspace
        .components()
        .chain(candidate.components())
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(ProjectInstructionsError::UnverifiedDirectory(
            candidate.to_path_buf(),
        ));
    }
    let workspace_absolute =
        std::path::absolute(workspace).map_err(|source| io_error(workspace, source))?;
    let candidate_absolute =
        std::path::absolute(candidate).map_err(|source| io_error(candidate, source))?;
    let metadata = fs::symlink_metadata(&workspace_absolute)
        .map_err(|source| io_error(&workspace_absolute, source))?;
    if is_link_or_reparse(&metadata) {
        return Err(ProjectInstructionsError::LinkedPath(workspace_absolute));
    }
    let Ok(relative) = candidate_absolute.strip_prefix(&workspace_absolute) else {
        return Ok(false);
    };
    let mut path = workspace_absolute;
    for part in relative.components() {
        let Component::Normal(name) = part else {
            return Err(ProjectInstructionsError::UnverifiedDirectory(
                candidate.to_path_buf(),
            ));
        };
        path.push(name);
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if is_link_or_reparse(&metadata) {
            return Err(ProjectInstructionsError::LinkedPath(path));
        }
    }
    Ok(true)
}

fn directories_root_to_target(
    root: &Path,
    cwd: &Path,
    max_levels: usize,
) -> Result<Vec<PathBuf>, ProjectInstructionsError> {
    let mut reversed = Vec::new();
    let mut directory = cwd;
    loop {
        if reversed.len() == max_levels {
            return Err(ProjectInstructionsError::TooManyLevels { max_levels });
        }
        reversed.push(directory.to_path_buf());
        if directory == root {
            reversed.reverse();
            return Ok(reversed);
        }
        directory =
            directory
                .parent()
                .ok_or_else(|| ProjectInstructionsError::EscapesDirectory {
                    directory: root.to_path_buf(),
                    path: cwd.to_path_buf(),
                })?;
    }
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, ProjectInstructionsError> {
    let file = open_no_follow(path).map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if is_link_or_reparse(&metadata) {
        return Err(ProjectInstructionsError::LinkedPath(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(ProjectInstructionsError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(ProjectInstructionsError::FileTooLarge {
            path: path.to_path_buf(),
            max_bytes,
        });
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() > max_bytes {
        return Err(ProjectInstructionsError::FileTooLarge {
            path: path.to_path_buf(),
            max_bytes,
        });
    }
    Ok(bytes)
}

fn open_no_follow(path: &Path) -> io::Result<File> {
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

fn canonicalize(path: &Path) -> Result<PathBuf, ProjectInstructionsError> {
    fs::canonicalize(path).map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> ProjectInstructionsError {
    ProjectInstructionsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn root_to_cwd_order_and_override_precedence_are_stable() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join(".git")).unwrap();
        let first = workspace.path().join("src");
        let cwd = first.join("nested");
        fs::create_dir_all(&cwd).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "root");
        write(&first.join(AGENTS_MD_FILENAME), "ignored");
        write(&first.join(AGENTS_OVERRIDE_FILENAME), "override");
        write(&cwd.join(AGENTS_MD_FILENAME), "leaf");
        let loaded =
            load_project_instructions(workspace.path(), &cwd, ProjectInstructionLimits::default())
                .unwrap();
        assert_eq!(loaded.root, fs::canonicalize(workspace.path()).unwrap());
        assert_eq!(loaded.entries.len(), 3);
        assert_eq!(loaded.text(), "root\n\noverride\n\nleaf");
        assert_eq!(
            loaded.entries[1].path.file_name().unwrap(),
            AGENTS_OVERRIDE_FILENAME
        );
        assert_eq!(loaded.entries[0].sha256_hex.len(), 64);
        assert_eq!(loaded.directory, loaded.cwd);
    }

    #[test]
    fn target_directory_extends_the_cwd_hierarchy_without_changing_old_api() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join(".git")).unwrap();
        let cwd = workspace.path().join("src");
        let target = cwd.join("nested");
        fs::create_dir_all(&target).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "root");
        write(&cwd.join(AGENTS_MD_FILENAME), "cwd");
        write(&target.join(AGENTS_MD_FILENAME), "target");
        let legacy =
            load_project_instructions(workspace.path(), &cwd, ProjectInstructionLimits::default())
                .unwrap();
        assert_eq!(legacy.text(), "root\n\ncwd");
        let targeted = load_project_instructions_for_target(
            workspace.path(),
            &cwd,
            &target,
            ProjectInstructionLimits::default(),
        )
        .unwrap();
        assert_eq!(targeted.text(), "root\n\ncwd\n\ntarget");
        assert_eq!(targeted.cwd, fs::canonicalize(cwd).unwrap());
        assert_eq!(targeted.directory, fs::canonicalize(target).unwrap());
    }

    #[test]
    fn target_must_be_beneath_cwd_and_selected_file_count_is_bounded() {
        let workspace = tempfile::tempdir().unwrap();
        let cwd = workspace.path().join("src");
        let target = cwd.join("nested");
        let sibling = workspace.path().join("sibling");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir(&sibling).unwrap();
        write(&workspace.path().join(AGENTS_OVERRIDE_FILENAME), " \n");
        write(&cwd.join(AGENTS_MD_FILENAME), "cwd");
        write(&target.join(AGENTS_MD_FILENAME), "target");
        assert!(matches!(
            load_project_instructions_for_target(
                workspace.path(),
                &cwd,
                &sibling,
                ProjectInstructionLimits::default(),
            ),
            Err(ProjectInstructionsError::TargetOutsideCwd { .. })
        ));
        let limits = ProjectInstructionLimits {
            max_files: 2,
            ..ProjectInstructionLimits::default()
        };
        assert!(matches!(
            load_project_instructions_for_target(workspace.path(), &cwd, &target, limits),
            Err(ProjectInstructionsError::TooManyFiles { max_files: 2 })
        ));
    }

    #[test]
    fn nearest_nested_repo_stops_outer_document_and_workspace_fallback_works() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("nested");
        let cwd = nested.join("src");
        fs::create_dir_all(&cwd).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "outer");
        write(&nested.join(AGENTS_MD_FILENAME), "inner");
        let fallback =
            load_project_instructions(workspace.path(), &cwd, ProjectInstructionLimits::default())
                .unwrap();
        assert_eq!(fallback.text(), "outer\n\ninner");
        fs::create_dir(nested.join(".git")).unwrap();
        let nested_repo =
            load_project_instructions(workspace.path(), &cwd, ProjectInstructionLimits::default())
                .unwrap();
        assert_eq!(nested_repo.root, fs::canonicalize(nested).unwrap());
        assert_eq!(nested_repo.text(), "inner");
    }

    #[test]
    fn worktree_git_file_is_a_root_marker_and_missing_docs_are_empty() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("worktree");
        fs::create_dir(&nested).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "outer");
        write(&nested.join(".git"), "gitdir: ../.git/worktrees/example");
        let loaded = load_project_instructions(
            workspace.path(),
            &nested,
            ProjectInstructionLimits::default(),
        )
        .unwrap();
        assert_eq!(loaded.root, fs::canonicalize(nested).unwrap());
        assert!(loaded.entries.is_empty());
        assert!(loaded.text().is_empty());
    }

    #[test]
    fn file_total_depth_and_utf8_limits_fail_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let child = workspace.path().join("child");
        fs::create_dir(&child).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "12345");
        write(&child.join(AGENTS_MD_FILENAME), "67890");
        let limits = ProjectInstructionLimits {
            max_file_bytes: 4,
            max_total_bytes: 20,
            max_levels: 2,
            max_files: 2,
        };
        assert!(matches!(
            load_project_instructions(workspace.path(), &child, limits),
            Err(ProjectInstructionsError::FileTooLarge { .. })
        ));
        let limits = ProjectInstructionLimits {
            max_file_bytes: 5,
            max_total_bytes: 9,
            max_levels: 2,
            max_files: 2,
        };
        assert!(matches!(
            load_project_instructions(workspace.path(), &child, limits),
            Err(ProjectInstructionsError::TotalTooLarge { .. })
        ));
        let limits = ProjectInstructionLimits {
            max_file_bytes: 5,
            max_total_bytes: 10,
            max_levels: 1,
            max_files: 2,
        };
        assert!(matches!(
            load_project_instructions(workspace.path(), &child, limits),
            Err(ProjectInstructionsError::TooManyLevels { .. })
        ));
        fs::write(child.join(AGENTS_MD_FILENAME), [0xff]).unwrap();
        assert!(matches!(
            load_project_instructions(
                workspace.path(),
                &child,
                ProjectInstructionLimits::default()
            ),
            Err(ProjectInstructionsError::InvalidUtf8(_))
        ));
    }

    #[test]
    fn outside_cwd_and_prefix_collision_are_rejected() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let sibling = parent.path().join("workspace-other");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&sibling).unwrap();
        assert!(matches!(
            load_project_instructions(&workspace, &sibling, ProjectInstructionLimits::default()),
            Err(ProjectInstructionsError::CwdOutsideWorkspace { .. })
        ));
    }

    #[test]
    fn blank_override_hides_regular_agents_file() {
        let workspace = tempfile::tempdir().unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "regular");
        write(&workspace.path().join(AGENTS_OVERRIDE_FILENAME), " \n");
        let loaded = load_project_instructions(
            workspace.path(),
            workspace.path(),
            ProjectInstructionLimits::default(),
        )
        .unwrap();
        assert!(loaded.entries.is_empty());
        assert_eq!(loaded.text(), "");
    }

    #[test]
    fn linked_document_outside_workspace_is_rejected_when_supported() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        write(&secret, "outside");
        let candidate = workspace.path().join(AGENTS_MD_FILENAME);
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&secret, &candidate);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&secret, &candidate);
        if let Err(error) = link_result {
            if error.kind() == io::ErrorKind::PermissionDenied
                || cfg!(windows) && error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("cannot create test symlink: {error}");
        }
        assert!(matches!(
            load_project_instructions(
                workspace.path(),
                workspace.path(),
                ProjectInstructionLimits::default()
            ),
            Err(ProjectInstructionsError::LinkedPath(_))
        ));
    }

    #[test]
    fn linked_directory_in_target_path_is_rejected_when_supported() {
        let workspace = tempfile::tempdir().unwrap();
        let cwd = workspace.path().join("src");
        let real = cwd.join("real");
        let link = cwd.join("alias");
        fs::create_dir_all(&real).unwrap();
        write(&real.join(AGENTS_MD_FILENAME), "real");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&real, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(&real, &link);
        if let Err(error) = link_result {
            if error.kind() == io::ErrorKind::PermissionDenied
                || cfg!(windows) && error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("cannot create test symlink: {error}");
        }
        assert!(matches!(
            load_project_instructions_for_target(
                workspace.path(),
                &cwd,
                &link,
                ProjectInstructionLimits::default(),
            ),
            Err(ProjectInstructionsError::LinkedPath(_))
        ));
    }

    #[test]
    fn nested_file_rules_activate_only_for_the_real_component_prefix() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let nested = workspace.join("src");
        let sibling = workspace.join("src-other");
        fs::create_dir_all(nested.join("deep")).unwrap();
        fs::create_dir(&sibling).unwrap();
        write(&workspace.join(AGENTS_MD_FILENAME), "root rule");
        write(&nested.join(AGENTS_MD_FILENAME), "nested rule");
        let workspace = workspace.canonicalize().unwrap();
        let ledger = ProjectInstructionLedger::default();
        let nested_call = ToolCall {
            id: "nested".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/deep/new.txt"}),
        };
        let output = ledger
            .preflight(&workspace, &nested_call, 16_000)
            .unwrap()
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("nested rule"));
        assert!(!output.content.contains("root rule"));
        for (name, arguments) in [
            ("list_files", json!({"path":"src/deep"})),
            (
                "apply_patch",
                json!({"patch":"*** Begin Patch\n*** Add File: src/deep/new.txt\n+hello\n*** End Patch"}),
            ),
        ] {
            let call = ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
            };
            assert!(ledger
                .preflight(&workspace, &call, 16_000)
                .unwrap()
                .is_some());
        }
        let sibling_call = ToolCall {
            id: "sibling".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src-other/new.txt"}),
        };
        assert!(ledger
            .preflight(&workspace, &sibling_call, 16_000)
            .unwrap()
            .is_none());
        let escape_call = ToolCall {
            id: "escape".into(),
            name: "read_file".into(),
            arguments: json!({"path":"../workspace-other/secret.txt"}),
        };
        assert!(ledger.preflight(&workspace, &escape_call, 16_000).is_err());
    }

    #[test]
    fn activation_reopens_from_journal_after_document_changes() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("src");
        fs::create_dir(&nested).unwrap();
        write(&nested.join(AGENTS_MD_FILENAME), "original nested rule");
        let workspace = workspace.path().canonicalize().unwrap();
        let call = ToolCall {
            id: "read-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/file.txt"}),
        };
        let output = ProjectInstructionLedger::default()
            .preflight(&workspace, &call, 16_000)
            .unwrap()
            .unwrap();
        let hook_id = activation_hook_id(&output.content).unwrap();
        let events = vec![
            AgentEvent::new(
                "session",
                2,
                AgentEventKind::ToolStarted {
                    tool_call: call.clone(),
                },
            ),
            AgentEvent::new(
                "session",
                3,
                AgentEventKind::HookStarted {
                    tool_call_id: call.id.clone(),
                    hook_id: hook_id.clone(),
                    phase: "pre_tool_use".into(),
                },
            ),
            AgentEvent::new(
                "session",
                4,
                AgentEventKind::HookFinished {
                    tool_call_id: call.id.clone(),
                    hook_id,
                    phase: "pre_tool_use".into(),
                    decision: "allow".into(),
                },
            ),
            AgentEvent::new(
                "session",
                5,
                AgentEventKind::ToolFinished {
                    tool_call_id: call.id.clone(),
                    output: output.content,
                    is_error: true,
                },
            ),
        ];
        write(&nested.join(AGENTS_MD_FILENAME), "changed nested rule");
        let reopened = ProjectInstructionLedger::from_events(&events, 1, &workspace).unwrap();
        let context = reopened.model_context().unwrap();
        assert!(context.content.contains("original nested rule"));
        assert!(!context.content.contains("changed nested rule"));
        assert!(reopened
            .preflight(&workspace, &call, 16_000)
            .unwrap()
            .is_none());
        let untrusted = ProjectInstructionLedger::from_events(
            &[events[0].clone(), events[3].clone()],
            1,
            &workspace,
        )
        .unwrap();
        assert!(untrusted.model_context().is_none());
    }

    #[test]
    fn nested_repository_replaces_outer_rules_and_linked_target_fails_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::create_dir(nested.join(".git")).unwrap();
        write(&workspace.path().join(AGENTS_MD_FILENAME), "outer rule");
        write(&nested.join(AGENTS_MD_FILENAME), "inner rule");
        let workspace = workspace.path().canonicalize().unwrap();
        let call = ToolCall {
            id: "read-nested".into(),
            name: "read_file".into(),
            arguments: json!({"path":"nested/file.txt"}),
        };
        let output = ProjectInstructionLedger::default()
            .preflight(&workspace, &call, 16_000)
            .unwrap()
            .unwrap();
        assert!(output.content.contains("inner rule"));
        assert!(!output.content.contains("outer rule"));
        let linked = workspace.join("linked");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&nested, &linked);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(&nested, &linked);
        if let Err(error) = link_result {
            if error.kind() == io::ErrorKind::PermissionDenied
                || cfg!(windows) && error.raw_os_error() == Some(1314)
            {
                return;
            }
            panic!("cannot create test symlink: {error}");
        }
        let linked_call = ToolCall {
            id: "read-linked".into(),
            name: "read_file".into(),
            arguments: json!({"path":"linked/file.txt"}),
        };
        assert!(ProjectInstructionLedger::default()
            .preflight(&workspace, &linked_call, 16_000)
            .is_err());
    }
}
