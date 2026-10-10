//! Bounded, workspace-scoped text patches for the native JSON tool protocol.
//!
//! The syntax follows the useful Add/Delete/Update/Move subset of Codex patches.
//! Parsing and every file precondition complete before staging or commit.
//! A commit error rolls back earlier file changes; an unsuccessful rollback is
//! reported as an uncertain effect so the native journal will not auto-retry.

use crate::{AgentError, ToolOutput};
use std::collections::HashMap;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_FILE_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 32;
const MAX_CHUNKS: usize = 128;
const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug)]
enum Operation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

impl Operation {
    fn path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Delete { path } | Self::Update { path, .. } => path,
        }
    }
}

#[derive(Debug, Default)]
struct Chunk {
    anchor: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    changed: bool,
    at_eof: bool,
}

struct PreparedFile {
    path: String,
    target: PathBuf,
    original: Option<Vec<u8>>,
    replacement: Option<Vec<u8>>,
    replacement_permissions: Option<fs::Permissions>,
    allow_new_parents: bool,
}

struct TransactionFile {
    prepared: PreparedFile,
    staged: Option<PathBuf>,
    backup: Option<PathBuf>,
    installed: bool,
    created_parents: Vec<PathBuf>,
}

pub(super) fn apply_patch(
    workspace: &Path,
    patch: &str,
    cancel: &CancellationToken,
) -> Result<ToolOutput, AgentError> {
    apply_patch_inner(workspace, patch, cancel, None)
}

fn apply_patch_inner(
    workspace: &Path,
    patch: &str,
    cancel: &CancellationToken,
    fail_before_operation: Option<usize>,
) -> Result<ToolOutput, AgentError> {
    let operations = parse(patch)?;
    if cancel.is_cancelled() {
        return Err(AgentError::Cancelled);
    }
    let root = workspace.canonicalize().map_err(io_error)?;
    if !root.is_dir() {
        return Err(tool_error("workspace is not a directory"));
    }
    let prepared = prepare(&root, operations)?;
    let count = prepared.len();
    let mut transaction = stage(&root, prepared)?;
    for index in 0..transaction.len() {
        let result = if cancel.is_cancelled() {
            Err(AgentError::Cancelled)
        } else if fail_before_operation == Some(index) {
            Err(tool_error("patch commit failed"))
        } else {
            (|| {
                let current = &transaction[index].prepared;
                let checked = if current.allow_new_parents {
                    safe_move_target(&root, &current.path)?
                } else {
                    safe_target(&root, &current.path)?
                };
                if checked != current.target || read_state(&checked)? != current.original {
                    Err(tool_error("file changed after patch validation"))
                } else {
                    commit_one(&root, &mut transaction[index])
                }
            })()
        };
        if let Err(error) = result {
            return Err(rollback_after_error(&mut transaction, error));
        }
    }
    let mut cleanup_failed = false;
    for entry in &mut transaction {
        if let Some(backup) = entry.backup.take() {
            cleanup_failed |= fs::remove_file(backup).is_err();
        }
    }
    let message = if cleanup_failed {
        format!("patch applied to {count} files; temporary backup cleanup failed")
    } else {
        format!("patch applied to {count} files")
    };
    Ok(ToolOutput::success(message))
}

fn parse(patch: &str) -> Result<Vec<Operation>, AgentError> {
    if patch.is_empty() || patch.len() > MAX_PATCH_BYTES || patch.contains('\0') {
        return Err(tool_error("patch is empty, oversized, or contains NUL"));
    }
    let lines = patch
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err(tool_error("patch needs Begin Patch and End Patch markers"));
    }
    let mut operations = Vec::new();
    let mut index = 1usize;
    let end = lines.len() - 1;
    let mut chunk_count = 0usize;
    while index < end {
        if operations.len() == MAX_FILES {
            return Err(tool_error("patch has too many files"));
        }
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            validate_path_text(path)?;
            index += 1;
            let mut added = Vec::new();
            while index < end && !is_file_marker(lines[index]) {
                let Some(text) = lines[index].strip_prefix('+') else {
                    return Err(tool_error("Add File accepts only + lines"));
                };
                added.push(text.to_owned());
                index += 1;
            }
            if added.is_empty() {
                return Err(tool_error("Add File needs at least one + line"));
            }
            operations.push(Operation::Add {
                path: path.to_owned(),
                lines: added,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            validate_path_text(path)?;
            operations.push(Operation::Delete {
                path: path.to_owned(),
            });
            index += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            validate_path_text(path)?;
            index += 1;
            let move_to = if index < end {
                if let Some(destination) = lines[index].strip_prefix("*** Move to: ") {
                    validate_path_text(destination)?;
                    index += 1;
                    Some(destination.to_owned())
                } else {
                    None
                }
            } else {
                None
            };
            let mut chunks = Vec::new();
            let mut active: Option<Chunk> = None;
            while index < end && !is_file_marker(lines[index]) {
                let change = lines[index];
                if change == "@@" || change.starts_with("@@ ") {
                    if let Some(chunk) = active.take() {
                        finish_chunk(&mut chunks, chunk, move_to.is_some())?;
                    }
                    active = Some(Chunk {
                        anchor: change.strip_prefix("@@ ").map(str::to_owned),
                        ..Chunk::default()
                    });
                } else if change == "*** End of File" {
                    let Some(chunk) = active.as_mut() else {
                        return Err(tool_error("End of File has no update hunk"));
                    };
                    if chunk.at_eof {
                        return Err(tool_error("duplicate End of File marker"));
                    }
                    chunk.at_eof = true;
                } else {
                    let chunk = active.get_or_insert_with(Chunk::default);
                    if chunk.at_eof {
                        return Err(tool_error("content follows End of File marker"));
                    }
                    let (prefix, text) = change
                        .get(..1)
                        .and_then(|prefix| change.get(1..).map(|text| (prefix, text)))
                        .ok_or_else(|| tool_error("empty update hunk line"))?;
                    match prefix {
                        " " => {
                            chunk.old_lines.push(text.to_owned());
                            chunk.new_lines.push(text.to_owned());
                        }
                        "-" => {
                            chunk.old_lines.push(text.to_owned());
                            chunk.changed = true;
                        }
                        "+" => {
                            chunk.new_lines.push(text.to_owned());
                            chunk.changed = true;
                        }
                        _ => return Err(tool_error("invalid update hunk line")),
                    }
                }
                index += 1;
            }
            if let Some(chunk) = active {
                finish_chunk(&mut chunks, chunk, move_to.is_some())?;
            }
            if chunks.is_empty() {
                return Err(tool_error("Update File needs a change hunk"));
            }
            chunk_count = chunk_count.saturating_add(chunks.len());
            if chunk_count > MAX_CHUNKS {
                return Err(tool_error("patch has too many update hunks"));
            }
            operations.push(Operation::Update {
                path: path.to_owned(),
                move_to,
                chunks,
            });
        } else {
            return Err(tool_error("unsupported patch file marker"));
        }
    }
    if operations.is_empty() {
        return Err(tool_error("patch has no file operations"));
    }
    Ok(operations)
}

/// A structural permission check only; execution still validates containment,
/// links, original text and transaction preconditions against the workspace.
pub(crate) fn is_non_deleting_workspace_patch(patch: &str) -> bool {
    parse(patch).is_ok_and(|operations| {
        !operations.is_empty() && operations.iter().all(|operation| matches!(operation,
            Operation::Add { .. } | Operation::Update { move_to: None, .. }))
    })
}

pub(crate) fn is_workspace_relative_edit_path(path: &str) -> bool {
    validate_path_text(path).is_ok()
}

fn is_file_marker(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
        || line == "*** End Patch"
}

fn finish_chunk(
    chunks: &mut Vec<Chunk>,
    chunk: Chunk,
    allow_unchanged_context: bool,
) -> Result<(), AgentError> {
    if !(chunk.changed || allow_unchanged_context && !chunk.old_lines.is_empty()) {
        return Err(tool_error("update hunk has no added or removed lines"));
    }
    if chunk.anchor.as_deref() == Some("") {
        return Err(tool_error("empty update context anchor"));
    }
    chunks.push(chunk);
    Ok(())
}

fn validate_path_text(path: &str) -> Result<(), AgentError> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.chars().any(char::is_control)
        || Path::new(path).is_absolute()
        || Path::new(path).components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || !Path::new(path)
            .components()
            .any(|part| matches!(part, Component::Normal(_)))
    {
        return Err(tool_error("patch path must be relative to the workspace"));
    }
    #[cfg(windows)]
    for component in Path::new(path).components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0');
        if reserved
            || name.ends_with('.')
            || name.ends_with(' ')
            || name
                .chars()
                .any(|character| matches!(character, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(tool_error("patch path has a reserved Windows file name"));
        }
    }
    Ok(())
}

fn safe_target(root: &Path, requested: &str) -> Result<PathBuf, AgentError> {
    safe_target_inner(root, requested, false)
}

fn safe_move_target(root: &Path, requested: &str) -> Result<PathBuf, AgentError> {
    safe_target_inner(root, requested, true)
}

fn safe_target_inner(
    root: &Path,
    requested: &str,
    allow_missing_parents: bool,
) -> Result<PathBuf, AgentError> {
    validate_path_text(requested)?;
    let components = Path::new(requested)
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut target = root.to_path_buf();
    for (index, name) in components.iter().enumerate() {
        target.push(name);
        match fs::symlink_metadata(&target) {
            Ok(metadata) if is_link_or_reparse(&metadata) => {
                return Err(tool_error("patch path traverses a symlink"));
            }
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                return Err(tool_error("patch parent is not a directory"));
            }
            Ok(metadata) if index + 1 == components.len() && metadata.is_dir() => {
                return Err(tool_error("patch target is a directory"));
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && index + 1 == components.len() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound && allow_missing_parents => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(tool_error("patch parent directory does not exist"));
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| tool_error("patch path has no parent"))?;
    let mut existing_parent = parent;
    let resolved_parent = loop {
        match fs::symlink_metadata(existing_parent) {
            Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
                return Err(tool_error(
                    "patch path traverses a symlink or non-directory",
                ));
            }
            Ok(_) => break existing_parent.canonicalize().map_err(io_error)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound && allow_missing_parents => {
                existing_parent = existing_parent
                    .parent()
                    .ok_or_else(|| tool_error("patch path has no existing parent"))?;
            }
            Err(error) => return Err(io_error(error)),
        }
    };
    if !resolved_parent.starts_with(root) {
        return Err(tool_error("patch path is outside the workspace"));
    }
    if !allow_missing_parents {
        Ok(resolved_parent.join(
            target
                .file_name()
                .ok_or_else(|| tool_error("patch path has no file name"))?,
        ))
    } else {
        Ok(target)
    }
}

fn read_state(target: &Path) -> Result<Option<Vec<u8>>, AgentError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_file() => {
            Err(tool_error("patch target is not a regular file"))
        }
        Ok(metadata) if metadata.len() > MAX_FILE_BYTES as u64 => {
            Err(tool_error("patch target exceeds file size limit"))
        }
        Ok(_) => {
            let file = open_no_follow(target).map_err(io_error)?;
            let metadata = file.metadata().map_err(io_error)?;
            if is_link_or_reparse(&metadata) || !metadata.is_file() {
                return Err(tool_error("patch target is not a regular file"));
            }
            let mut bytes = Vec::new();
            file.take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err(tool_error("patch target exceeds file size limit"));
            }
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn open_no_follow(target: &Path) -> io::Result<fs::File> {
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
    options.open(target)
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

fn prepare(root: &Path, operations: Vec<Operation>) -> Result<Vec<PreparedFile>, AgentError> {
    // Apply the whole batch to an in-memory view first. This permits an Add,
    // Update, then Move of the same new file without writing an intermediate
    // version, and ensures every hunk is checked before the first mutation.
    let mut files: Vec<PreparedFile> = Vec::new();
    let mut indexes = HashMap::new();
    for operation in operations {
        let source = load_virtual_file(root, operation.path(), &mut files, &mut indexes)?;
        match operation {
            Operation::Add { path, lines } => {
                if files[source].replacement.is_some() {
                    return Err(tool_error("Add File target already exists"));
                }
                files[source].path = path;
                files[source].replacement = Some(format!("{}\n", lines.join("\n")).into_bytes());
                files[source].replacement_permissions = None;
            }
            Operation::Delete { .. } => {
                if files[source].replacement.is_none() {
                    return Err(tool_error("Delete File target does not exist"));
                }
                files[source].replacement = None;
                files[source].replacement_permissions = None;
            }
            Operation::Update {
                move_to, chunks, ..
            } => {
                let old = files[source]
                    .replacement
                    .as_ref()
                    .ok_or_else(|| tool_error("Update File target does not exist"))?;
                let old = std::str::from_utf8(old)
                    .map_err(|_| tool_error("patch target is not UTF-8 text"))?;
                let replacement = apply_chunks(old, &chunks)?.into_bytes();
                if let Some(destination) = move_to {
                    let dest = load_virtual_file(root, &destination, &mut files, &mut indexes)?;
                    if dest == source {
                        return Err(tool_error("Move to destination is the source"));
                    }
                    if files[dest].original.is_some() || files[dest].replacement.is_some() {
                        return Err(tool_error("Move to destination already exists"));
                    }
                    files[dest].replacement = Some(replacement);
                    files[dest].replacement_permissions =
                        files[source].replacement_permissions.clone();
                    files[source].replacement = None;
                    files[source].replacement_permissions = None;
                } else {
                    files[source].replacement = Some(replacement);
                }
            }
        }
    }
    let mut prepared = Vec::new();
    let mut total_bytes = 0usize;
    for file in files {
        if file.original != file.replacement {
            push_prepared(&mut prepared, &mut total_bytes, file)?;
        }
    }
    if prepared.is_empty() {
        return Err(tool_error("patch has no net file changes"));
    }
    Ok(prepared)
}

fn path_identity(target: &Path) -> String {
    if cfg!(windows) {
        target.to_string_lossy().to_lowercase()
    } else {
        target.to_string_lossy().into_owned()
    }
}

fn load_virtual_file(
    root: &Path,
    path: &str,
    files: &mut Vec<PreparedFile>,
    indexes: &mut HashMap<String, usize>,
) -> Result<usize, AgentError> {
    let lexical = safe_move_target(root, path)?;
    let key = path_identity(&lexical);
    if let Some(&index) = indexes.get(&key) {
        return Ok(index);
    }
    let allow_new_parents = !lexical
        .parent()
        .expect("validated path has a parent")
        .is_dir();
    let target = if allow_new_parents {
        lexical
    } else {
        safe_target(root, path)?
    };
    let original = read_state(&target)?;
    let replacement_permissions = if original.is_some() {
        let metadata = fs::symlink_metadata(&target).map_err(io_error)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(tool_error("patch target is not a regular file"));
        }
        if metadata.permissions().readonly() {
            return Err(tool_error("patch target is read-only"));
        }
        Some(metadata.permissions())
    } else {
        None
    };
    let index = files.len();
    files.push(PreparedFile {
        path: path.to_owned(),
        target,
        replacement: original.clone(),
        original,
        replacement_permissions,
        allow_new_parents,
    });
    indexes.insert(key, index);
    Ok(index)
}

fn push_prepared(
    files: &mut Vec<PreparedFile>,
    total_bytes: &mut usize,
    file: PreparedFile,
) -> Result<(), AgentError> {
    if file
        .replacement
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_FILE_BYTES)
    {
        return Err(tool_error("patched file exceeds file size limit"));
    }
    *total_bytes = total_bytes
        .saturating_add(file.original.as_ref().map_or(0, Vec::len))
        .saturating_add(file.replacement.as_ref().map_or(0, Vec::len));
    if *total_bytes > MAX_TOTAL_FILE_BYTES {
        return Err(tool_error("patch exceeds total file size limit"));
    }
    files.push(file);
    Ok(())
}

fn apply_chunks(source: &str, chunks: &[Chunk]) -> Result<String, AgentError> {
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let final_newline = source.is_empty() || source.ends_with('\n');
    let mut lines = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if source.ends_with('\n') {
        lines.pop();
    }
    if source.is_empty() {
        lines.clear();
    }
    let mut cursor = 0usize;
    for chunk in chunks {
        let search_from = if let Some(anchor) = &chunk.anchor {
            unique_line(&lines, anchor, cursor)? + 1
        } else {
            cursor
        };
        let start = if chunk.old_lines.is_empty() {
            if chunk.anchor.is_some() {
                search_from
            } else {
                lines.len()
            }
        } else {
            unique_sequence(&lines, &chunk.old_lines, search_from, chunk.at_eof)?
        };
        if chunk.at_eof && start + chunk.old_lines.len() != lines.len() {
            return Err(tool_error("End of File precondition does not match"));
        }
        let old_len = chunk.old_lines.len();
        let new_len = chunk.new_lines.len();
        lines.splice(start..start + old_len, chunk.new_lines.iter().cloned());
        cursor = start + new_len;
    }
    let mut result = lines.join(newline);
    if final_newline && !lines.is_empty() {
        result.push_str(newline);
    }
    Ok(result)
}

fn unique_line(lines: &[String], anchor: &str, from: usize) -> Result<usize, AgentError> {
    let positions = lines
        .iter()
        .enumerate()
        .skip(from)
        .filter_map(|(index, line)| (line == anchor).then_some(index))
        .take(2)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => Ok(*position),
        [] => Err(tool_error("update context anchor was not found")),
        _ => Err(tool_error("update context anchor is ambiguous")),
    }
}

fn unique_sequence(
    lines: &[String],
    pattern: &[String],
    from: usize,
    at_eof: bool,
) -> Result<usize, AgentError> {
    let positions = (from..=lines.len().saturating_sub(pattern.len()))
        .filter(|&index| {
            lines.get(index..index + pattern.len()) == Some(pattern)
                && (!at_eof || index + pattern.len() == lines.len())
        })
        .take(2)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => Ok(*position),
        [] => Err(tool_error("update hunk context did not match")),
        _ => Err(tool_error("update hunk context is ambiguous")),
    }
}

fn stage(root: &Path, prepared: Vec<PreparedFile>) -> Result<Vec<TransactionFile>, AgentError> {
    let mut transaction = Vec::with_capacity(prepared.len());
    for file in prepared {
        let staged = if let Some(bytes) = &file.replacement {
            // Staging under the verified workspace root also works when a
            // Move to parent does not exist yet. Hard-link installation below
            // will fail safely if that destination is on another filesystem.
            let path = root.join(format!(".openide-patch-{}.tmp", Uuid::new_v4()));
            let write_result = (|| -> io::Result<()> {
                let mut handle = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                handle.write_all(bytes)?;
                handle.sync_all()?;
                if let Some(permissions) = &file.replacement_permissions {
                    fs::set_permissions(&path, permissions.clone())?;
                }
                Ok(())
            })();
            if let Err(error) = write_result {
                let remove_failed = fs::remove_file(&path)
                    .is_err_and(|remove_error| remove_error.kind() != io::ErrorKind::NotFound);
                let rollback_failed = rollback(&mut transaction).is_err();
                if remove_failed || rollback_failed {
                    return Err(AgentError::EffectUncertain(
                        "patch staging failed and temporary files could not be removed; inspect the workspace before retrying"
                            .into(),
                    ));
                }
                return Err(io_error(error));
            }
            Some(path)
        } else {
            None
        };
        transaction.push(TransactionFile {
            prepared: file,
            staged,
            backup: None,
            installed: false,
            created_parents: Vec::new(),
        });
    }
    Ok(transaction)
}

fn create_missing_parents(
    root: &Path,
    requested: &str,
    created: &mut Vec<PathBuf>,
) -> Result<(), AgentError> {
    validate_path_text(requested)?;
    let relative_parent = Path::new(requested)
        .parent()
        .ok_or_else(|| tool_error("patch path has no parent"))?;
    let mut parent = root.to_path_buf();
    for component in relative_parent.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        parent.push(name);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if is_link_or_reparse(&metadata) || !metadata.is_dir() => {
                return Err(tool_error("Move to parent is a symlink or non-directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&parent) {
                    Ok(()) => created.push(parent.clone()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&parent).map_err(io_error)?;
                        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
                            return Err(tool_error("Move to parent is a symlink or non-directory"));
                        }
                    }
                    Err(error) => return Err(io_error(error)),
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn commit_one(root: &Path, file: &mut TransactionFile) -> Result<(), AgentError> {
    if file.prepared.allow_new_parents {
        create_missing_parents(root, &file.prepared.path, &mut file.created_parents)?;
        if safe_target(root, &file.prepared.path)? != file.prepared.target
            || read_state(&file.prepared.target)?.is_some()
        {
            return Err(tool_error("Move to destination changed after validation"));
        }
    }
    if file.prepared.original.is_some() {
        let parent = file
            .prepared
            .target
            .parent()
            .expect("validated target has parent");
        let backup = parent.join(format!(".openide-patch-{}.bak", Uuid::new_v4()));
        fs::rename(&file.prepared.target, &backup).map_err(io_error)?;
        file.backup = Some(backup);
    }
    if let Some(staged) = &file.staged {
        // hard_link is an atomic create-if-absent operation. A plain rename
        // could overwrite an unexpected destination on Unix after validation.
        fs::hard_link(staged, &file.prepared.target).map_err(io_error)?;
        file.installed = true;
        fs::remove_file(staged).map_err(io_error)?;
        file.staged = None;
    }
    Ok(())
}

fn rollback_after_error(transaction: &mut [TransactionFile], error: AgentError) -> AgentError {
    match rollback(transaction) {
        Ok(()) => error,
        Err(_) => AgentError::EffectUncertain(
            "patch commit failed and rollback could not restore all files; inspect the workspace before any retry"
                .into(),
        ),
    }
}

fn rollback(transaction: &mut [TransactionFile]) -> io::Result<()> {
    let mut first_error = None;
    for file in transaction.iter_mut().rev() {
        if file.installed {
            if let Err(error) = fs::remove_file(&file.prepared.target) {
                first_error.get_or_insert(error);
            } else {
                file.installed = false;
            }
        }
        if let Some(backup) = &file.backup {
            if let Err(error) = fs::rename(backup, &file.prepared.target) {
                first_error.get_or_insert(error);
            } else {
                file.backup = None;
            }
        }
        if let Some(staged) = &file.staged {
            if let Err(error) = fs::remove_file(staged) {
                first_error.get_or_insert(error);
            } else {
                file.staged = None;
            }
        }
        for parent in file.created_parents.iter().rev() {
            if let Err(error) = fs::remove_dir(parent) {
                first_error.get_or_insert(error);
            }
        }
        file.created_parents.clear();
    }
    first_error.map_or(Ok(()), Err)
}

fn tool_error(message: &str) -> AgentError {
    AgentError::Tool(message.into())
}

fn io_error(error: io::Error) -> AgentError {
    AgentError::Tool(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(workspace: &Path, patch: &str) -> Result<ToolOutput, AgentError> {
        apply_patch(workspace, patch, &CancellationToken::new())
    }

    #[test]
    fn applies_context_hunks_and_preserves_crlf() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(
            workspace.path().join("one.txt"),
            b"top\r\nold\r\nbottom\r\n",
        )
        .unwrap();
        fs::write(workspace.path().join("two.txt"), "alpha\nbeta\n").unwrap();
        let result = apply(
            workspace.path(),
            "*** Begin Patch\n*** Update File: one.txt\n@@ top\n-old\n+new\n*** Update File: two.txt\n@@\n alpha\n-beta\n+gamma\n*** End Patch\n",
        )
        .unwrap();
        assert!(!result.is_error);
        assert_eq!(
            fs::read(workspace.path().join("one.txt")).unwrap(),
            b"top\r\nnew\r\nbottom\r\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("two.txt")).unwrap(),
            "alpha\ngamma\n"
        );
    }

    #[test]
    fn invalid_second_file_leaves_first_unchanged() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("first.txt"), "old\n").unwrap();
        fs::write(workspace.path().join("second.txt"), "actual\n").unwrap();
        assert!(apply(
            workspace.path(),
            "*** Begin Patch\n*** Update File: first.txt\n@@\n-old\n+new\n*** Update File: second.txt\n@@\n-missing\n+wrong\n*** End Patch",
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(workspace.path().join("first.txt")).unwrap(),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("second.txt")).unwrap(),
            "actual\n"
        );
    }

    #[test]
    fn add_delete_and_rollback_after_commit_error() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("old.txt"), "remove me\n").unwrap();
        let patch = "*** Begin Patch\n*** Delete File: old.txt\n*** Add File: new.txt\n+created\n*** End Patch";
        assert!(
            apply_patch_inner(workspace.path(), patch, &CancellationToken::new(), Some(1),)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("old.txt")).unwrap(),
            "remove me\n"
        );
        assert!(!workspace.path().join("new.txt").exists());
        apply(workspace.path(), patch).unwrap();
        assert!(!workspace.path().join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(workspace.path().join("new.txt")).unwrap(),
            "created\n"
        );
    }

    #[test]
    fn moves_and_updates_into_new_nested_directory() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("old")).unwrap();
        fs::write(workspace.path().join("old/name.txt"), "old content\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n-old content\n+new content\n*** End Patch";
        apply(workspace.path(), patch).unwrap();
        assert!(!workspace.path().join("old/name.txt").exists());
        assert_eq!(
            fs::read_to_string(workspace.path().join("renamed/dir/name.txt")).unwrap(),
            "new content\n"
        );
        assert!(fs::read_dir(workspace.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".openide-patch-")));
    }

    #[test]
    fn add_update_move_chain_is_prevalidated_and_committed_as_one_batch() {
        let workspace = tempfile::tempdir().unwrap();
        let patch = "*** Begin Patch\n*** Add File: draft.txt\n+first\n*** Update File: draft.txt\n@@\n-first\n+second\n*** Update File: draft.txt\n*** Move to: nested/final.txt\n@@\n second\n*** End Patch";
        apply(workspace.path(), patch).unwrap();
        assert!(!workspace.path().join("draft.txt").exists());
        assert_eq!(
            fs::read_to_string(workspace.path().join("nested/final.txt")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn prevalidation_rejects_invalid_context_and_destination_conflicts_without_changes() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("source.txt"), "original\n").unwrap();
        fs::write(workspace.path().join("taken.txt"), "untouched\n").unwrap();
        for suffix in [
            "*** Update File: source.txt\n*** Move to: new/target.txt\n@@\n-missing\n+new\n",
            "*** Update File: source.txt\n*** Move to: taken.txt\n@@\n-original\n+new\n",
            "*** Update File: source.txt\n*** Move to: ../outside.txt\n@@\n-original\n+new\n",
            "*** Update File: source.txt\n*** Move to: source.txt\n@@\n-original\n+new\n",
        ] {
            let patch = format!(
                "*** Begin Patch\n*** Add File: first.txt\n+created\n{suffix}*** End Patch"
            );
            assert!(apply(workspace.path(), &patch).is_err(), "{suffix}");
            assert!(!workspace.path().join("first.txt").exists());
            assert!(!workspace.path().join("new").exists());
            assert_eq!(
                fs::read_to_string(workspace.path().join("source.txt")).unwrap(),
                "original\n"
            );
            assert_eq!(
                fs::read_to_string(workspace.path().join("taken.txt")).unwrap(),
                "untouched\n"
            );
        }
    }

    #[test]
    fn move_rollback_restores_source_and_removes_new_directories() {
        for failed_index in [1, 2] {
            let workspace = tempfile::tempdir().unwrap();
            fs::write(workspace.path().join("source.txt"), "original\n").unwrap();
            let patch = "*** Begin Patch\n*** Update File: source.txt\n*** Move to: deep/nested/dest.txt\n@@\n-original\n+updated\n*** Add File: later.txt\n+later\n*** End Patch";
            assert!(apply_patch_inner(
                workspace.path(),
                patch,
                &CancellationToken::new(),
                Some(failed_index),
            )
            .is_err());
            assert_eq!(
                fs::read_to_string(workspace.path().join("source.txt")).unwrap(),
                "original\n"
            );
            assert!(!workspace.path().join("deep").exists());
            assert!(!workspace.path().join("later.txt").exists());
            assert_eq!(fs::read_dir(workspace.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn destination_created_after_prevalidation_is_not_overwritten() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("source.txt"), "original\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: source.txt\n*** Move to: destination.txt\n@@\n-original\n+updated\n*** End Patch";
        let root = workspace.path().canonicalize().unwrap();
        let prepared = prepare(&root, parse(patch).unwrap()).unwrap();
        let mut transaction = stage(&root, prepared).unwrap();
        commit_one(&root, &mut transaction[0]).unwrap();
        fs::write(workspace.path().join("destination.txt"), "intruder\n").unwrap();
        assert!(commit_one(&root, &mut transaction[1]).is_err());
        rollback(&mut transaction).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("source.txt")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("destination.txt")).unwrap(),
            "intruder\n"
        );
        assert_eq!(fs::read_dir(workspace.path()).unwrap().count(), 2);
    }

    #[test]
    fn move_rejects_symlink_source_destination_and_parent() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("source.txt"), "original\n").unwrap();
        fs::write(outside.path().join("outside.txt"), "outside\n").unwrap();
        let destination_link = workspace.path().join("dest.txt");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(
            outside.path().join("outside.txt"),
            &destination_link,
        );
        #[cfg(unix)]
        let linked =
            std::os::unix::fs::symlink(outside.path().join("outside.txt"), &destination_link);
        if linked.is_ok() {
            let patch = "*** Begin Patch\n*** Update File: source.txt\n*** Move to: dest.txt\n@@\n-original\n+updated\n*** End Patch";
            assert!(apply(workspace.path(), patch).is_err());
            let source_link_patch = "*** Begin Patch\n*** Update File: dest.txt\n*** Move to: moved.txt\n@@\n-outside\n+updated\n*** End Patch";
            assert!(apply(workspace.path(), source_link_patch).is_err());
        }
        let parent_link = workspace.path().join("linked");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(outside.path(), &parent_link);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(outside.path(), &parent_link);
        if linked.is_ok() {
            let patch = "*** Begin Patch\n*** Update File: source.txt\n*** Move to: linked/escaped.txt\n@@\n-original\n+updated\n*** End Patch";
            assert!(apply(workspace.path(), patch).is_err());
        }
        assert_eq!(
            fs::read_to_string(workspace.path().join("source.txt")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(outside.path().join("outside.txt")).unwrap(),
            "outside\n"
        );
        assert!(!outside.path().join("escaped.txt").exists());
    }

    #[test]
    fn rollback_restores_installed_update_and_removes_staged_files() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("first.txt"), "old\n").unwrap();
        fs::write(workspace.path().join("second.txt"), "other\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: first.txt\n@@\n-old\n+new\n*** Update File: second.txt\n@@\n-other\n+changed\n*** End Patch";
        assert!(
            apply_patch_inner(workspace.path(), patch, &CancellationToken::new(), Some(1),)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("first.txt")).unwrap(),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("second.txt")).unwrap(),
            "other\n"
        );
        assert_eq!(fs::read_dir(workspace.path()).unwrap().count(), 2);
    }

    #[test]
    fn rejects_traversal_symlinks_and_ambiguous_context() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("private.txt"), "private\n").unwrap();
        let traversal = "*** Begin Patch\n*** Update File: ../private.txt\n@@\n-private\n+changed\n*** End Patch";
        assert!(apply(workspace.path(), traversal).is_err());
        fs::write(workspace.path().join("repeat.txt"), "same\nsame\n").unwrap();
        let ambiguous =
            "*** Begin Patch\n*** Update File: repeat.txt\n@@\n-same\n+changed\n*** End Patch";
        assert!(apply(workspace.path(), ambiguous).is_err());
        let link = workspace.path().join("link.txt");
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_file(outside.path().join("private.txt"), &link);
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(outside.path().join("private.txt"), &link);
        if created.is_ok() {
            let symlink_patch =
                "*** Begin Patch\n*** Update File: link.txt\n@@\n-private\n+changed\n*** End Patch";
            assert!(apply(workspace.path(), symlink_patch).is_err());
        }
        assert_eq!(
            fs::read_to_string(outside.path().join("private.txt")).unwrap(),
            "private\n"
        );
    }

    #[test]
    fn rejects_invalid_repeated_context_oversize_and_missing_preconditions() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("one.txt"), "old\n").unwrap();
        let stale_context = "*** Begin Patch\n*** Update File: one.txt\n@@\n-old\n+new\n*** Update File: ./one.txt\n@@\n-old\n+newer\n*** End Patch";
        assert!(apply(workspace.path(), stale_context).is_err());
        assert_eq!(
            fs::read_to_string(workspace.path().join("one.txt")).unwrap(),
            "old\n"
        );
        let oversized = format!(
            "*** Begin Patch\n*** Add File: big.txt\n+{}\n*** End Patch",
            "x".repeat(MAX_PATCH_BYTES)
        );
        assert!(apply(workspace.path(), &oversized).is_err());
        assert!(!workspace.path().join("big.txt").exists());
        let missing = "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch";
        assert!(apply(workspace.path(), missing).is_err());
        let misplaced_move = "*** Begin Patch\n*** Update File: one.txt\n@@\n-old\n+new\n*** Move to: other.txt\n*** End Patch";
        assert!(apply(workspace.path(), misplaced_move).is_err());
        let empty_hunk_line =
            "*** Begin Patch\n*** Update File: one.txt\n@@\n\n-old\n+new\n*** End Patch";
        assert!(apply(workspace.path(), empty_hunk_line).is_err());
        #[cfg(windows)]
        for forbidden in ["CON", "file.txt:stream", "trailing.", "COM1.log"] {
            let patch =
                format!("*** Begin Patch\n*** Add File: {forbidden}\n+hello\n*** End Patch");
            assert!(apply(workspace.path(), &patch).is_err(), "{forbidden}");
        }
    }
}
