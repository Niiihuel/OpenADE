//! Read-only workspace browser data for the sidebar and right preview.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, ErrorKind};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

pub const MAX_DIRECTORY_ENTRIES: usize = 200;
pub const MAX_DIRECTORY_ENTRIES_TOTAL: usize = 2_000;
pub const MAX_VISIBLE_ROWS: usize = 2_048;
pub const MAX_EXPANDED_DIRECTORIES: usize = 512;
#[cfg(test)]
pub const MAX_PREVIEW_BYTES: u64 = 256 * 1024;
pub const MAX_PREVIEW_PAGE_LINES: usize = 200;
pub const MAX_PREVIEW_SCAN_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PREVIEW_SCAN_LINES: usize = 100_000;
const MAX_PREVIEW_LINE_CHARS: usize = 2_000;
pub const MAX_SEARCH_DEPTH: usize = 12;
pub const MAX_SEARCH_DIRECTORIES: usize = 512;
pub const MAX_SEARCH_ENTRIES: usize = 16_384;
pub const MAX_SEARCH_ENTRIES_PER_DIRECTORY: usize = 1_024;
pub const MAX_SEARCH_RESULTS: usize = 80;

/// File names found below one workspace. Every hit is relative to that
/// workspace and must still pass `read_text_preview`'s scope check when opened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectFileSearch {
    pub hits: Vec<PathBuf>,
    /// Some directories, entries, or results could not be inspected or shown.
    pub truncated: bool,
    pub directories_scanned: usize,
    pub entries_scanned: usize,
    pub matched_files: usize,
}

#[derive(Clone, Copy)]
struct SearchLimits {
    depth: usize,
    directories: usize,
    entries: usize,
    entries_per_directory: usize,
    results: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            depth: MAX_SEARCH_DEPTH,
            directories: MAX_SEARCH_DIRECTORIES,
            entries: MAX_SEARCH_ENTRIES,
            entries_per_directory: MAX_SEARCH_ENTRIES_PER_DIRECTORY,
            results: MAX_SEARCH_RESULTS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub relative: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreviewPage {
    pub text: String,
    /// Zero-based first line requested for this page.
    pub start_line: usize,
    /// Zero-based exclusive end of the lines actually returned.
    pub end_line: usize,
    /// Exact only when the entire file was scanned.
    pub total_lines: Option<usize>,
    pub has_more: bool,
    /// Content exceeded a safety limit, or a displayed line was shortened.
    pub truncated: bool,
}

#[derive(Eq, PartialEq)]
struct SortedEntry(FileEntry);

impl Ord for SortedEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare_entries(&self.0, &other.0)
    }
}

impl PartialOrd for SortedEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// One row in the visible, depth-first tree. The workspace root itself is not
/// a row; its direct children have depth zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub relative: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub children_truncated: bool,
}

/// Read-only, lazy workspace tree. It reads the root on creation and reads a
/// child directory only when the user first expands it. Collapse preserves the
/// cached listing; `refresh` reloads the root and currently expanded paths.
pub struct FileExplorer {
    workspace: PathBuf,
    loaded: HashMap<PathBuf, DirectoryListing>,
    directory_limits: HashMap<PathBuf, usize>,
    expanded: HashSet<PathBuf>,
    rows: Vec<FileRow>,
    rows_truncated: bool,
}

impl FileExplorer {
    pub fn new(workspace: &Path) -> io::Result<Self> {
        let workspace = workspace.canonicalize()?;
        let root = list_directory(&workspace, Path::new(""))?;
        let mut explorer = Self {
            workspace,
            loaded: HashMap::from([(PathBuf::new(), root)]),
            directory_limits: HashMap::from([(PathBuf::new(), MAX_DIRECTORY_ENTRIES)]),
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_truncated: false,
        };
        explorer.rebuild_rows();
        Ok(explorer)
    }

    pub fn rows(&self) -> &[FileRow] {
        &self.rows
    }

    /// The folders whose entries are on screen: the root and every expanded one.
    pub fn shown_directories(&self) -> Vec<PathBuf> {
        std::iter::once(self.workspace.clone())
            .chain(self.expanded.iter().map(|relative| self.workspace.join(relative)))
            .collect()
    }

    pub fn rows_truncated(&self) -> bool {
        self.rows_truncated
    }

    pub fn root_truncated(&self) -> bool {
        self.loaded
            .get(Path::new(""))
            .is_some_and(|listing| listing.truncated)
    }

    /// Continue a truncated directory in fixed pages. The root is addressed
    /// with an empty path; other directories must be expanded and visible.
    /// A failed read leaves the current rows and loaded page unchanged.
    pub fn load_more(&mut self, relative: &Path) -> io::Result<bool> {
        if !relative.as_os_str().is_empty()
            && !self
                .rows
                .iter()
                .any(|row| row.is_dir && row.expanded && row.relative == relative)
        {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "directory is not expanded and visible",
            ));
        }
        let Some(current) = self.loaded.get(relative) else {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "directory is not loaded",
            ));
        };
        if !current.truncated {
            return Ok(false);
        }
        let next_limit = self
            .directory_limits
            .get(relative)
            .copied()
            .unwrap_or(MAX_DIRECTORY_ENTRIES)
            .saturating_add(MAX_DIRECTORY_ENTRIES)
            .min(MAX_DIRECTORY_ENTRIES_TOTAL);
        let current_limit = self
            .directory_limits
            .get(relative)
            .copied()
            .unwrap_or(MAX_DIRECTORY_ENTRIES);
        if next_limit == current_limit {
            return Ok(false);
        }
        let listing = list_directory_with_limit(&self.workspace, relative, next_limit)?;
        self.directory_limits
            .insert(relative.to_owned(), next_limit);
        self.loaded.insert(relative.to_owned(), listing);
        self.rebuild_rows();
        Ok(true)
    }

    pub fn directory_truncated(&self, relative: &Path) -> bool {
        self.loaded
            .get(relative)
            .is_some_and(|listing| listing.truncated)
    }

    pub fn directory_can_load_more(&self, relative: &Path) -> bool {
        self.directory_truncated(relative)
            && self
                .directory_limits
                .get(relative)
                .copied()
                .unwrap_or(MAX_DIRECTORY_ENTRIES)
                < MAX_DIRECTORY_ENTRIES_TOTAL
    }

    /// A path must be a currently visible directory row. On first expansion,
    /// its children are read from disk. Returns the new expanded state.
    pub fn toggle(&mut self, relative: &Path) -> io::Result<bool> {
        if !self
            .rows
            .iter()
            .any(|row| row.is_dir && row.relative == relative)
        {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "directory is not a visible row",
            ));
        }
        if self.expanded.remove(relative) {
            self.rebuild_rows();
            return Ok(false);
        }
        if self.expanded.len() >= MAX_EXPANDED_DIRECTORIES {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "too many expanded directories",
            ));
        }
        if !self.loaded.contains_key(relative) {
            let listing = list_directory(&self.workspace, relative)?;
            self.loaded.insert(relative.to_owned(), listing);
            self.directory_limits
                .insert(relative.to_owned(), MAX_DIRECTORY_ENTRIES);
        }
        self.expanded.insert(relative.to_owned());
        self.rebuild_rows();
        Ok(true)
    }

    /// Collapse every folder; the cached listings stay for the next expansion.
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild_rows();
    }

    /// Expand a breadcrumb hierarchy without toggling already-open ancestors.
    pub fn reveal(&mut self, relative: &Path) -> io::Result<()> {
        let mut ancestor = PathBuf::new();
        for part in relative.components() {
            if !matches!(part, Component::Normal(_)) {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "invalid folder path",
                ));
            }
            ancestor.push(part);
            if !self.expanded.contains(&ancestor) {
                self.toggle(&ancestor)?;
            }
        }
        Ok(())
    }

    /// Reload the root and all expanded directories, including descendants of
    /// a temporarily collapsed parent. A removed directory loses its cached
    /// expansion; any other read error leaves the previous tree intact.
    pub fn refresh(&mut self) -> io::Result<()> {
        let root_limit = self
            .directory_limits
            .get(Path::new(""))
            .copied()
            .unwrap_or(MAX_DIRECTORY_ENTRIES);
        let root = list_directory_with_limit(&self.workspace, Path::new(""), root_limit)?;
        let mut loaded = HashMap::from([(PathBuf::new(), root)]);
        let mut expanded = HashSet::new();
        let mut expanded_paths: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        expanded_paths.sort_by_key(|path| path.components().count());
        for child in expanded_paths {
            let limit = self
                .directory_limits
                .get(&child)
                .copied()
                .unwrap_or(MAX_DIRECTORY_ENTRIES);
            match list_directory_with_limit(&self.workspace, &child, limit) {
                Ok(listing) => {
                    expanded.insert(child.clone());
                    loaded.insert(child, listing);
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput) =>
                {
                    // The directory was deleted or replaced with a file.
                }
                Err(error) => return Err(error),
            }
        }
        self.loaded = loaded;
        self.expanded = expanded;
        self.directory_limits
            .retain(|path, _| self.loaded.contains_key(path));
        self.rebuild_rows();
        Ok(())
    }

    /// Switch workspaces atomically, clearing expansion and cached listings.
    /// Returns false when the canonical workspace is unchanged.
    pub fn set_workspace(&mut self, workspace: &Path) -> io::Result<bool> {
        let workspace = workspace.canonicalize()?;
        if workspace == self.workspace {
            return Ok(false);
        }
        *self = Self::new(&workspace)?;
        Ok(true)
    }

    fn rebuild_rows(&mut self) {
        let (rows, truncated) = flatten_rows(&self.loaded, &self.expanded);
        self.rows = rows;
        self.rows_truncated = truncated;
    }
}

fn flatten_rows(
    loaded: &HashMap<PathBuf, DirectoryListing>,
    expanded: &HashSet<PathBuf>,
) -> (Vec<FileRow>, bool) {
    let mut rows = Vec::new();
    let mut pending: Vec<(FileEntry, usize)> = loaded
        .get(Path::new(""))
        .into_iter()
        .flat_map(|listing| {
            listing
                .entries
                .iter()
                .rev()
                .cloned()
                .map(|entry| (entry, 0))
        })
        .collect();
    while let Some((entry, depth)) = pending.pop() {
        if rows.len() >= MAX_VISIBLE_ROWS {
            return (rows, true);
        }
        let is_expanded = entry.is_dir && expanded.contains(&entry.relative);
        let children_truncated = loaded
            .get(&entry.relative)
            .is_some_and(|listing| listing.truncated);
        if is_expanded {
            if let Some(listing) = loaded.get(&entry.relative) {
                pending.extend(
                    listing
                        .entries
                        .iter()
                        .rev()
                        .cloned()
                        .map(|child| (child, depth + 1)),
                );
            }
        }
        rows.push(FileRow {
            relative: entry.relative,
            name: entry.name,
            is_dir: entry.is_dir,
            depth,
            expanded: is_expanded,
            children_truncated,
        });
    }
    (rows, false)
}

fn within_workspace(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "invalid relative path",
        ));
    }
    let canonical_root = root.canonicalize()?;
    let target = canonical_root.join(relative).canonicalize()?;
    if !target.starts_with(&canonical_root) {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "path escapes the workspace",
        ));
    }
    Ok(target)
}

pub fn list_directory(root: &Path, relative: &Path) -> io::Result<DirectoryListing> {
    list_directory_with_limit(root, relative, MAX_DIRECTORY_ENTRIES)
}

fn list_directory_with_limit(
    root: &Path,
    relative: &Path,
    limit: usize,
) -> io::Result<DirectoryListing> {
    let directory = within_workspace(root, relative)?;
    if !directory.is_dir() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "not a directory"));
    }
    let mut entries = BinaryHeap::new();
    let mut truncated = false;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || entry.file_name() == ".git" {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let item = FileEntry {
            relative: relative.join(&name),
            name,
            is_dir: file_type.is_dir(),
        };
        entries.push(SortedEntry(item));
        if entries.len() > limit {
            entries.pop();
            truncated = true;
        }
    }
    let mut entries: Vec<FileEntry> = entries.into_iter().map(|entry| entry.0).collect();
    entries.sort_by(compare_entries);
    Ok(DirectoryListing { entries, truncated })
}

/// Search file names and relative paths throughout a workspace. The scan is
/// deliberately bounded, never reads file contents, and never follows symbolic
/// links. Run it outside the paint loop when used for interactive search.
pub fn search_project_files(workspace: &Path, query: &str) -> io::Result<ProjectFileSearch> {
    search_project_files_with_limits(workspace, query, SearchLimits::default())
}

fn search_project_files_with_limits(
    workspace: &Path,
    query: &str,
    limits: SearchLimits,
) -> io::Result<ProjectFileSearch> {
    let normalized_query = query.trim().replace('\\', "/").to_lowercase();
    let terms: Vec<&str> = normalized_query.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(ProjectFileSearch::default());
    }

    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "workspace is not a directory",
        ));
    }
    let mut result = ProjectFileSearch::default();
    let mut pending = VecDeque::from([(PathBuf::new(), 0_usize)]);
    let mut visited = HashSet::new();
    let mut hits = Vec::new();

    while let Some((relative_dir, depth)) = pending.pop_front() {
        if result.directories_scanned >= limits.directories
            || result.entries_scanned >= limits.entries
        {
            result.truncated = true;
            break;
        }
        let directory = workspace.join(&relative_dir);
        // A directory can change between discovery and traversal. Resolve it
        // again before read_dir and reject anything outside this workspace.
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if relative_dir.as_os_str().is_empty() => return Err(error),
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        let canonical = match directory.canonicalize() {
            Ok(path) if path.starts_with(&workspace) => path,
            Ok(_) | Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if !visited.insert(canonical.clone()) {
            continue;
        }
        let mut reader = match fs::read_dir(&canonical) {
            Ok(reader) => reader,
            Err(error) if relative_dir.as_os_str().is_empty() => return Err(error),
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        result.directories_scanned += 1;

        let allowance = limits
            .entries_per_directory
            .min(limits.entries.saturating_sub(result.entries_scanned));
        let mut entries = Vec::with_capacity(allowance.min(256));
        for entry in reader.by_ref().take(allowance) {
            result.entries_scanned += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    result.truncated = true;
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    result.truncated = true;
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            if file_type.is_dir() && ignored_search_directory(&name.to_string_lossy()) {
                continue;
            }
            entries.push((name, file_type));
        }
        if reader.next().is_some() {
            result.truncated = true;
        }
        entries.sort_by(|(name_a, _), (name_b, _)| {
            name_a
                .to_string_lossy()
                .to_lowercase()
                .cmp(&name_b.to_string_lossy().to_lowercase())
                .then_with(|| name_a.cmp(name_b))
        });

        for (name, file_type) in entries {
            let relative = relative_dir.join(&name);
            if file_type.is_dir() {
                if depth >= limits.depth
                    || pending.len() + result.directories_scanned >= limits.directories
                {
                    result.truncated = true;
                } else {
                    pending.push_back((relative, depth + 1));
                }
            } else if file_type.is_file() {
                let filename = name.to_string_lossy().to_lowercase();
                let path_text = relative.to_string_lossy().replace('\\', "/").to_lowercase();
                let search_text = format!("{filename} {path_text}");
                if terms.iter().all(|term| search_text.contains(term)) {
                    result.matched_files += 1;
                    let rank = if filename == normalized_query {
                        0
                    } else if filename.starts_with(&normalized_query) {
                        1
                    } else if terms.iter().all(|term| filename.contains(term)) {
                        2
                    } else {
                        3
                    };
                    hits.push((rank, path_text, relative));
                }
            }
        }
    }

    hits.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| {
                left.2
                    .components()
                    .count()
                    .cmp(&right.2.components().count())
            })
            .then_with(|| left.1.len().cmp(&right.1.len()))
            .then_with(|| left.1.cmp(&right.1))
    });
    if hits.len() > limits.results {
        result.truncated = true;
    }
    result.hits = hits
        .into_iter()
        .take(limits.results)
        .map(|(_, _, relative)| relative)
        .collect();
    Ok(result)
}

fn ignored_search_directory(name: &str) -> bool {
    [".git", "node_modules", "target"]
        .iter()
        .any(|ignored| name.eq_ignore_ascii_case(ignored))
}

fn compare_entries(left: &FileEntry, right: &FileEntry) -> std::cmp::Ordering {
    right
        .is_dir
        .cmp(&left.is_dir)
        .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        .then_with(|| left.name.cmp(&right.name))
}

#[cfg(test)]
pub fn read_text_preview(root: &Path, relative: &Path) -> io::Result<String> {
    let path = within_workspace(root, relative)?;
    if !path.is_file() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "not a file"));
    }
    if path.metadata()?.len() > MAX_PREVIEW_BYTES {
        return Err(io::Error::new(
            ErrorKind::FileTooLarge,
            "file exceeds the 256 KiB preview limit",
        ));
    }
    // Bound the actual read too: the file can grow after the metadata check.
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PREVIEW_BYTES {
        return Err(io::Error::new(
            ErrorKind::FileTooLarge,
            "file exceeds the 256 KiB preview limit",
        ));
    }
    if bytes.contains(&0) {
        return Err(io::Error::new(ErrorKind::InvalidData, "binary file"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::new(ErrorKind::InvalidData, "not UTF-8 text"))
}

/// Read a bounded page of UTF-8 lines without loading the whole file into the
/// UI. An eight MiB scan ceiling protects against very large files and lines;
/// ordinary files expose an exact line count for pagination.
pub fn read_text_preview_page(
    root: &Path,
    relative: &Path,
    page: usize,
    page_size: usize,
) -> io::Result<PreviewPage> {
    if page_size == 0 || page_size > MAX_PREVIEW_PAGE_LINES {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid preview page size",
        ));
    }
    let start_line = page
        .checked_mul(page_size)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "preview page overflow"))?;
    let requested_end = start_line
        .checked_add(page_size)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "preview page overflow"))?;
    let path = within_workspace(root, relative)?;
    if !path.is_file() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "not a file"));
    }
    let file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file).take(MAX_PREVIEW_SCAN_BYTES + 1);
    let mut page = PreviewPage {
        start_line,
        end_line: start_line,
        ..PreviewPage::default()
    };
    let mut bytes_scanned = 0_u64;
    let mut line_count = 0_usize;
    let mut line = Vec::new();
    let mut complete = false;
    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if bytes == 0 {
            complete = true;
            break;
        }
        bytes_scanned += bytes as u64;
        if bytes_scanned > MAX_PREVIEW_SCAN_BYTES || line_count >= MAX_PREVIEW_SCAN_LINES {
            page.truncated = true;
            break;
        }
        if line.contains(&0) {
            return Err(io::Error::new(ErrorKind::InvalidData, "binary file"));
        }
        let decoded = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "not UTF-8 text"))?;
        if (start_line..requested_end).contains(&line_count) {
            if decoded.chars().count() > MAX_PREVIEW_LINE_CHARS {
                page.text
                    .extend(decoded.chars().take(MAX_PREVIEW_LINE_CHARS));
                page.text.push_str("…\n");
                page.truncated = true;
            } else {
                page.text.push_str(decoded);
            }
            page.end_line += 1;
        }
        line_count += 1;
        // For files above the scan budget, an exact total is unavailable.
        // Stop once the next page is known to exist.
        if file_len > MAX_PREVIEW_SCAN_BYTES && line_count > requested_end {
            break;
        }
    }
    if complete {
        page.total_lines = Some(line_count);
    }
    page.has_more = line_count > requested_end;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn collapse_all_leaves_only_the_root_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src/deep")).unwrap();
        fs::write(temp.path().join("src/deep/a.rs"), "x").unwrap();
        let mut explorer = FileExplorer::new(temp.path()).unwrap();
        explorer.reveal(Path::new("src/deep")).unwrap();
        assert_eq!(explorer.rows().len(), 3);
        explorer.collapse_all();
        assert_eq!(explorer.rows().len(), 1);
        assert!(!explorer.rows()[0].expanded);
        // The nested folder does not come back expanded with its parent.
        explorer.toggle(Path::new("src")).unwrap();
        assert_eq!(explorer.rows().len(), 2);
    }

    #[test]
    fn lists_directories_first_and_previews_workspace_text() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("README.md"), "hola\n").unwrap();
        let listing = list_directory(temp.path(), Path::new("")).unwrap();
        assert_eq!(listing.entries[0].name, "src");
        assert_eq!(listing.entries[1].name, "README.md");
        assert_eq!(
            read_text_preview(temp.path(), Path::new("README.md")).unwrap(),
            "hola\n"
        );
    }

    #[test]
    fn truncated_directory_loads_next_page_and_refresh_keeps_the_page() {
        let temp = tempfile::tempdir().unwrap();
        for number in 0..(MAX_DIRECTORY_ENTRIES + 5) {
            fs::write(temp.path().join(format!("file-{number:04}.txt")), "x").unwrap();
        }
        let mut explorer = FileExplorer::new(temp.path()).unwrap();
        assert_eq!(explorer.rows().len(), MAX_DIRECTORY_ENTRIES);
        assert!(explorer.root_truncated());
        assert!(explorer.directory_can_load_more(Path::new("")));
        assert!(explorer.load_more(Path::new("")).unwrap());
        assert_eq!(explorer.rows().len(), MAX_DIRECTORY_ENTRIES + 5);
        assert_eq!(explorer.rows()[0].name, "file-0000.txt");
        assert_eq!(explorer.rows().last().unwrap().name, "file-0204.txt");
        assert!(!explorer.root_truncated());
        assert!(!explorer.load_more(Path::new("")).unwrap());
        explorer.refresh().unwrap();
        assert_eq!(explorer.rows().len(), MAX_DIRECTORY_ENTRIES + 5);
    }

    #[test]
    fn nested_page_requires_a_visible_expanded_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        for number in 0..(MAX_DIRECTORY_ENTRIES + 1) {
            fs::write(temp.path().join("src").join(format!("{number:04}.rs")), "x").unwrap();
        }
        let mut explorer = FileExplorer::new(temp.path()).unwrap();
        assert_eq!(
            explorer.load_more(Path::new("src")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        explorer.toggle(Path::new("src")).unwrap();
        assert_eq!(explorer.rows().len(), MAX_DIRECTORY_ENTRIES + 1);
        assert!(explorer.directory_can_load_more(Path::new("src")));
        explorer.load_more(Path::new("src")).unwrap();
        assert_eq!(explorer.rows().len(), MAX_DIRECTORY_ENTRIES + 2);
        explorer.toggle(Path::new("src")).unwrap();
        assert_eq!(
            explorer.load_more(Path::new("src")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        assert_eq!(explorer.rows().len(), 1);
    }

    #[test]
    fn rejects_parent_paths_and_large_or_binary_previews() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            list_directory(temp.path(), Path::new("../outside"))
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
        fs::write(temp.path().join("binary"), [0, 1, 2]).unwrap();
        assert_eq!(
            read_text_preview(temp.path(), Path::new("binary"))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
        fs::write(
            temp.path().join("large"),
            vec![b'x'; MAX_PREVIEW_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            read_text_preview(temp.path(), Path::new("large"))
                .unwrap_err()
                .kind(),
            ErrorKind::FileTooLarge
        );
    }

    #[test]
    fn paged_preview_reads_large_text_without_losing_line_numbers() {
        let temp = tempfile::tempdir().unwrap();
        let content = (0..1_000)
            .map(|line| format!("{line:04} {}\n", "x".repeat(300)))
            .collect::<String>();
        assert!(content.len() as u64 > MAX_PREVIEW_BYTES);
        fs::write(temp.path().join("large.txt"), &content).unwrap();
        let first = read_text_preview_page(temp.path(), Path::new("large.txt"), 0, 100).unwrap();
        assert_eq!((first.start_line, first.end_line), (0, 100));
        assert_eq!(first.total_lines, Some(1_000));
        assert!(first.has_more);
        assert!(first.text.starts_with("0000 "));
        let last = read_text_preview_page(temp.path(), Path::new("large.txt"), 9, 100).unwrap();
        assert_eq!((last.start_line, last.end_line), (900, 1_000));
        assert_eq!(last.total_lines, Some(1_000));
        assert!(!last.has_more);
        assert!(last.text.ends_with(&format!("0999 {}\n", "x".repeat(300))));
    }

    #[test]
    fn paged_preview_rejects_invalid_scope_and_marks_long_lines() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("long.txt"),
            "a".repeat(MAX_PREVIEW_LINE_CHARS + 5),
        )
        .unwrap();
        let page = read_text_preview_page(temp.path(), Path::new("long.txt"), 0, 1).unwrap();
        assert!(page.truncated);
        assert_eq!(page.total_lines, Some(1));
        assert!(page.text.ends_with("…\n"));
        assert_eq!(
            read_text_preview_page(temp.path(), Path::new("../long.txt"), 0, 1)
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            read_text_preview_page(temp.path(), Path::new("long.txt"), 0, 0)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        fs::write(temp.path().join("binary"), [0_u8, 1, 2]).unwrap();
        assert_eq!(
            read_text_preview_page(temp.path(), Path::new("binary"), 0, 1)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn searches_relative_paths_with_ranked_file_results() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src/components")).unwrap();
        fs::write(temp.path().join("foo.rs"), "root").unwrap();
        fs::write(temp.path().join("src/foo.rs"), "source").unwrap();
        fs::write(temp.path().join("src/components/foobar.rs"), "component").unwrap();
        fs::write(temp.path().join("src/components/other.txt"), "other").unwrap();

        let by_name = search_project_files(temp.path(), "foo").unwrap();
        assert_eq!(
            by_name.hits,
            vec![
                PathBuf::from("foo.rs"),
                PathBuf::from("src/foo.rs"),
                PathBuf::from("src/components/foobar.rs"),
            ]
        );
        assert!(!by_name.truncated);
        assert_eq!(by_name.matched_files, 3);

        let by_path = search_project_files(temp.path(), "src\\components foo").unwrap();
        assert_eq!(
            by_path.hits,
            vec![PathBuf::from("src/components/foobar.rs")]
        );
        assert!(search_project_files(temp.path(), "  ")
            .unwrap()
            .hits
            .is_empty());
    }

    #[test]
    fn skips_generated_directories_and_symbolic_links() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        for ignored in [".git", "node_modules", "target"] {
            fs::create_dir(temp.path().join(ignored)).unwrap();
            fs::write(temp.path().join(ignored).join("secret-match.txt"), "x").unwrap();
        }
        fs::write(outside.path().join("secret-match.txt"), "x").unwrap();
        fs::write(temp.path().join("visible-match.txt"), "x").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), temp.path().join("linked"))
            .expect("create directory symlink");
        // Windows without Developer Mode or the privilege cannot create a
        // symlink; the ignored-directory assertions still run in that case.
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_dir(outside.path(), temp.path().join("linked"));

        let result = search_project_files(temp.path(), "match").unwrap();
        assert_eq!(result.hits, vec![PathBuf::from("visible-match.txt")]);
        assert!(!result.truncated);
    }

    #[test]
    fn search_reports_each_limit_without_unbounded_traversal() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("nested/needle.txt"), "x").unwrap();
        for number in 0..4 {
            fs::write(temp.path().join(format!("needle-{number}.txt")), "x").unwrap();
        }
        let tiny = SearchLimits {
            depth: 0,
            directories: 1,
            entries: 2,
            entries_per_directory: 2,
            results: 1,
        };
        let result = search_project_files_with_limits(temp.path(), "needle", tiny).unwrap();
        assert!(result.truncated);
        assert_eq!(result.directories_scanned, 1);
        assert_eq!(result.entries_scanned, 2);
        assert!(result.hits.len() <= 1);

        let only_result_limit = SearchLimits {
            depth: 2,
            directories: 10,
            entries: 20,
            entries_per_directory: 20,
            results: 1,
        };
        let result =
            search_project_files_with_limits(temp.path(), "needle", only_result_limit).unwrap();
        assert_eq!(result.matched_files, 5);
        assert_eq!(result.hits.len(), 1);
        assert!(result.truncated);
    }
}
