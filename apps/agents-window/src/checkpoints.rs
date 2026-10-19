//! Restore points of a task's project, one before each turn of any agent:
//! the native one, ACP agents and CLIs alike, whatever wrote the files.
//!
//! A point is a commit of the whole working tree under the project (tracked
//! changes and untracked files, minus what `.gitignore` leaves out), built in
//! a private index copied from the person's, and kept alive by a ref under
//! `refs/openade/checkpoints/<task>/<n>`. HEAD, the index and branches are
//! never touched. A project that is not a Git work tree gets no points.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::git_branches::{git_error, run_git, run_git_env};

pub(crate) const REF_PREFIX: &str = "refs/openade/checkpoints/";
/// Older points of a task are dropped past this many.
const KEEP: usize = 50;

/// Capture and restore of one project must not interleave; they are rare
/// enough that one at a time for the whole app costs nothing.
static GIT: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// 1 for the point before the task's first turn seen here.
    pub ordinal: u32,
    pub commit: String,
    pub at_ms: u64,
    /// The start of the message that began the turn; empty for a CLI's.
    pub label: String,
}

/// A task's points, newest last, and the state before the last restore.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Record {
    pub workspace: String,
    pub checkpoints: Vec<Checkpoint>,
    pub undo: Option<Checkpoint>,
}

/// Task ids become a file name and a ref component.
fn valid(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn record_path(data_root: &Path, session: &str) -> Option<PathBuf> {
    valid(session).then(|| data_root.join("checkpoints").join(format!("{session}.json")))
}

pub(crate) fn load(data_root: &Path, session: &str) -> Record {
    record_path(data_root, session)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(data_root: &Path, session: &str, record: &Record) -> Result<(), String> {
    let path = record_path(data_root, session).ok_or("Tarea sin un nombre válido")?;
    let failed = |error: std::io::Error| format!("No se pudo guardar el punto de restauración: {error}");
    std::fs::create_dir_all(path.parent().unwrap_or(data_root)).map_err(failed)?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(failed)?;
    crate::agent_tool_settings::replace_atomically(&temporary, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        failed(error)
    })
}

fn git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    git_with(workspace, args, &[])
}

fn git_with(workspace: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Result<String, String> {
    let output = run_git_env(workspace, args, env)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(git_error(&output))
    }
}

/// Commit the working tree under `workspace` and keep it under `reference`.
/// `None` when the project is not a Git work tree.
fn commit_worktree(workspace: &Path, reference: &str) -> Result<Option<String>, String> {
    if git(workspace, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Ok("true") {
        return Ok(None);
    }
    let index = PathBuf::from(git(workspace, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?);
    let private = index.with_file_name(format!("openade-checkpoint-{}", uuid::Uuid::new_v4().simple()));
    // The person's index carries file times, so only changed files are read.
    // shortcut: assume-unchanged and skip-worktree entries keep their indexed
    // content; upgrade by rebuilding from HEAD when such flags matter.
    let result = (|| {
        if index.exists() {
            std::fs::copy(&index, &private).map_err(|error| format!("No se pudo copiar el índice: {error}"))?;
        }
        let env = [("GIT_INDEX_FILE", private.as_os_str())];
        git_with(workspace, &["-c", "core.fsmonitor=false", "add", "-A", "--", "."], &env)?;
        let tree = git_with(workspace, &["write-tree"], &env)?;
        let head = git(workspace, &["rev-parse", "--verify", "-q", "HEAD^{commit}"]).ok();
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "openADE checkpoint"];
        if let Some(head) = head.as_deref().filter(|head| !head.is_empty()) {
            args.extend(["-p", head]);
        }
        let author = [
            ("GIT_AUTHOR_NAME", OsStr::new("openADE")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("checkpoints@openade.invalid")),
            ("GIT_COMMITTER_NAME", OsStr::new("openADE")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("checkpoints@openade.invalid")),
        ];
        let commit = git_with(workspace, &args, &author)?;
        git(workspace, &["update-ref", reference, &commit])?;
        Ok(Some(commit))
    })();
    let _ = std::fs::remove_file(&private);
    let _ = std::fs::remove_file(private.with_extension("lock"));
    result
}

/// Record a point before a turn of `session` in `workspace`.
pub(crate) fn capture(data_root: &Path, session: &str, workspace: &Path, label: &str, at_ms: u64) -> Result<(), String> {
    if !valid(session) || !workspace.is_dir() {
        return Ok(());
    }
    let _guard = GIT.lock().unwrap_or_else(|e| e.into_inner());
    let mut record = load(data_root, session);
    let ordinal = record.checkpoints.last().map_or(1, |last| last.ordinal + 1);
    let reference = format!("{REF_PREFIX}{session}/{ordinal}");
    let Some(commit) = commit_worktree(workspace, &reference)? else {
        return Ok(());
    };
    // The same tree twice is one point: a turn that changed nothing.
    if record.checkpoints.last().is_some_and(|last| same_tree(workspace, &last.commit, &commit)) {
        let _ = git(workspace, &["update-ref", "-d", &reference]);
        return Ok(());
    }
    record.workspace = workspace.to_string_lossy().into_owned();
    record.checkpoints.push(Checkpoint {
        ordinal,
        commit,
        at_ms,
        label: label.lines().next().unwrap_or("").chars().take(80).collect(),
    });
    while record.checkpoints.len() > KEEP {
        let old = record.checkpoints.remove(0);
        let _ = git(workspace, &["update-ref", "-d", &format!("{REF_PREFIX}{session}/{}", old.ordinal)]);
    }
    save(data_root, session, &record)
}

fn same_tree(workspace: &Path, first: &str, second: &str) -> bool {
    let tree = |commit: &str| git(workspace, &["rev-parse", &format!("{commit}^{{tree}}")]).ok();
    tree(first).is_some_and(|tree_one| tree(second) == Some(tree_one))
}

#[cfg(test)]
/// The files that differ between the point `commit` and the project now.
pub(crate) fn changed_since(workspace: &Path, commit: &str) -> Result<Vec<String>, String> {
    let _guard = GIT.lock().unwrap_or_else(|e| e.into_inner());
    let now = commit_worktree(workspace, &format!("{REF_PREFIX}scratch"))?.ok_or("El proyecto no es un repositorio Git")?;
    let _ = git(workspace, &["update-ref", "-d", &format!("{REF_PREFIX}scratch")]);
    let names = git(workspace, &["diff", "--name-only", "--no-renames", commit, &now, "--", "."])?;
    Ok(names.lines().map(str::to_owned).collect())
}

/// Put the project back as it was at `ordinal`. The state just before is
/// kept as the record's `undo`, so the restore can itself be undone.
pub(crate) fn restore(data_root: &Path, session: &str, ordinal: Option<u32>) -> Result<(), String> {
    let _guard = GIT.lock().unwrap_or_else(|e| e.into_inner());
    let mut record = load(data_root, session);
    let workspace = PathBuf::from(&record.workspace);
    let target = match ordinal {
        Some(ordinal) => record.checkpoints.iter().find(|point| point.ordinal == ordinal).cloned(),
        None => record.undo.clone(),
    }
    .ok_or("Ese punto de restauración ya no existe")?;
    let undo_ref = format!("{REF_PREFIX}{session}/undo");
    let undo = commit_worktree(&workspace, &undo_ref)?.ok_or("El proyecto no es un repositorio Git")?;
    // Untracked files go first; the point's own come back with its tree.
    // Ignored files (builds, dependencies) are left alone.
    git(&workspace, &["clean", "-f", "-d", "-q", "--", "."])?;
    git(&workspace, &["restore", "--source", &target.commit, "--worktree", "--", "."])?;
    record.undo = (ordinal.is_some()).then(|| Checkpoint {
        ordinal: 0,
        commit: undo,
        at_ms: crate::cli_tasks::now_ms(),
        label: String::new(),
    });
    if ordinal.is_none() {
        let _ = git(&workspace, &["update-ref", "-d", &undo_ref]);
    }
    save(data_root, session, &record)
}

/// Forget a deleted task's points and their refs.
pub(crate) fn remove(data_root: &Path, session: &str) {
    let Some(path) = record_path(data_root, session) else {
        return;
    };
    let record = load(data_root, session);
    if !record.workspace.is_empty() {
        let workspace = Path::new(&record.workspace);
        let prefix = format!("{REF_PREFIX}{session}/");
        if let Ok(output) = run_git(workspace, &["for-each-ref", "--format=%(refname)", &prefix]) {
            for reference in String::from_utf8_lossy(&output.stdout).lines() {
                let _ = git(workspace, &["update-ref", "-d", reference]);
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("kept.txt"), "one\n").unwrap();
        git(&root, &["add", "-A"]).unwrap();
        git_with(
            &root,
            &["commit", "-q", "-m", "first"],
            &[
                ("GIT_AUTHOR_NAME", OsStr::new("t")),
                ("GIT_AUTHOR_EMAIL", OsStr::new("t@t")),
                ("GIT_COMMITTER_NAME", OsStr::new("t")),
                ("GIT_COMMITTER_EMAIL", OsStr::new("t@t")),
            ],
        )
        .unwrap();
        temp
    }

    #[test]
    fn a_point_brings_back_the_tree_and_leaves_head_index_and_ignored_files_alone() {
        let temp = repo();
        let data = temp.path().join("data");
        let root = temp.path().join("project");
        // The person's own staged change and an untracked note exist before.
        std::fs::write(root.join("kept.txt"), "staged\n").unwrap();
        git(&root, &["add", "kept.txt"]).unwrap();
        std::fs::write(root.join("note.md"), "mine\n").unwrap();
        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        capture(&data, "task-1", &root, "Arreglá el login\nmás", 1).unwrap();
        // The agent's turn: edits, creates and deletes.
        std::fs::write(root.join("kept.txt"), "agent\n").unwrap();
        std::fs::write(root.join("new.rs"), "fn x() {}\n").unwrap();
        std::fs::remove_file(root.join("note.md")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/out"), "build\n").unwrap();
        capture(&data, "task-1", &root, "", 2).unwrap();
        let record = load(&data, "task-1");
        assert_eq!(record.checkpoints.len(), 2);
        assert_eq!(record.checkpoints[0].label, "Arreglá el login");
        let mut changed = changed_since(&root, &record.checkpoints[0].commit).unwrap();
        changed.sort();
        assert_eq!(changed, ["kept.txt", "new.rs", "note.md"]);

        restore(&data, "task-1", Some(1)).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("kept.txt")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(root.join("note.md")).unwrap(), "mine\n");
        assert!(!root.join("new.rs").exists());
        assert!(root.join("target/out").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).unwrap(), "kept.txt");

        // And the restore itself can be undone.
        restore(&data, "task-1", None).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("kept.txt")).unwrap(), "agent\n");
        assert!(root.join("new.rs").exists());
        assert!(!root.join("note.md").exists());
        assert!(load(&data, "task-1").undo.is_none());

        remove(&data, "task-1");
        assert_eq!(git(&root, &["for-each-ref", REF_PREFIX]).unwrap(), "");
        assert_eq!(load(&data, "task-1"), Record::default());
    }

    #[test]
    fn an_unchanged_tree_and_a_folder_without_git_add_no_point() {
        let temp = repo();
        let data = temp.path().join("data");
        let root = temp.path().join("project");
        capture(&data, "task", &root, "", 1).unwrap();
        capture(&data, "task", &root, "", 2).unwrap();
        assert_eq!(load(&data, "task").checkpoints.len(), 1);
        let plain = temp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        capture(&data, "other", &plain, "", 1).unwrap();
        assert!(load(&data, "other").checkpoints.is_empty());
        capture(&data, "../escape", &root, "", 1).unwrap();
        assert!(!temp.path().join("escape.json").exists());
    }
}
