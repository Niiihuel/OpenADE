//! Read-only, bounded Git diff projection for the right dock.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::sync::Arc;

use openide_ui::{CodeDiffLine, CodeDiffLineKind};
use std::path::{Component, Path};
use std::process::Stdio;

use crate::child_process::command_no_window;

const MAX_SUMMARY_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(test)]
const MAX_REVIEW_LINES: usize = 3_000;
const MAX_UNTRACKED_BYTES: u64 = 2 * 1024 * 1024;
const MAX_UNTRACKED_FILES: usize = 10_000;
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
    /// Prepared once off-thread; cloning a view retains the same editor cache.
    pub editor_lines: Arc<[CodeDiffLine]>,
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
    /// Unresolved files, which lead the unstaged section.
    pub conflicts: usize,
    /// Short hash and subject of HEAD, when there is a commit.
    pub last_commit: Option<(String, String)>,
}

/// The groups a list of changes shows, in their order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeGroup {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

impl ChangeGroup {
    pub const ALL: [Self; 4] = [Self::Conflicts, Self::Staged, Self::Changes, Self::Untracked];

    /// The letter that names the group in an action ID.
    pub fn key(self) -> &'static str {
        match self {
            Self::Conflicts => "c",
            Self::Staged => "s",
            Self::Changes => "u",
            Self::Untracked => "n",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|group| group.key() == key)
    }
}

/// Where a group's files are: `len` files of `sections[section]` from
/// `start`, or of `untracked` for that group.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GroupRange {
    pub section: usize,
    pub start: usize,
    pub len: usize,
}

/// The error of a review asked of a folder outside any work tree.
pub const NOT_A_REPOSITORY: &str = "Esta carpeta no es un repositorio de Git";

fn read_last_commit(workspace: &Path) -> Option<(String, String)> {
    let output = command_no_window("git")
        .args(["log", "-1", "--format=%h%x00%s"])
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (hash, subject) = text.trim_end().split_once('\0')?;
    (output.status.success() && !hash.is_empty())
        .then(|| (hash.to_owned(), subject.chars().take(200).collect()))
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
    /// Unresolved files lead the unstaged section and leave the staged one,
    /// so a list can show them as one group by range, without copying paths.
    pub fn group_conflicts(&mut self) {
        self.conflicts = 0;
        for section in &mut self.sections {
            if section.title == "CAMBIOS PREPARADOS" {
                let before = section.files.len();
                section.files.retain(|file| file.status != "Conflicto");
                self.files = self.files.saturating_sub(before - section.files.len());
            } else {
                section.files.sort_by_key(|file| file.status != "Conflicto");
                self.conflicts = section
                    .files
                    .iter()
                    .take_while(|file| file.status == "Conflicto")
                    .count();
            }
        }
        self.sections.retain(|section| !section.files.is_empty());
    }

    pub fn group(&self, group: ChangeGroup) -> GroupRange {
        let section = |title: &str| {
            self.sections
                .iter()
                .enumerate()
                .find(|(_, section)| section.title == title)
                .map(|(index, section)| (index, section.files.len()))
        };
        let unstaged = section("CAMBIOS SIN PREPARAR").unwrap_or_default();
        let conflicts = self.conflicts.min(unstaged.1);
        let (section, start, len) = match group {
            ChangeGroup::Conflicts => (unstaged.0, 0, conflicts),
            ChangeGroup::Changes => (unstaged.0, conflicts, unstaged.1 - conflicts),
            ChangeGroup::Staged => {
                let staged = section("CAMBIOS PREPARADOS").unwrap_or_default();
                (staged.0, 0, staged.1)
            }
            ChangeGroup::Untracked => (0, 0, self.untracked.len()),
        };
        GroupRange { section, start, len }
    }

    /// The group and the place in it of a row named `section:file`, or
    /// `n:index` for a file without tracking.
    pub fn locate(&self, target: &str) -> Option<(ChangeGroup, usize)> {
        let (first, second) = target.split_once(':')?;
        let index = second.parse::<usize>().ok()?;
        if first == "n" {
            return (index < self.untracked.len()).then_some((ChangeGroup::Untracked, index));
        }
        let section = first.parse::<usize>().ok()?;
        [ChangeGroup::Conflicts, ChangeGroup::Staged, ChangeGroup::Changes]
            .into_iter()
            .find_map(|group| {
                let range = self.group(group);
                (range.len > 0
                    && range.section == section
                    && (range.start..range.start + range.len).contains(&index))
                .then(|| (group, index - range.start))
            })
    }

    /// The name a row's actions carry for the file at `index` of `group`.
    pub fn target(&self, group: ChangeGroup, index: usize) -> String {
        let range = self.group(group);
        if group == ChangeGroup::Untracked {
            format!("n:{index}")
        } else {
            format!("{}:{}", range.section, range.start + index)
        }
    }

    /// What Git is given for the file at `index` of `group`: its path, then
    /// the path a rename left.
    pub fn paths(&self, group: ChangeGroup, index: usize) -> Vec<std::path::PathBuf> {
        if group == ChangeGroup::Untracked {
            return self.untracked.get(index).map(Into::into).into_iter().collect();
        }
        let range = self.group(group);
        let Some(file) = (index < range.len)
            .then(|| self.sections.get(range.section)?.files.get(range.start + index))
            .flatten()
        else {
            return Vec::new();
        };
        std::iter::once(&file.path)
            .chain(file.old_path.iter().filter(|old| *old != &file.path))
            .map(Into::into)
            .collect()
    }

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

#[cfg(test)]
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

fn read_diff_with_scope(
    workspace: &Path,
    staged: bool,
    paths: &[&Path],
    max_bytes: u64,
) -> io::Result<(String, bool)> {
    let mut command = command_no_window("git");
    command.arg("--literal-pathspecs").arg("diff").args([
        "--no-ext-diff",
        "--no-textconv",
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
        return if stage == DiffStage::Unstaged {
            load_untracked_diff(workspace, relative)
        } else {
            Ok(None)
        };
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
    Ok(file.map(|file| file_detail(stage, file, truncated || review.truncated)))
}

fn file_detail(stage: DiffStage, file: ReviewFile, truncated: bool) -> FileDiffDetail {
    let editor_lines = file
        .lines
        .iter()
        .filter_map(|line| {
            let kind = match line.kind {
                ReviewLineKind::Added => CodeDiffLineKind::Added,
                ReviewLineKind::Removed => CodeDiffLineKind::Removed,
                ReviewLineKind::Context | ReviewLineKind::Hunk => CodeDiffLineKind::Context,
                _ => return None,
            };
            let text = if line.kind == ReviewLineKind::Hunk {
                line.text.clone()
            } else {
                line.text.get(1..).unwrap_or_default().to_owned()
            };
            Some(CodeDiffLine {
                kind,
                old_line: line.old_number,
                new_line: line.new_number,
                text,
            })
        })
        .collect::<Vec<_>>()
        .into();
    FileDiffDetail {
        stage,
        file,
        editor_lines,
        truncated,
    }
}

/// Untracked files become additions only after Git confirms their status. Do
/// not follow symlinks out of the workspace or read ignored files on fallback.
fn load_untracked_diff(workspace: &Path, relative: &Path) -> io::Result<Option<FileDiffDetail>> {
    let status = command_no_window("git")
        .args([
            "--literal-pathspecs",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
        ])
        .arg(relative)
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()?;
    if !status.status.success() || !status.stdout.starts_with(b"?? ") {
        return Ok(None);
    }
    let expected = relative.to_string_lossy();
    let reported = status.stdout[3..]
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or_default();
    if String::from_utf8_lossy(reported) != expected {
        return Ok(None);
    }
    let root = workspace.canonicalize()?;
    let path = workspace.join(relative);
    let metadata = std::fs::symlink_metadata(&path)?;
    let mut file = ReviewFile {
        path: expected.into_owned(),
        status: "Nuevo".into(),
        ..Default::default()
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        file.status = "Enlace".into();
        return Ok(Some(file_detail(DiffStage::Unstaged, file, false)));
    }
    if !path.canonicalize()?.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "El archivo está fuera del proyecto",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE_DIFF_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let mut truncated = bytes.len() as u64 > MAX_FILE_DIFF_BYTES;
    bytes.truncate(MAX_FILE_DIFF_BYTES as usize);
    if bytes.contains(&0) {
        file.status = "Binario".into();
    } else {
        let text = String::from_utf8_lossy(&bytes);
        for (number, line) in text.lines().enumerate() {
            if number == MAX_FILE_DIFF_LINES {
                truncated = true;
                break;
            }
            file.lines.push(ReviewLine {
                kind: ReviewLineKind::Added,
                text: format!("+{line}"),
                old_number: None,
                new_number: Some(number + 1),
            });
        }
        file.additions = file.lines.len();
    }
    Ok(Some(file_detail(DiffStage::Unstaged, file, truncated)))
}

/// Read only path/status/stat metadata. Unlike a patch, this stays small even
/// when a generated file has millions of changed lines. NUL-delimited records
/// preserve tabs, newlines, unicode and rename source/destination pairs.
fn load_summary_section(
    workspace: &Path,
    staged: bool,
    title: &str,
) -> io::Result<(ReviewSection, bool)> {
    let mut command = command_no_window("git");
    command.args([
        "diff",
        "--raw",
        "--numstat",
        "-z",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--color=never",
    ]);
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
        .take(MAX_SUMMARY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > MAX_SUMMARY_BYTES;
    if truncated {
        let _ = child.kill();
        bytes.truncate(MAX_SUMMARY_BYTES as usize);
        if let Some(last) = bytes.iter().rposition(|byte| *byte == 0) {
            bytes.truncate(last + 1);
        } else {
            bytes.clear();
        }
    }
    let status = child.wait()?;
    if !truncated && !status.success() {
        return Err(io::Error::other(
            "Git no pudo leer los cambios del proyecto",
        ));
    }
    Ok((parse_summary_section(title, &bytes), truncated))
}

fn parse_summary_section(title: &str, bytes: &[u8]) -> ReviewSection {
    let mut files = BTreeMap::<String, ReviewFile>::new();
    let mut fields = bytes.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        if field.starts_with(b":") {
            let status = field
                .rsplit(|byte| *byte == b' ')
                .next()
                .and_then(|value| value.first())
                .copied()
                .unwrap_or(b'M');
            let Some(first_path) = fields.next() else {
                break;
            };
            let (old_path, path) = if matches!(status, b'R' | b'C') {
                let Some(path) = fields.next() else { break };
                (Some(String::from_utf8_lossy(first_path).into_owned()), path)
            } else {
                (None, first_path)
            };
            if path.is_empty() {
                continue;
            }
            let path = String::from_utf8_lossy(path).into_owned();
            // Unmerged files appear twice in --raw output (U, then M).
            // Preserve the unresolved status while accepting the later stats.
            if files
                .get(&path)
                .is_some_and(|file| file.status == "Conflicto")
            {
                continue;
            }
            files.insert(
                path.clone(),
                ReviewFile {
                    path,
                    old_path,
                    status: match status {
                        b'A' => "Nuevo",
                        b'D' => "Eliminado",
                        b'R' => "Renombrado",
                        b'C' => "Copiado",
                        b'U' => "Conflicto",
                        b'T' => "Tipo cambiado",
                        _ => "Modificado",
                    }
                    .into(),
                    ..Default::default()
                },
            );
        } else {
            let mut parts = field.splitn(3, |byte| *byte == b'\t');
            let (Some(added), Some(removed), Some(path)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let path = if path.is_empty() {
                let _old = fields.next();
                let Some(path) = fields.next() else { break };
                path
            } else {
                path
            };
            if let Some(file) = files.get_mut(String::from_utf8_lossy(path).as_ref()) {
                file.additions = std::str::from_utf8(added)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                file.deletions = std::str::from_utf8(removed)
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                if added == b"-" && removed == b"-" {
                    file.status = "Binario".into();
                }
            }
        }
    }
    let files: Vec<_> = files.into_values().collect();
    ReviewSection {
        title: title.into(),
        additions: files.iter().map(|file| file.additions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
        files,
    }
}

/// What counting the lines of new files may read in one review.
const MAX_UNTRACKED_LINE_BYTES: u64 = 8 * 1024 * 1024;

/// The lines of a new text file. A binary file, and one beyond what is left
/// of `budget`, counts none.
fn untracked_lines(path: &Path, budget: &mut u64) -> usize {
    let Ok(metadata) = std::fs::metadata(path) else {
        return 0;
    };
    if !metadata.is_file() || metadata.len() > *budget {
        return 0;
    }
    *budget -= metadata.len();
    match std::fs::read(path) {
        Ok(bytes) if !bytes.contains(&0) => bytes.split_inclusive(|byte| *byte == b'\n').count(),
        _ => 0,
    }
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
        paths.push(path.into_owned());
    }
    Ok((paths, truncated))
}

pub fn load_git_review(workspace: &Path) -> io::Result<GitReview> {
    load_review(workspace, None)
}

/// The review with each changed file's diff lines filled in, for a reader
/// that has no panel to open files in and may have no way to run git.
///
/// `max_diff_chars` is shared between the files rather than spent in order:
/// small diffs are kept whole and the rest split what is left, so every file
/// shows at least the start of its change. A file whose diff was shortened
/// ends with a line saying how much is missing, and the review is marked
/// truncated.
pub fn load_git_review_with_diffs(workspace: &Path, max_diff_chars: usize) -> io::Result<GitReview> {
    let mut review = load_review(workspace, Some(MAX_FILES_WITH_DIFF))?;
    let mut sizes: Vec<usize> = review
        .sections
        .iter()
        .flat_map(|section| &section.files)
        .map(|file| diff_chars(&file.lines))
        .collect();
    let allowance = fair_allowance(&mut sizes, max_diff_chars);
    for file in review.sections.iter_mut().flat_map(|section| &mut section.files) {
        let total_lines = file.lines.len();
        let mut used = 0;
        let kept = file
            .lines
            .iter()
            .take_while(|line| {
                used += line.text.chars().count() + 1;
                used <= allowance
            })
            .count();
        if kept < total_lines {
            file.lines.truncate(kept);
            file.lines.push(review_line(
                ReviewLineKind::Meta,
                &format!(
                    "[diff cut: {} more lines of {} not shown; read the file for the rest]",
                    total_lines - kept,
                    file.path
                ),
            ));
            review.truncated = true;
        }
    }
    Ok(review)
}

/// Beyond this many changed files the rest keep only their summary line; one
/// git call per file is made up to here.
const MAX_FILES_WITH_DIFF: usize = 200;

fn diff_chars(lines: &[ReviewLine]) -> usize {
    lines.iter().map(|line| line.text.chars().count() + 1).sum()
}

/// The largest per-file size such that giving every file `min(size, result)`
/// stays within `budget`: files smaller than an equal share keep everything
/// and pass their unused share on to the larger ones.
fn fair_allowance(sizes: &mut [usize], budget: usize) -> usize {
    sizes.sort_unstable();
    let mut remaining = budget;
    for (index, size) in sizes.iter().enumerate() {
        let share = remaining / (sizes.len() - index);
        if *size > share {
            return share;
        }
        remaining -= size;
    }
    usize::MAX
}

/// `diff_files` loads the diff lines of up to that many files.
fn load_review(workspace: &Path, diff_files: Option<usize>) -> io::Result<GitReview> {
    if !workspace.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "El proyecto no existe",
        ));
    }
    let inside = command_no_window("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .stderr(Stdio::null())
        .output()?;
    if !inside.status.success() || inside.stdout.trim_ascii() != b"true" {
        return Err(io::Error::other(NOT_A_REPOSITORY));
    }
    let mut review = GitReview {
        branch: read_branch(workspace),
        tracking: read_tracking(workspace),
        last_commit: read_last_commit(workspace),
        ..GitReview::default()
    };
    let mut loaded = 0usize;
    for (staged, title) in [
        (true, "CAMBIOS PREPARADOS"),
        (false, "CAMBIOS SIN PREPARAR"),
    ] {
        let (mut section, truncated) = load_summary_section(workspace, staged, title)?;
        if let Some(max_files) = diff_files {
            let stage = if staged {
                DiffStage::Staged
            } else {
                DiffStage::Unstaged
            };
            for file in &mut section.files {
                if loaded >= max_files {
                    review.truncated = true;
                    break;
                }
                loaded += 1;
                // A file whose diff cannot be read keeps its summary line.
                if let Ok(Some(detail)) = load_file_diff(workspace, Path::new(&file.path), stage) {
                    review.truncated |= detail.truncated;
                    file.lines = detail.file.lines;
                }
            }
        }
        review.files += section.files.len();
        review.additions += section.additions;
        review.deletions += section.deletions;
        if !section.files.is_empty() {
            review.sections.push(section);
        }
        review.truncated |= truncated;
    }
    let (untracked, truncated) = read_untracked(workspace)?;
    // A new file is all additions, as its diff shows once it is added.
    let mut budget = MAX_UNTRACKED_LINE_BYTES;
    review.additions += untracked
        .iter()
        .map(|path| untracked_lines(&workspace.join(path), &mut budget))
        .sum::<usize>();
    review.untracked = untracked;
    review.truncated |= truncated;
    review.group_conflicts();
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
    fn allowance_keeps_small_diffs_whole_and_splits_the_rest_evenly() {
        // Everything fits: no limit.
        assert_eq!(fair_allowance(&mut [10, 20, 30], 60), usize::MAX);
        assert_eq!(fair_allowance(&mut [], 100), usize::MAX);
        // Equal shares would be 100 each; the 10 passes its unused 90 on.
        assert_eq!(fair_allowance(&mut [500, 10, 500], 300), 145);
        // No small files: a plain equal split.
        assert_eq!(fair_allowance(&mut [900, 900, 900], 300), 100);
        // The result always fits the budget.
        for (sizes, budget) in [(vec![5usize, 400, 70, 3000, 1], 500usize), (vec![1000; 7], 10), (vec![3, 3, 3], 0)] {
            let mut sorted = sizes.clone();
            let allowance = fair_allowance(&mut sorted, budget);
            let spent: usize = sizes.iter().map(|size| (*size).min(allowance)).sum();
            assert!(spent <= budget, "{sizes:?} within {budget}: spent {spent}");
        }
    }

    #[test]
    fn a_new_untracked_file_counts_its_lines_as_additions() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        fs::write(root.path().join("prueba.md"), "# Prueba\n\nsin salto final").unwrap();
        fs::write(root.path().join("logo.bin"), [1, 0, 2, b'\n']).unwrap();
        let review = load_git_review(root.path()).unwrap();
        assert_eq!((review.files, review.untracked.len()), (0, 2));
        assert_eq!((review.additions, review.deletions), (3, 0));
    }

    #[test]
    fn a_large_change_shows_the_start_of_every_file_and_says_what_is_missing() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.email", "t@example.com"]);
        git(root.path(), &["config", "user.name", "t"]);
        for name in ["a.rs", "b.rs", "small.rs"] {
            fs::write(root.path().join(name), "base\n").unwrap();
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "base"]);
        let long = |tag: &str| (0..400).map(|line| format!("{tag} line {line}\n")).collect::<String>();
        fs::write(root.path().join("a.rs"), long("AAA")).unwrap();
        fs::write(root.path().join("b.rs"), long("BBB")).unwrap();
        fs::write(root.path().join("small.rs"), "PEQUENO\n").unwrap();

        let whole = load_git_review_with_diffs(root.path(), 1_000_000).unwrap();
        assert!(!whole.truncated);
        let cut = load_git_review_with_diffs(root.path(), 2_000).unwrap();
        assert!(cut.truncated);
        let files: Vec<&ReviewFile> = cut.sections.iter().flat_map(|section| &section.files).collect();
        assert_eq!(files.len(), 3);
        let text = |file: &ReviewFile| file.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
        let by_name = |name: &str| *files.iter().find(|file| file.path == name).unwrap();
        // The small file is whole; in order-of-arrival filling it would have had nothing left.
        assert!(text(by_name("small.rs")).contains("PEQUENO") && !text(by_name("small.rs")).contains("diff cut"));
        // Both large files show their beginning and name what is missing.
        for (name, tag) in [("a.rs", "AAA"), ("b.rs", "BBB")] {
            let shown = text(by_name(name));
            assert!(shown.contains(&format!("{tag} line 0")), "{name} shows nothing");
            assert!(!shown.contains(&format!("{tag} line 399")));
            assert!(shown.contains("[diff cut:") && shown.contains(name), "{shown}");
        }
        let total: usize = files.iter().map(|file| diff_chars(&file.lines)).sum();
        assert!(total < 2_000 + 3 * 120, "budget plus one marker per file: {total}");
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
        assert!(review.lines.is_empty());
        assert!(review
            .sections
            .iter()
            .flat_map(|section| &section.files)
            .all(|file| file.lines.is_empty()));
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
    fn lists_untracked_paths_and_never_reads_an_ignored_file() {
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
        // One line each for `.gitignore` and the large file; none for the
        // ignored one.
        assert_eq!(
            (review.files, review.additions, review.deletions),
            (0, 2, 0)
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
        assert_eq!(
            load_git_review(temp.path()).unwrap_err().to_string(),
            NOT_A_REPOSITORY
        );
    }

    #[test]
    fn groups_locate_rows_by_the_names_their_actions_carry() {
        let file = |path: &str, status: &str| ReviewFile {
            path: path.into(),
            status: status.into(),
            ..Default::default()
        };
        let mut review = GitReview {
            sections: vec![
                ReviewSection {
                    title: "CAMBIOS PREPARADOS".into(),
                    files: vec![file("staged.rs", "Modificado"), file("z.rs", "Conflicto")],
                    ..Default::default()
                },
                ReviewSection {
                    title: "CAMBIOS SIN PREPARAR".into(),
                    files: vec![
                        file("a.rs", "Modificado"),
                        ReviewFile {
                            old_path: Some("old.rs".into()),
                            ..file("new.rs", "Renombrado")
                        },
                        file("z.rs", "Conflicto"),
                    ],
                    ..Default::default()
                },
            ],
            untracked: vec!["notes.md".into()],
            files: 5,
            ..Default::default()
        };
        review.group_conflicts();
        assert_eq!((review.conflicts, review.files), (1, 4));
        assert_eq!(review.group(ChangeGroup::Staged), GroupRange { section: 0, start: 0, len: 1 });
        assert_eq!(review.group(ChangeGroup::Conflicts), GroupRange { section: 1, start: 0, len: 1 });
        assert_eq!(review.group(ChangeGroup::Changes), GroupRange { section: 1, start: 1, len: 2 });
        assert_eq!(review.locate("1:0"), Some((ChangeGroup::Conflicts, 0)));
        assert_eq!(review.locate("1:2"), Some((ChangeGroup::Changes, 1)));
        assert_eq!(review.locate("0:0"), Some((ChangeGroup::Staged, 0)));
        assert_eq!(review.locate("n:0"), Some((ChangeGroup::Untracked, 0)));
        for missing in ["0:1", "1:3", "2:0", "n:1", "x", "n:x"] {
            assert_eq!(review.locate(missing), None, "{missing}");
        }
        assert_eq!(review.target(ChangeGroup::Changes, 1), "1:2");
        assert_eq!(review.target(ChangeGroup::Untracked, 0), "n:0");
        assert_eq!(
            review.paths(ChangeGroup::Changes, 1),
            vec![std::path::PathBuf::from("new.rs"), "old.rs".into()]
        );
        assert_eq!(review.paths(ChangeGroup::Untracked, 0), vec![std::path::PathBuf::from("notes.md")]);
        assert!(review.paths(ChangeGroup::Changes, 2).is_empty());
    }

    #[test]
    fn a_merge_conflict_leads_the_unstaged_files_and_the_last_commit_is_named() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q", "--initial-branch=main"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "user.email", "test@example.invalid"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("z.txt"), "base\n").unwrap();
        fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "Base"]);
        git(dir, &["switch", "-q", "-c", "other"]);
        fs::write(dir.join("z.txt"), "theirs\n").unwrap();
        git(dir, &["commit", "-qam", "Theirs"]);
        git(dir, &["switch", "-q", "main"]);
        fs::write(dir.join("z.txt"), "ours\n").unwrap();
        git(dir, &["commit", "-qam", "Ours on main"]);
        let _ = command_no_window("git")
            .args(["merge", "-q", "other"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("a.txt"), "edited\n").unwrap();
        let review = load_git_review(dir).unwrap();
        assert_eq!(review.conflicts, 1);
        assert_eq!(review.sections.len(), 1);
        let files = &review.sections[0].files;
        assert_eq!(review.sections[0].title, "CAMBIOS SIN PREPARAR");
        assert_eq!((files[0].path.as_str(), files[0].status.as_str()), ("z.txt", "Conflicto"));
        assert_eq!(files[1].path, "a.txt");
        assert_eq!(review.last_commit.as_ref().unwrap().1, "Ours on main");
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
    #[test]
    fn summary_preserves_renames_binary_and_control_char_paths_without_hunks() {
        let bytes = b":100644 100644 abc def R100\0old name.txt\0new name.txt\0:100644 100644 abc def M\0a\tb\nc.bin\0:100644 000000 abc 000 D\0removed.txt\0";
        let mut bytes = bytes.to_vec();
        bytes.extend_from_slice(
            b"0\t0\t\0old name.txt\0new name.txt\0-\t-\ta\tb\nc.bin\03\t7\tremoved.txt\0",
        );
        let section = parse_summary_section("TEST", &bytes);
        assert_eq!(section.files.len(), 3);
        let renamed = section
            .files
            .iter()
            .find(|file| file.path == "new name.txt")
            .unwrap();
        assert_eq!(renamed.old_path.as_deref(), Some("old name.txt"));
        assert_eq!(renamed.status, "Renombrado");
        assert_eq!(section.files[0].status, "Binario");
        assert_eq!(section.files[0].path, "a\tb\nc.bin");
        assert_eq!((section.additions, section.deletions), (3, 7));
        assert!(section.files.iter().all(|file| file.lines.is_empty()));
    }

    #[test]
    fn summary_counts_a_large_patch_exactly_without_retaining_source_lines() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        fs::write(dir.join("generated.txt"), "old\n").unwrap();
        git(dir, &["add", "."]);
        fs::write(dir.join("generated.txt"), "new\n".repeat(15_000)).unwrap();
        let review = load_git_review(dir).unwrap();
        assert_eq!(review.additions, 15_001);
        assert_eq!(review.deletions, 1);
        assert!(!review.truncated);
        assert!(review.lines.is_empty());
        assert!(review
            .sections
            .iter()
            .flat_map(|section| &section.files)
            .all(|file| file.lines.is_empty()));
    }

    #[test]
    fn untracked_text_diff_is_loaded_only_on_demand_and_ignored_files_stay_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q"]);
        fs::write(dir.join(".gitignore"), "hidden.txt\n").unwrap();
        fs::write(dir.join("hidden.txt"), "hidden\n").unwrap();
        fs::write(dir.join("new file.txt"), "one\ntwo\n").unwrap();
        let detail = load_file_diff(dir, Path::new("new file.txt"), DiffStage::Unstaged)
            .unwrap()
            .unwrap();
        assert_eq!(detail.file.status, "Nuevo");
        assert_eq!(detail.file.additions, 2);
        assert_eq!(detail.editor_lines[1].text, "two");
        assert_eq!(detail.editor_lines[1].new_line, Some(2));
        assert!(
            load_file_diff(dir, Path::new("hidden.txt"), DiffStage::Unstaged)
                .unwrap()
                .is_none()
        );
        assert!(
            load_file_diff(dir, Path::new("new file.txt"), DiffStage::Staged)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn untracked_symlink_does_not_read_a_file_outside_the_project() {
        let temp = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        fs::write(external.path().join("private.txt"), "not workspace content").unwrap();
        std::os::unix::fs::symlink(
            external.path().join("private.txt"),
            temp.path().join("link"),
        )
        .unwrap();
        let detail = load_file_diff(temp.path(), Path::new("link"), DiffStage::Unstaged)
            .unwrap()
            .unwrap();
        assert_eq!(detail.file.status, "Enlace");
        assert!(detail.file.lines.is_empty());
    }
    #[test]
    fn unmerged_raw_record_is_not_overwritten_by_the_worktree_record() {
        let section = parse_summary_section("CHANGES", b":000000 100644 0000000 0000000 U\0conflict.txt\0:100644 100644 ba2906d 0000000 M\0conflict.txt\00\t0\tconflict.txt\04\t0\tconflict.txt\0");
        assert_eq!(section.files.len(), 1);
        assert_eq!(section.files[0].status, "Conflicto");
        assert_eq!(section.files[0].additions, 4);
    }
}
