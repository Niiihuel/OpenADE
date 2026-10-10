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
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<GitActionRequest>();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-git-actions".into())
            .spawn(move || {
                // Unlike read requests, explicit mutations are never coalesced.
                while let Ok(request) = request_rx.recv() {
                    let result = apply(&request).map_err(|error| error.to_string());
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

fn apply(request: &GitActionRequest) -> io::Result<()> {
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
    let mut command = command_no_window("git");
    command.arg("--literal-pathspecs");
    match request.kind {
        GitActionKind::Stage => {
            command.arg("add");
        }
        GitActionKind::Unstage => {
            let has_head = command_no_window("git")
                .args(["rev-parse", "--verify", "HEAD"])
                .current_dir(&request.workspace)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success();
            if has_head {
                command.args(["reset", "--quiet", "HEAD"]);
            } else {
                // An unborn branch has no HEAD. Removing only index entries
                // preserves worktree contents, including edits after staging.
                command.args(["rm", "--cached", "--force", "--ignore-unmatch"]);
            }
        }
    }
    let output = command
        .arg("--")
        .args(&request.paths)
        .current_dir(&request.workspace)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr)
        .trim()
        .chars()
        .take(1600)
        .collect::<String>();
    Err(io::Error::other(if message.is_empty() {
        "Git no pudo actualizar los archivos preparados".into()
    } else {
        message
    }))
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
        apply(&request).unwrap();
        let review = crate::git_review::load_git_review(dir.path()).unwrap();
        assert_eq!(review.sections[0].files[0].path, "[x].txt");
        assert_eq!(review.files, 1);
        fs::write(dir.path().join("[x].txt"), "new worktree edit\n").unwrap();
        request.kind = GitActionKind::Unstage;
        apply(&request).unwrap();
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
            apply(&request).unwrap_err().kind(),
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
        apply(&GitActionRequest {
            workspace: dir.path().into(),
            generation: 2,
            kind: GitActionKind::Unstage,
            paths: vec!["before.txt".into(), "after.txt".into()],
        })
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
