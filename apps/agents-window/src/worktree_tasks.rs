//! Tasks that work in a Git worktree of their own, beside the repository:
//! `<repo>.worktrees/<id>` on a new branch `openade/<id>`, prepared by the
//! repository's `.openide/setup` and removed with its last task when clean.

use std::path::{Path, PathBuf};

use crate::git_branches::{git_error, run_git};

const SETUP_FILE: &str = ".openide/setup";
const MAX_SETUP_BYTES: u64 = 8 * 1024;

/// Where a task worktree named `id` of the repository at `root` goes, and
/// its branch.
pub(crate) fn task_worktree(root: &Path, id: &str) -> Option<(PathBuf, String)> {
    let name = root.file_name()?.to_str()?;
    Some((root.parent()?.join(format!("{name}.worktrees")).join(id), format!("openade/{id}")))
}

/// Whether `workspace` is one of those worktrees.
pub(crate) fn is_task_worktree(workspace: &Path) -> bool {
    workspace
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".worktrees"))
}

/// The commands of the repository's `.openide/setup`, one per line (`#`
/// starts a comment), as the single line a terminal runs; `None` without
/// one. A link, an oversized file or a control character is refused.
pub(crate) fn setup_command(workspace: &Path) -> Option<String> {
    let path = workspace.join(SETUP_FILE);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SETUP_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.is_empty() || lines.iter().any(|line| line.chars().any(char::is_control)) {
        return None;
    }
    Some(lines.join(" && "))
}

/// Remove the worktree at `workspace` when it has nothing uncommitted. Its
/// branch stays. `Ok(false)` when it was kept.
pub(crate) fn remove_if_clean(workspace: &Path) -> Result<bool, String> {
    let status = run_git(workspace, &["status", "--porcelain"])?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Ok(false);
    }
    let common = run_git(workspace, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let common = PathBuf::from(String::from_utf8_lossy(&common.stdout).trim());
    let repository = common.parent().ok_or("No se encontró el repositorio del worktree")?;
    let path = workspace.to_str().ok_or("La ruta del worktree no es texto")?;
    let removed = run_git(repository, &["worktree", "remove", path])?;
    if removed.status.success() {
        Ok(true)
    } else {
        Err(git_error(&removed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn git(dir: &Path, args: &[&str]) {
        let env = [
            ("GIT_AUTHOR_NAME", OsStr::new("t")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("t@t")),
            ("GIT_COMMITTER_NAME", OsStr::new("t")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("t@t")),
        ];
        let output = crate::git_branches::run_git_env(dir, args, &env).unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn a_task_worktree_sits_beside_the_repository_and_goes_when_clean() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        std::fs::create_dir_all(root.join(".openide")).unwrap();
        std::fs::write(root.join(".openide/setup"), "# preparar\nnpm install\n\ncp .env.example .env\n").unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "one"]);
        let (path, branch) = task_worktree(&root, "a1b2c3d4").unwrap();
        assert_eq!(path, temp.path().join("app.worktrees/a1b2c3d4"));
        assert_eq!(branch, "openade/a1b2c3d4");
        git(&root, &["worktree", "add", "-b", &branch, "--", path.to_str().unwrap(), "HEAD"]);
        assert!(is_task_worktree(&path) && !is_task_worktree(&root));
        assert_eq!(setup_command(&path).as_deref(), Some("npm install && cp .env.example .env"));

        std::fs::write(path.join("work.txt"), "sin guardar").unwrap();
        assert_eq!(remove_if_clean(&path), Ok(false), "kept with uncommitted work");
        std::fs::remove_file(path.join("work.txt")).unwrap();
        assert_eq!(remove_if_clean(&path), Ok(true));
        assert!(!path.exists());
    }

    #[test]
    fn a_setup_file_that_is_a_link_or_has_control_characters_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".openide")).unwrap();
        assert_eq!(setup_command(temp.path()), None);
        std::fs::write(temp.path().join(".openide/setup"), "echo hola\u{1b}[2J\n").unwrap();
        assert_eq!(setup_command(temp.path()), None);
        #[cfg(unix)]
        {
            std::fs::remove_file(temp.path().join(".openide/setup")).unwrap();
            std::fs::write(temp.path().join("elsewhere"), "echo hola\n").unwrap();
            std::os::unix::fs::symlink(temp.path().join("elsewhere"), temp.path().join(".openide/setup")).unwrap();
            assert_eq!(setup_command(temp.path()), None);
        }
    }
}
