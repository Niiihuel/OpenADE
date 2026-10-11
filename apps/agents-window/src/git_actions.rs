//! Explicit staging actions run independently of read-only review scans.
use crate::child_process::command_no_window;
use std::{
    io,
    path::{Component, PathBuf},
    process::Stdio,
    sync::mpsc::{self, Receiver, SendError, Sender},
    thread,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitActionKind {
    Stage,
    Unstage,
    /// Restore tracked files in the worktree from the index. Staged content
    /// and history are untouched.
    Discard,
    /// Move untracked files to the system trash, where they can be recovered.
    Trash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitActionRequest {
    pub workspace: PathBuf,
    /// Echoed back so the host cannot refresh a different active workspace.
    pub generation: u64,
    pub kind: GitActionKind,
    /// Include both source and destination for a rename.
    pub paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct GitActionResponse {
    pub request: GitActionRequest,
    pub result: Result<(), String>,
}

pub struct GitActionWorker {
    request_tx: Sender<GitActionRequest>,
    result_rx: Receiver<GitActionResponse>,
}

impl GitActionWorker {
    pub fn spawn(trash: crate::file_ops::Trash) -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<GitActionRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-git-actions".into())
            .spawn(move || {
                // Unlike read requests, explicit mutations are never coalesced.
                while let Ok(request) = request_rx.recv() {
                    let result = apply(&request, trash).map_err(|error| error.to_string());
                    if result_tx
                        .send(GitActionResponse { request, result })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn submit(&self, request: GitActionRequest) -> Result<(), SendError<GitActionRequest>> {
        self.request_tx.send(request)
    }

    pub fn poll(&self) -> Option<GitActionResponse> {
        self.result_rx.try_recv().ok()
    }
}

fn apply(request: &GitActionRequest, trash: crate::file_ops::Trash) -> io::Result<()> {
    if request.paths.is_empty()
        || request.paths.iter().any(|path| {
            path.as_os_str().is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "La ruta del cambio no es válida",
        ));
    }
    if request.kind == GitActionKind::Trash {
        for path in &request.paths {
            let target = request.workspace.join(path);
            // Only a file Git does not track goes to the trash from here.
            let tracked = command_no_window("git")
                .args(["--literal-pathspecs", "ls-files", "--error-unmatch", "--"])
                .arg(path)
                .current_dir(&request.workspace)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success();
            if tracked || std::fs::symlink_metadata(&target).is_err() {
                return Err(io::Error::other(format!(
                    "«{}» ya no es un archivo sin seguimiento",
                    path.display()
                )));
            }
            trash(&target).map_err(io::Error::other)?;
        }
        return Ok(());
    }
    let has_head = request.kind != GitActionKind::Unstage
        || command_no_window("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(&request.workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
    // A whole group can hold thousands of paths: keep each command line short.
    for paths in request.paths.chunks(200) {
        let mut command = command_no_window("git");
        command.arg("--literal-pathspecs");
        match request.kind {
            GitActionKind::Stage => command.arg("add"),
            GitActionKind::Unstage if has_head => command.args(["reset", "--quiet", "HEAD"]),
            // An unborn branch has no HEAD. Removing only index entries
            // preserves worktree contents, including edits after staging.
            GitActionKind::Unstage => {
                command.args(["rm", "--cached", "--force", "--ignore-unmatch"])
            }
            GitActionKind::Discard | GitActionKind::Trash => command.args(["checkout", "--quiet"]),
        };
        let output = command
            .arg("--")
            .args(paths)
            .current_dir(&request.workspace)
            .output()?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(1600)
                .collect::<String>();
            return Err(io::Error::other(if message.is_empty() {
                "Git no pudo actualizar los archivos".into()
            } else {
                message
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn git(path: &Path, args: &[&str]) {
        let result = command_no_window("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn no_trash(_: &Path) -> Result<(), String> {
        Err("no trash in this test".into())
    }

    /// Stands in for the system trash: the file leaves the worktree.
    fn remove(path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|error| error.to_string())
    }

    #[test]
    fn discard_restores_the_worktree_from_the_index_and_keeps_staged_content() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(dir.path(), &["config", "user.email", "test@example.invalid"]);
        fs::write(dir.path().join("kept.txt"), "base\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "base\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        fs::write(dir.path().join("kept.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "kept.txt"]);
        fs::write(dir.path().join("kept.txt"), "worktree\n").unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        fs::write(dir.path().join("other.txt"), "untracked\n").unwrap();
        apply(
            &GitActionRequest {
                workspace: dir.path().into(),
                generation: 3,
                kind: GitActionKind::Discard,
                paths: vec!["kept.txt".into(), "gone.txt".into()],
            },
            no_trash,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("kept.txt")).unwrap(), "staged\n");
        assert_eq!(fs::read_to_string(dir.path().join("gone.txt")).unwrap(), "base\n");
        assert_eq!(fs::read_to_string(dir.path().join("other.txt")).unwrap(), "untracked\n");
        let review = crate::git_review::load_git_review(dir.path()).unwrap();
        assert_eq!(review.sections.len(), 1);
        assert_eq!(review.sections[0].title, "CAMBIOS PREPARADOS");
    }

    #[test]
    fn trash_only_takes_untracked_files_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("tracked.txt"), "tracked\n").unwrap();
        git(dir.path(), &["add", "tracked.txt"]);
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        let mut request = GitActionRequest {
            workspace: dir.path().into(),
            generation: 4,
            kind: GitActionKind::Trash,
            paths: vec!["new.txt".into()],
        };
        apply(&request, remove).unwrap();
        assert!(!dir.path().join("new.txt").exists());
        request.paths = vec!["tracked.txt".into()];
        assert!(apply(&request, remove).is_err());
        assert!(dir.path().join("tracked.txt").exists());
        request.paths = vec!["missing.txt".into()];
        assert!(apply(&request, remove).is_err());
        request.paths = vec!["../outside.txt".into()];
        assert_eq!(
            apply(&request, remove).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn a_whole_group_is_staged_across_several_command_lines() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        let paths: Vec<PathBuf> = (0..450).map(|index| format!("file-{index}.txt").into()).collect();
        for path in &paths {
            fs::write(dir.path().join(path), "x\n").unwrap();
        }
        apply(
            &GitActionRequest {
                workspace: dir.path().into(),
                generation: 5,
                kind: GitActionKind::Stage,
                paths,
            },
            no_trash,
        )
        .unwrap();
        let review = crate::git_review::load_git_review(dir.path()).unwrap();
        assert_eq!(review.sections[0].files.len(), 450);
        assert!(review.untracked.is_empty());
    }

    #[test]
    fn stage_and_unstage_are_literal_and_preserve_worktree_contents_on_an_unborn_branch() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("[x].txt"), "staged\n").unwrap();
        fs::write(dir.path().join("x.txt"), "untouched\n").unwrap();
        let mut request = GitActionRequest {
            workspace: dir.path().into(),
            generation: 1,
            kind: GitActionKind::Stage,
            paths: vec![PathBuf::from("[x].txt")],
        };
        apply(&request, no_trash).unwrap();
        let review = crate::git_review::load_git_review(dir.path()).unwrap();
        assert_eq!(review.sections[0].files[0].path, "[x].txt");
        assert_eq!(review.files, 1);
        fs::write(dir.path().join("[x].txt"), "new worktree edit\n").unwrap();
        request.kind = GitActionKind::Unstage;
        apply(&request, no_trash).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("[x].txt")).unwrap(),
            "new worktree edit\n"
        );
        assert!(crate::git_review::load_git_review(dir.path())
            .unwrap()
            .sections
            .is_empty());
        request.paths = vec![PathBuf::from("../outside")];
        assert_eq!(
            apply(&request, no_trash).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn unstage_a_rename_keeps_the_renamed_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "Test"]);
        git(
            dir.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(dir.path().join("before.txt"), "contents\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "initial"]);
        git(dir.path(), &["mv", "before.txt", "after.txt"]);
        apply(
            &GitActionRequest {
                workspace: dir.path().into(),
                generation: 2,
                kind: GitActionKind::Unstage,
                paths: vec!["before.txt".into(), "after.txt".into()],
            },
            no_trash,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("after.txt")).unwrap(),
            "contents\n"
        );
        assert!(!dir.path().join("before.txt").exists());
        let review = crate::git_review::load_git_review(dir.path()).unwrap();
        assert!(!review
            .sections
            .iter()
            .any(|section| section.title == "CAMBIOS PREPARADOS"));
    }
}
