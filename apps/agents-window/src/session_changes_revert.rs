//! Putting files back when a rewind removes the turns that changed them.
//!
//! Only the exact before/after captures of `write_file` and `apply_patch`
//! are used. A path is written when every capture of its chain is there, the
//! chain is continuous, and the disk still holds the last recorded content;
//! anything else is skipped with its reason. Writes go through the review
//! strip's guarded write, which rechecks the content at the last moment.
use super::*;
use crate::code_editor_files::{review_text_round_trips, write_review_file_guarded};
use crate::tool_presentation::{kind, ToolKind};

/// Why a path that the removed turns changed is left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RevertSkip {
    ModifiedLater,
    IncompleteCapture,
    OutsideProject,
    UnsavedInEditor,
    NotText,
}

impl RevertSkip {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ModifiedLater => "modificado después por ti u otra herramienta",
            Self::IncompleteCapture => "captura incompleta",
            Self::OutsideProject => "fuera del proyecto",
            Self::UnsavedInEditor => "tiene cambios sin guardar en el editor",
            Self::NotText => "no es texto que se pueda reescribir con exactitud",
        }
    }
}

/// One path and its two recorded states. `None` is "does not exist".
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RevertFile {
    pub path: String,
    /// What it held before the first removed call touched it.
    pub before: Option<String>,
    /// What the last removed call left, and the disk held at the preflight.
    pub after: Option<String>,
}

/// Calls of the removed turns whose effects are not recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct OpaqueEffects {
    pub commands: usize,
    pub mcp: usize,
    pub other: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RevertPreflight {
    pub restorable: Vec<RevertFile>,
    pub skipped: Vec<(String, RevertSkip)>,
    pub opaque: OpaqueEffects,
}

fn symlinked(root: &Path, relative: &str) -> bool {
    let mut path = root.to_path_buf();
    Path::new(relative).components().any(|component| {
        path.push(component);
        fs::symlink_metadata(&path).is_ok_and(|metadata| is_link(&metadata))
    })
}

fn text(side: &Option<Vec<u8>>) -> Option<Option<String>> {
    match side {
        None => Some(None),
        Some(bytes) if review_text_round_trips(bytes) => {
            Some(Some(String::from_utf8(bytes.clone()).ok()?))
        }
        Some(_) => None,
    }
}

/// What reverting the turns from `before_sequence` on would do right now.
/// Reads only. `unsaved` are the relative paths open with unsaved edits.
pub(crate) fn revert_preflight(
    storage: &Path,
    session: &str,
    workspace: &Path,
    before_sequence: u64,
    unsaved: &BTreeSet<String>,
) -> io::Result<RevertPreflight> {
    let root = workspace.canonicalize()?;
    let events = match read_stored_journal(storage, session)? {
        StoredJournal::Events(events) => events,
        StoredJournal::Unavailable(reason) => return Err(invalid(&reason)),
    };
    validate_journal(session, &root, &events)?;
    let recorded = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } => Some(workspace.as_path()),
            _ => None,
        })
        .unwrap_or(&root);
    let roots = [&root as &Path, recorded];
    let mut result = RevertPreflight::default();
    // Per path: its first "before" and latest "after", or why it is skipped.
    type Chain = Result<(Option<Vec<u8>>, Option<Vec<u8>>), RevertSkip>;
    let mut chains = BTreeMap::<String, Chain>::new();
    for event in events.iter().filter(|event| event.sequence >= before_sequence) {
        let AgentEventKind::ToolStarted { tool_call: call } = &event.kind else {
            continue;
        };
        if !matches!(call.name.as_str(), "write_file" | "apply_patch") {
            if opaque_effect(&call.name) {
                match kind(&call.name) {
                    ToolKind::Command => result.opaque.commands += 1,
                    ToolKind::Mcp => result.opaque.mcp += 1,
                    _ => result.opaque.other += 1,
                }
            }
            continue;
        }
        let Ok(paths) = call_paths(call, &roots) else {
            // Its target cannot be named inside the project: never written.
            let (shown, reason) = match call.arguments.get("path").and_then(|path| path.as_str()) {
                Some(path) if call.name == "write_file" => (path, RevertSkip::OutsideProject),
                _ => ("parche sin rutas verificables", RevertSkip::IncompleteCapture),
            };
            result.skipped.push((shown.chars().take(120).collect(), reason));
            continue;
        };
        let finish = events.iter().find_map(|other| match &other.kind {
            AgentEventKind::ToolFinished { tool_call_id, is_error, .. } if *tool_call_id == call.id => {
                Some((other.sequence, *is_error))
            }
            _ => None,
        });
        let transition = finish.and_then(|(sequence, _)| {
            tool_snapshots::load_transition(storage, session, &root, recorded, call, event, sequence)
                .ok()
                .flatten()
        });
        for path in paths {
            let step = transition.as_ref().and_then(|transition| {
                let before = transition.before.get(&path)?.clone().ok()?;
                // A failed call claims no change; the continuity check below
                // and the disk check catch one that changed something anyway.
                let after = match finish {
                    Some((_, true)) => before.clone(),
                    _ => transition.after.get(&path)?.clone().ok()?,
                };
                Some((before, after))
            });
            match (chains.get_mut(&path), step) {
                (Some(Err(_)), _) => {}
                // A chain with a gap is not trustworthy: the whole path is skipped.
                (_, None) => {
                    chains.insert(path, Err(RevertSkip::IncompleteCapture));
                }
                (None, Some(step)) => {
                    chains.insert(path, Ok(step));
                }
                (Some(chain), Some((before, after))) => {
                    *chain = match &mut *chain {
                        Ok((first, last)) if *last == before => Ok((first.take(), after)),
                        _ => Err(RevertSkip::ModifiedLater),
                    };
                }
            }
        }
    }
    for (path, chain) in chains {
        let (before, after) = match chain {
            Ok(chain) => chain,
            Err(reason) => {
                result.skipped.push((path, reason));
                continue;
            }
        };
        if before == after {
            continue;
        }
        let skip = if symlinked(&root, &path) {
            Some(RevertSkip::OutsideProject)
        } else if unsaved.contains(&path) {
            Some(RevertSkip::UnsavedInEditor)
        } else {
            let disk = match read_workspace_file(&root, &path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            };
            // A file to recreate needs its folder: none is created here.
            let folder = after.is_some()
                || Path::new(&path).parent().is_some_and(|parent| root.join(parent).is_dir());
            (!disk.is_ok_and(|disk| disk == after) || !folder).then_some(RevertSkip::ModifiedLater)
        };
        match (skip, text(&before), text(&after)) {
            (Some(reason), ..) => result.skipped.push((path, reason)),
            (None, Some(before), Some(after)) => {
                result.restorable.push(RevertFile { path, before, after })
            }
            _ => result.skipped.push((path, RevertSkip::NotText)),
        }
    }
    Ok(result)
}

fn write(
    guard: &crate::session_changes_editor::WorkspaceGuard,
    path: &str,
    expected: Option<&str>,
    replacement: Option<&str>,
) -> Result<(), String> {
    write_review_file_guarded(guard, Path::new(path), expected, replacement, || Ok(()))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Write every file's "before" over its "after". All or nothing: when one
/// write fails, those already done are put back and the error names the path.
/// `Err.1` are paths that could not be put back.
pub(crate) fn revert_files(
    storage: &Path,
    workspace: &Path,
    files: &[RevertFile],
) -> Result<(), (String, Vec<String>)> {
    let guard = crate::session_changes_editor::lock_workspace(storage, workspace)
        .map_err(|error| (error.to_string(), Vec::new()))?;
    for (index, file) in files.iter().enumerate() {
        if let Err(error) = write(&guard, &file.path, file.after.as_deref(), file.before.as_deref()) {
            let left = reapply(&guard, &files[..index]);
            return Err((format!("{}: {error}", file.path), left));
        }
    }
    Ok(())
}

fn reapply(guard: &crate::session_changes_editor::WorkspaceGuard, files: &[RevertFile]) -> Vec<String> {
    files
        .iter()
        .rev()
        .filter(|file| write(guard, &file.path, file.before.as_deref(), file.after.as_deref()).is_err())
        .map(|file| file.path.clone())
        .collect()
}

/// Put each file's "after" back where the disk still holds the "before"
/// that a revert wrote. Returns the paths left alone, sorted.
pub(crate) fn reapply_reverted_files(
    storage: &Path,
    workspace: &Path,
    files: &[RevertFile],
) -> Vec<String> {
    let mut left = match crate::session_changes_editor::lock_workspace(storage, workspace) {
        Ok(guard) => reapply(&guard, files),
        Err(_) => files.iter().map(|file| file.path.clone()).collect(),
    };
    left.sort();
    left
}

#[cfg(test)]
#[path = "session_changes_revert_tests.rs"]
mod tests;
