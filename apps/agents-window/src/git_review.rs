//! Read-only, bounded Git diff projection for the right dock.

use std::io::{self, Read};
use std::path::{Component, Path};
use std::process::Stdio;

use crate::child_process::command_no_window;

const MAX_DIFF_BYTES: u64 = 512 * 1024;
const MAX_REVIEW_LINES: usize = 3_000;
const MAX_UNTRACKED_BYTES: u64 = 128 * 1024;
const MAX_UNTRACKED_FILES: usize = 200;
const MAX_FILE_DIFF_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILE_DIFF_LINES: usize = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffStage {
    Staged,
    Unstaged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDiffDetail {
    pub stage: DiffStage,
    pub file: ReviewFile,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewLineKind {
    File,
    Hunk,
    Added,
    Removed,
    Context,
    Meta,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewLine {
    pub kind: ReviewLineKind,
    pub text: String,
    pub old_number: Option<usize>,
    pub new_number: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub lines: Vec<ReviewLine>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewSection {
    pub title: String,
    pub files: Vec<ReviewFile>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitTracking {
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GitReview {
    pub branch: String,
    pub tracking: Option<GitTracking>,
    pub github: crate::github::GithubStatus,
    pub sections: Vec<ReviewSection>,
    pub lines: Vec<ReviewLine>,
    pub untracked: Vec<String>,
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub truncated: bool,
}

pub(crate) fn read_branch(workspace: &Path) -> String {
    let output = command_no_window("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let name = String::from_utf8_lossy(&output.stdout);
            let name = name.trim();
            if name.is_empty() {
                "Rama desconocida".to_owned()
            } else {
                name.chars().take(256).collect()
            }
        }
        Ok(_) => {
            let detached = command_no_window("git")
                .args(["rev-parse", "--verify", "HEAD"])
                .current_dir(workspace)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if detached {
                "HEAD separado".to_owned()
            } else {
                "Rama desconocida".to_owned()
            }
        }
        Err(_) => "Rama desconocida".to_owned(),
    }
}

fn read_tracking(workspace: &Path) -> Option<GitTracking> {
    let upstream = command_no_window("git")
        .args([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !upstream.status.success() {
        return None;
    }
    let upstream = String::from_utf8_lossy(&upstream.stdout).trim().to_owned();
    if upstream.is_empty() || upstream.len() > 256 {
        return None;
    }
    let counts = command_no_window("git")
        .args(["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !counts.status.success() {
        return None;
    }
    let counts = String::from_utf8_lossy(&counts.stdout);
    let mut parts = counts.split_whitespace();
    let behind = parts.next()?.parse().ok()?;
    let ahead = parts.next()?.parse().ok()?;
    Some(GitTracking {
        upstream,
        ahead,
        behind,
    })
}

impl GitReview {
    pub fn unique_files(&self) -> usize {
        let mut paths = std::collections::HashSet::new();
        for section in &self.sections {
            for file in &section.files {
                paths.insert(&file.path);
            }
        }
        paths.len()
    }
}

fn classify(line: &str) -> ReviewLineKind {
    if line.starts_with("diff --git ") {
        ReviewLineKind::File
    } else if line.starts_with("@@") {
        ReviewLineKind::Hunk
    } else if line.starts_with('+') && !line.starts_with("+++") {
        ReviewLineKind::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        ReviewLineKind::Removed
    } else if line.starts_with(' ') {
        ReviewLineKind::Context
    } else {
        ReviewLineKind::Meta
    }
}

fn path_from_diff_header(header: &str) -> String {
    if let Some(start) = header.rfind(" \"b/") {
        return decode_git_path(&header[start + 1..])
            .strip_prefix("b/")
            .unwrap_or(header)
            .to_owned();
    }
    header
        .rsplit_once(" b/")
        .map(|(_, path)| path.trim_end_matches('"'))
        .unwrap_or(header)
        .to_owned()
}

fn decode_git_path(raw: &str) -> String {
    let raw = raw.trim_end_matches('\t');
    let Some(quoted) = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return raw.to_owned();
    };
    let bytes = quoted.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' || index + 1 >= bytes.len() {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes[index] {
            b'0'..=b'7' => {
                let mut value = 0_u16;
                let mut digits = 0;
                while digits < 3 && index < bytes.len() && (b'0'..=b'7').contains(&bytes[index]) {
                    value = value * 8 + u16::from(bytes[index] - b'0');
                    index += 1;
                    digits += 1;
                }
                decoded.push(value as u8);
            }
            b'n' => {
                decoded.push(b'\n');
                index += 1;
            }
            b'r' => {
                decoded.push(b'\r');
                index += 1;
            }
            b't' => {
                decoded.push(b'\t');
                index += 1;
            }
            escaped => {
                decoded.push(escaped);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn diff_marker_path(raw: &str, side: char) -> Option<String> {
    let raw = raw.split_once('\t').map_or(raw, |(path, _)| path);
    let decoded = decode_git_path(raw);
    decoded.strip_prefix(&format!("{side}/")).map(str::to_owned)
}

fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let range = header.split("@@").nth(1)?;
    let mut parts = range.split_whitespace();
    let old = parts
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = parts
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

fn review_line(kind: ReviewLineKind, text: impl Into<String>) -> ReviewLine {
    ReviewLine {
        kind,
        text: text.into(),
        old_number: None,
        new_number: None,
    }
}

fn parse_section(review: &mut GitReview, section: &str, text: &str) {
    parse_section_with_limit(review, section, text, MAX_REVIEW_LINES);
}

fn parse_section_with_limit(review: &mut GitReview, section: &str, text: &str, limit: usize) {
    if text.is_empty() {
        return;
    }
    if !review.lines.is_empty() {
        review.lines.push(review_line(ReviewLineKind::Meta, ""));
    }
    review
        .lines
        .push(review_line(ReviewLineKind::Meta, section));
    let mut group = ReviewSection {
        title: section.to_owned(),
        ..ReviewSection::default()
    };
    let mut current_file: Option<ReviewFile> = None;
    let mut numbers: Option<(usize, usize)> = None;
    for line in text.lines() {
        if review.lines.len() >= limit {
            review.truncated = true;
            break;
        }
        let kind = classify(line);
        if kind == ReviewLineKind::File {
            if let Some(file) = current_file.take() {
                group.files.push(file);
            }
            current_file = Some(ReviewFile {
                path: path_from_diff_header(line),
                status: "Modificado".to_owned(),
                ..ReviewFile::default()
            });
            numbers = None;
        }
        if let Some(file) = current_file.as_mut() {
            if let Some(path) = line.strip_prefix("+++ ") {
                if let Some(path) = diff_marker_path(path, 'b') {
                    file.path = path;
                }
            } else if let Some(path) = line.strip_prefix("--- ") {
                if let Some(path) = diff_marker_path(path, 'a') {
                    file.old_path = Some(path);
                }
            } else if line.starts_with("new file mode") {
                file.status = "Nuevo".to_owned();
            } else if line.starts_with("deleted file mode") {
                file.status = "Eliminado".to_owned();
            } else if let Some(path) = line.strip_prefix("rename from ") {
                file.old_path = Some(decode_git_path(path));
            } else if line.starts_with("rename to ") {
                file.status = "Renombrado".to_owned();
                file.path = decode_git_path(line.trim_start_matches("rename to "));
            } else if line.starts_with("Binary files ") {
                file.status = "Binario".to_owned();
            }
        }
        match kind {
            ReviewLineKind::File => review.files += 1,
            ReviewLineKind::Added => {
                review.additions += 1;
                group.additions += 1;
                if let Some(file) = current_file.as_mut() {
                    file.additions += 1;
                }
            }
            ReviewLineKind::Removed => {
                review.deletions += 1;
                group.deletions += 1;
                if let Some(file) = current_file.as_mut() {
                    file.deletions += 1;
                }
            }
            _ => {}
        }
        let mut projected = review_line(kind, line);
        if kind == ReviewLineKind::Hunk {
            numbers = hunk_start(line);
        } else if let Some((old, new)) = numbers.as_mut() {
            match kind {
                ReviewLineKind::Added => {
                    projected.new_number = Some(*new);
                    *new += 1;
                }
                ReviewLineKind::Removed => {
                    projected.old_number = Some(*old);
                    *old += 1;
                }
                ReviewLineKind::Context => {
                    projected.old_number = Some(*old);
                    projected.new_number = Some(*new);
                    *old += 1;
                    *new += 1;
                }
                _ => {}
            }
        }
        if let Some(file) = current_file.as_mut() {
            if matches!(
                kind,
                ReviewLineKind::Hunk
                    | ReviewLineKind::Added
                    | ReviewLineKind::Removed
                    | ReviewLineKind::Context
            ) {
                file.lines.push(projected.clone());
            }
        }
        review.lines.push(projected);
    }
    if let Some(file) = current_file {
        group.files.push(file);
    }
    if !group.files.is_empty() {
        review.sections.push(group);
    }
}

fn read_diff(workspace: &Path, staged: bool) -> io::Result<(String, bool)> {
    read_diff_with_scope(workspace, staged, &[], MAX_DIFF_BYTES)
}

fn read_diff_with_scope(
    workspace: &Path,
    staged: bool,
    paths: &[&Path],
    max_bytes: u64,
) -> io::Result<(String, bool)> {
    let mut command = command_no_window("git");
    command.arg("--literal-pathspecs").arg("diff").args([
        "--no-ext-diff",
        "--color=never",
        "--find-renames",
        "--unified=3",
    ]);
    if staged {
        command.arg("--cached");
    }
    command.arg("--");
    for path in paths {
        command.arg(path);
    }
    let mut child = command
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > max_bytes;
    if truncated {
        let _ = child.kill();
        bytes.truncate(max_bytes as usize);
        if let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
            bytes.truncate(last_newline + 1);
        } else {
            bytes.clear();
        }
    }
    let status = child.wait()?;
    if !truncated && !status.success() {
        return Err(io::Error::other("git diff no pudo leer los cambios"));
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn rename_origin(workspace: &Path, relative: &Path, staged: bool) -> io::Result<Option<String>> {
    let mut command = command_no_window("git");
    command
        .arg("diff")
        .args(["--name-status", "-z", "--find-renames"]);
    if staged {
        command.arg("--cached");
    }
    let mut child = command
        .arg("--")
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_FILE_DIFF_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_DIFF_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    }
    if !child.wait()?.success() {
        return Ok(None);
    }
    let requested = relative.to_string_lossy().replace('\\', "/");
    let mut fields = bytes.split(|byte| *byte == 0);
    while let Some(status) = fields.next() {
        if status.is_empty() {
            break;
        }
        let Some(old) = fields.next() else { break };
        if !matches!(status.first(), Some(b'R' | b'C')) {
            continue;
        }
        let Some(new) = fields.next() else { break };
        if String::from_utf8_lossy(new) == requested {
            return Ok(Some(String::from_utf8_lossy(old).into_owned()));
        }
    }
    Ok(None)
}

/// Loads one complete file diff on demand. The summary stays small and the UI
/// can request this only when the user opens a file. Deleted files are valid
/// targets, so the path is checked lexically rather than canonicalized.
pub fn load_file_diff(
    workspace: &Path,
    relative: &Path,
    stage: DiffStage,
) -> io::Result<Option<FileDiffDetail>> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid relative diff path",
        ));
    }
    if !workspace.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "El proyecto no existe",
        ));
    }
    let staged = stage == DiffStage::Staged;
    let (mut text, mut truncated) =
        read_diff_with_scope(workspace, staged, &[relative], MAX_FILE_DIFF_BYTES)?;
    if text.is_empty() {
        return Ok(None);
    }
    let mut review = GitReview::default();
    parse_section_with_limit(&mut review, "ARCHIVO", &text, MAX_FILE_DIFF_LINES);
    if review
        .sections
        .iter()
        .flat_map(|section| &section.files)
        .any(|file| file.status == "Nuevo")
    {
        if let Some(old_path) = rename_origin(workspace, relative, staged)? {
            let old_path = Path::new(&old_path);
            let (renamed_text, renamed_truncated) = read_diff_with_scope(
                workspace,
                staged,
                &[old_path, relative],
                MAX_FILE_DIFF_BYTES,
            )?;
            if !renamed_text.is_empty() {
                text = renamed_text;
                truncated = renamed_truncated;
                review = GitReview::default();
                parse_section_with_limit(&mut review, "ARCHIVO", &text, MAX_FILE_DIFF_LINES);
            }
        }
    }
    let requested = relative.to_string_lossy().replace('\\', "/");
    let file = review
        .sections
        .into_iter()
        .flat_map(|section| section.files)
        .find(|file| {
            file.path == requested || file.old_path.as_deref() == Some(requested.as_str())
        });
    Ok(file.map(|file| FileDiffDetail {
        stage,
        file,
        truncated: truncated || review.truncated,
    }))
}

fn read_untracked(workspace: &Path) -> io::Result<(Vec<String>, bool)> {
    let mut child = command_no_window("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_UNTRACKED_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let mut truncated = bytes.len() as u64 > MAX_UNTRACKED_BYTES;
    if truncated {
        let _ = child.kill();
        bytes.truncate(MAX_UNTRACKED_BYTES as usize);
    }
    let status = child.wait()?;
    if !truncated && !status.success() {
        return Err(io::Error::other(
            "git ls-files no pudo leer los archivos sin seguimiento",
        ));
    }
    let mut paths = Vec::new();
    for chunk in bytes.split_inclusive(|byte| *byte == 0) {
        if chunk.last() != Some(&0) {
            truncated = true;
            break;
        }
        let path = &chunk[..chunk.len() - 1];
        if path.is_empty() {
            continue;
        }
        if paths.len() == MAX_UNTRACKED_FILES {
            truncated = true;
            break;
        }
        let path = String::from_utf8_lossy(path);
        paths.push(path.chars().filter(|ch| !ch.is_control()).collect());
    }
    Ok((paths, truncated))
}

pub fn load_git_review(workspace: &Path) -> io::Result<GitReview> {
    if !workspace.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "El proyecto no existe",
        ));
    }
    let mut review = GitReview {
        branch: read_branch(workspace),
        tracking: read_tracking(workspace),
        ..GitReview::default()
    };
    for (staged, title) in [
        (true, "CAMBIOS PREPARADOS"),
        (false, "CAMBIOS SIN PREPARAR"),
    ] {
        let (text, truncated) = read_diff(workspace, staged)?;
        parse_section(&mut review, title, &text);
        review.truncated |= truncated;
    }
    let (untracked, truncated) = read_untracked(workspace)?;
    review.untracked = untracked;
    review.truncated |= truncated;
    Ok(review)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(workspace: &Path, args: &[&str]) {
        let output = command_no_window("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .expect("git available for repository test");
        assert!(
            output.status.success(),
            "git {args:?} exited {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn diff_projection_counts_only_real_additions_and_deletions() {
        let mut review = GitReview::default();
        parse_section(
            &mut review,
            "CAMBIOS",
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n context",
        );
        assert_eq!(
            (review.files, review.additions, review.deletions),
            (1, 1, 1)
        );
        assert_eq!(review.lines[5].kind, ReviewLineKind::Removed);
        assert_eq!(review.lines[6].kind, ReviewLineKind::Added);
        let file = &review.sections[0].files[0];
        assert_eq!(file.path, "a.rs");
        assert_eq!((file.additions, file.deletions), (1, 1));
        assert_eq!(file.lines[1].old_number, Some(1));
        assert_eq!(file.lines[1].new_number, None);
        assert_eq!(file.lines[2].old_number, None);
        assert_eq!(file.lines[2].new_number, Some(1));
        assert_eq!(file.status, "Modificado");
    }

    #[test]
    fn quoted_git_paths_decode_spaces_unicode_and_escapes() {
        assert_eq!(
            diff_marker_path("\"b/space name/\\303\\251.txt\"", 'b'),
            Some("space name/é.txt".to_owned())
        );
        assert_eq!(
            path_from_diff_header("diff --git \"a/space name/a.txt\" \"b/space name/a.txt\""),
            "space name/a.txt"
        );
    }

    #[test]
    fn empty_section_does_not_create_a_fake_review() {
        let mut review = GitReview::default();
        parse_section(&mut review, "CAMBIOS", "");
        assert!(review.lines.is_empty());
    }

    #[test]
    fn file_header_keeps_spaces_without_git_separator() {
        let mut review = GitReview::default();
        parse_section(
            &mut review,
            "CAMBIOS",
            "diff --git a/folder/a file.txt b/folder/a file.txt\n--- a/folder/a file.txt\t\n+++ b/folder/a file.txt\t\n@@ -1 +1 @@\n-before\n+after",
        );
        assert_eq!(review.sections[0].files[0].path, "folder/a file.txt");
    }

    #[test]
    fn reads_staged_and_unstaged_changes_from_a_real_repository() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        git(
            dir,
            &["config", "user.email", "openide-test@example.invalid"],
        );
        git(dir, &["config", "user.name", "OpenIDE Test"]);
        fs::write(dir.join("a.txt"), "first\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-qm", "initial"]);

        fs::write(dir.join("a.txt"), "second\n").unwrap();
        git(dir, &["add", "a.txt"]);
        fs::write(dir.join("a.txt"), "third\n").unwrap();

        let review = load_git_review(dir).unwrap();
        assert_eq!(
            (review.files, review.additions, review.deletions),
            (2, 2, 2)
        );
        assert!(review.lines.iter().any(|line| line.text == "+second"));
        assert!(review.lines.iter().any(|line| line.text == "+third"));
        assert!(!review.truncated);
        assert_eq!(review.unique_files(), 1);
        assert_eq!(review.sections.len(), 2);
        assert_eq!(review.sections[0].files[0].path, "a.txt");
        assert_eq!(review.sections[1].files[0].path, "a.txt");
        assert!(!review.branch.is_empty());

        let staged = load_file_diff(dir, Path::new("a.txt"), DiffStage::Staged)
            .unwrap()
            .unwrap();
        assert_eq!(staged.file.path, "a.txt");
        assert_eq!((staged.file.additions, staged.file.deletions), (1, 1));
        assert!(staged.file.lines.iter().any(|line| line.text == "+second"));
        assert!(!staged.file.lines.iter().any(|line| line.text == "+third"));
        let unstaged = load_file_diff(dir, Path::new("a.txt"), DiffStage::Unstaged)
            .unwrap()
            .unwrap();
        assert!(unstaged.file.lines.iter().any(|line| line.text == "+third"));
        assert!(!unstaged.truncated);
        assert!(
            load_file_diff(dir, Path::new("unchanged.txt"), DiffStage::Unstaged)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            load_file_diff(dir, Path::new("../a.txt"), DiffStage::Staged)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn targeted_diff_resolves_renames_deletions_and_quoted_paths() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        git(
            dir,
            &["config", "user.email", "openide-test@example.invalid"],
        );
        git(dir, &["config", "user.name", "OpenIDE Test"]);
        fs::write(dir.join("old name-é.txt"), "old\n").unwrap();
        fs::write(dir.join("delete me.txt"), "gone\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "initial"]);
        git(dir, &["mv", "old name-é.txt", "new name-é.txt"]);
        git(dir, &["rm", "-q", "delete me.txt"]);

        let renamed = load_file_diff(dir, Path::new("new name-é.txt"), DiffStage::Staged)
            .unwrap()
            .unwrap();
        assert_eq!(renamed.file.path, "new name-é.txt");
        assert_eq!(renamed.file.old_path.as_deref(), Some("old name-é.txt"));
        assert_eq!(renamed.file.status, "Renombrado");
        let deleted = load_file_diff(dir, Path::new("delete me.txt"), DiffStage::Staged)
            .unwrap()
            .unwrap();
        assert_eq!(deleted.file.path, "delete me.txt");
        assert_eq!(deleted.file.status, "Eliminado");
        assert!(deleted.file.lines.iter().any(|line| line.text == "-gone"));
    }

    #[test]
    fn targeted_diff_marks_a_large_file_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        git(
            dir,
            &["config", "user.email", "openide-test@example.invalid"],
        );
        git(dir, &["config", "user.name", "OpenIDE Test"]);
        fs::write(dir.join("large.txt"), "before\n").unwrap();
        git(dir, &["add", "large.txt"]);
        git(dir, &["commit", "-qm", "initial"]);
        fs::write(
            dir.join("large.txt"),
            vec![b'x'; MAX_FILE_DIFF_BYTES as usize + 100],
        )
        .unwrap();
        let detail = load_file_diff(dir, Path::new("large.txt"), DiffStage::Unstaged)
            .unwrap()
            .unwrap();
        assert!(detail.truncated);
        assert_eq!(detail.file.path, "large.txt");
        assert!(!detail
            .file
            .lines
            .iter()
            .any(|line| line.kind == ReviewLineKind::Added));
    }

    #[test]
    fn lists_untracked_paths_without_reading_their_contents() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        fs::write(dir.join(".gitignore"), "hidden.txt\n").unwrap();
        fs::write(dir.join("hidden.txt"), "ignored").unwrap();
        fs::create_dir(dir.join("folder")).unwrap();
        fs::write(
            dir.join("folder").join("large file.txt"),
            vec![b'x'; 600_000],
        )
        .unwrap();

        let review = load_git_review(dir).unwrap();
        assert_eq!(
            (review.files, review.additions, review.deletions),
            (0, 0, 0)
        );
        assert!(review
            .untracked
            .iter()
            .any(|path| path == "folder/large file.txt"));
        assert!(!review.untracked.iter().any(|path| path == "hidden.txt"));
        assert!(!review.truncated);
        assert!(!review.branch.is_empty());
    }

    #[test]
    fn detached_head_has_an_explicit_label() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        git(
            dir,
            &["config", "user.email", "openide-test@example.invalid"],
        );
        git(dir, &["config", "user.name", "OpenIDE Test"]);
        fs::write(dir.join("a.txt"), "first\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-qm", "initial"]);
        git(dir, &["checkout", "--detach", "-q", "HEAD"]);

        assert_eq!(load_git_review(dir).unwrap().branch, "HEAD separado");
    }

    #[test]
    fn non_repository_has_no_false_detached_head_label() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(read_branch(temp.path()), "Rama desconocida");
    }

    #[test]
    fn tracking_counts_ahead_and_behind_against_upstream() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        let work = temp.path().join("work");
        fs::create_dir(&remote).unwrap();
        fs::create_dir(&work).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        git(
            &work,
            &["config", "user.email", "openide-test@example.invalid"],
        );
        git(&work, &["config", "user.name", "OpenIDE Test"]);
        fs::write(work.join("a.txt"), "first\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-qm", "first"]);
        git(
            &work,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        git(&work, &["push", "-qu", "origin", "main"]);
        fs::write(work.join("a.txt"), "second\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-qm", "second"]);
        let tracking = load_git_review(&work).unwrap().tracking.unwrap();
        assert_eq!(tracking.upstream, "origin/main");
        assert_eq!((tracking.ahead, tracking.behind), (1, 0));
    }
}
