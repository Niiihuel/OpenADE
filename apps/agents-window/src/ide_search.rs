//! Bounded, cancellable project content search. Files are read only by the
//! worker; the view owns a generation so old projects and queries cannot leak
//! results or navigation into the current workspace.
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub const MAX_HITS: usize = 500;
pub const MAX_QUERY_CHARS: usize = 512;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 40_000;
const MAX_SNIPPET_CHARS: usize = 180;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// One-based line for display; character is a zero-based UTF-16 column.
    pub line: u32,
    pub character: u32,
    pub snippet: String,
}

#[derive(Clone, Debug, Default)]
pub struct IdeSearchState {
    pub workspace: String,
    pub query: String,
    pub case_sensitive: bool,
    pub generation: u64,
    pub running: bool,
    pub searched: bool,
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
    pub skipped_files: usize,
    pub error: Option<String>,
    /// Only navigation originated by this search, while its document loads.
    pub pending_navigation: Option<SearchHit>,
}

impl IdeSearchState {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.running = false;
        self.searched = false;
        self.hits.clear();
        self.truncated = false;
        self.skipped_files = 0;
        self.error = None;
    }

    pub fn change_workspace(&mut self, workspace: &str) -> bool {
        if self.workspace == workspace {
            return false;
        }
        self.invalidate();
        self.workspace = workspace.into();
        self.query.clear();
        self.pending_navigation = None;
        true
    }

    pub fn request(&mut self) -> SearchRequest {
        self.invalidate();
        self.running = true;
        SearchRequest {
            workspace: PathBuf::from(&self.workspace),
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            generation: self.generation,
        }
    }

    pub fn apply(&mut self, result: SearchResponse) -> bool {
        if result.request.generation != self.generation
            || result.request.workspace != Path::new(&self.workspace)
            || result.request.query != self.query
            || result.request.case_sensitive != self.case_sensitive
        {
            return false;
        }
        self.running = false;
        self.searched = true;
        self.hits = result.hits;
        self.truncated = result.truncated;
        self.skipped_files = result.skipped_files;
        self.error = result.error;
        true
    }

    pub fn result_action(&self, index: usize) -> String {
        format!("ide_search_result:{}:{index}", self.generation)
    }

    pub fn navigation(&self, action: &str, workspace: &str) -> Option<SearchHit> {
        let (generation, index) = action.strip_prefix("ide_search_result:")?.split_once(':')?;
        if self.workspace != workspace
            || self.running
            || generation.parse::<u64>().ok()? != self.generation
        {
            return None;
        }
        let hit = self.hits.get(index.parse::<usize>().ok()?)?;
        if hit.path.as_os_str().is_empty()
            || hit
                .path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return None;
        }
        Some(hit.clone())
    }
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub workspace: PathBuf,
    pub query: String,
    pub case_sensitive: bool,
    pub generation: u64,
}

#[derive(Debug)]
pub struct SearchResponse {
    pub request: SearchRequest,
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
    pub skipped_files: usize,
    pub error: Option<String>,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Option<SearchRequest>>,
    result: Mutex<Option<SearchResponse>>,
    wake: Condvar,
    generation: AtomicU64,
    stopping: AtomicBool,
}

pub struct IdeSearchWorker {
    shared: Arc<Shared>,
}

impl IdeSearchWorker {
    pub fn spawn() -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("openide-project-search".into())
            .spawn(move || loop {
                let request = {
                    let Ok(mut pending) = worker.pending.lock() else {
                        return;
                    };
                    while pending.is_none() && !worker.stopping.load(Ordering::Acquire) {
                        let Ok(next) = worker.wake.wait(pending) else {
                            return;
                        };
                        pending = next;
                    }
                    if worker.stopping.load(Ordering::Acquire) {
                        return;
                    }
                    pending.take().unwrap()
                };
                let generation = request.generation;
                let cancelled = || {
                    worker.stopping.load(Ordering::Acquire)
                        || worker.generation.load(Ordering::Acquire) != generation
                };
                if let Some(result) = execute(request, cancelled) {
                    if !cancelled() {
                        let Ok(mut ready) = worker.result.lock() else {
                            return;
                        };
                        *ready = Some(result);
                    }
                }
            })?;
        Ok(Self { shared })
    }

    pub fn cancel(&self, generation: u64) {
        self.shared.generation.store(generation, Ordering::Release);
        if let Ok(mut pending) = self.shared.pending.lock() {
            *pending = None;
        }
        if let Ok(mut result) = self.shared.result.lock() {
            *result = None;
        }
    }

    pub fn submit(&self, request: SearchRequest) {
        self.cancel(request.generation);
        if let Ok(mut pending) = self.shared.pending.lock() {
            *pending = Some(request);
            self.shared.wake.notify_one();
        }
    }

    pub fn poll(&self) -> Option<SearchResponse> {
        self.shared.result.lock().ok()?.take()
    }
}

impl Drop for IdeSearchWorker {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        // Hold the same mutex as the wait predicate to avoid a lost wakeup.
        if let Ok(_pending) = self.shared.pending.lock() {
            self.shared.wake.notify_one();
        }
    }
}

fn execute(request: SearchRequest, cancelled: impl Fn() -> bool) -> Option<SearchResponse> {
    let mut result = SearchResponse {
        request,
        hits: Vec::new(),
        truncated: false,
        skipped_files: 0,
        error: None,
    };
    let root = match result.request.workspace.canonicalize() {
        Ok(path) if path.is_dir() => path,
        _ => {
            result.error = Some("No se pudo abrir el proyecto para buscar.".into());
            return Some(result);
        }
    };
    if result.request.query.is_empty() || result.request.query.chars().count() > MAX_QUERY_CHARS {
        result.error = Some("Escribí un texto de hasta 512 caracteres.".into());
        return Some(result);
    }
    let needle = if result.request.case_sensitive {
        result.request.query.clone()
    } else {
        result.request.query.to_lowercase()
    };
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(32))
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | "node_modules")
                )
        })
        .build();
    let mut bytes_read = 0_u64;
    for (index, entry) in walker.enumerate() {
        if cancelled() {
            return None;
        }
        if index >= MAX_ENTRIES || bytes_read >= MAX_SCAN_BYTES {
            result.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.skipped_files += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        // Recheck scope after traversal, including entries replaced by links.
        let path = entry.path();
        if std::fs::symlink_metadata(path).map_or(true, |m| m.file_type().is_symlink()) {
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(path) if path.starts_with(&root) => path,
            _ => {
                result.skipped_files += 1;
                continue;
            }
        };
        let relative = match path.strip_prefix(&root) {
            Ok(path) => path.to_path_buf(),
            Err(_) => continue,
        };
        let file = match File::open(canonical) {
            Ok(file) => file,
            Err(_) => {
                result.skipped_files += 1;
                continue;
            }
        };
        let remaining = MAX_SCAN_BYTES.saturating_sub(bytes_read);
        let limit = MAX_FILE_BYTES.min(remaining);
        if file.metadata().map_or(true, |m| m.len() > limit) {
            result.skipped_files += 1;
            continue;
        }
        let mut bytes = Vec::new();
        if file.take(limit + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > limit {
            result.skipped_files += 1;
            continue;
        }
        bytes_read += bytes.len() as u64;
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (line_index, line) in protocol_lines(text).enumerate() {
            if cancelled() {
                return None;
            }
            let Some(byte) = match_offset(line, &needle, result.request.case_sensitive) else {
                continue;
            };
            if result.hits.len() == MAX_HITS {
                result.truncated = true;
                return Some(result);
            }
            result.hits.push(SearchHit {
                path: relative.clone(),
                line: (line_index + 1) as u32,
                character: line[..byte].encode_utf16().count() as u32,
                snippet: excerpt(line, byte),
            });
        }
    }
    Some(result)
}

fn protocol_lines(text: &str) -> impl Iterator<Item = &str> {
    // Match the editor's LSP coordinate conversion: CR, LF and CRLF each end
    // one line, while other Unicode separators stay within the same line.
    let mut after_cr = false;
    text.split_inclusive(['\r', '\n']).filter_map(move |part| {
        let crlf_tail = after_cr && part == "\n";
        after_cr = part.ends_with('\r');
        (!crlf_tail).then(|| part.trim_end_matches(['\r', '\n']))
    })
}

fn match_offset(line: &str, needle: &str, case_sensitive: bool) -> Option<usize> {
    if case_sensitive {
        return line.find(needle);
    }
    let offset = line.to_lowercase().find(needle)?;
    // Unicode lowercasing can change byte lengths (e.g. İ → i + combining dot).
    let mut lowered = 0;
    for (original, character) in line.char_indices() {
        let next = lowered + character.to_lowercase().map(char::len_utf8).sum::<usize>();
        if offset < next {
            return Some(original);
        }
        lowered = next;
    }
    None
}

fn excerpt(line: &str, byte: usize) -> String {
    let preceding = line[..byte].chars().count();
    let start = preceding.saturating_sub(40);
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.extend(line.chars().skip(start).take(MAX_SNIPPET_CHARS).map(|c| {
        if c.is_control() {
            ' '
        } else {
            c
        }
    }));
    if line
        .chars()
        .skip(start + MAX_SNIPPET_CHARS)
        .next()
        .is_some()
    {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(root: &Path, query: &str) -> SearchRequest {
        SearchRequest {
            workspace: root.into(),
            query: query.into(),
            case_sensitive: true,
            generation: 1,
        }
    }

    #[test]
    fn ide_search_respects_gitignore_generated_folders_and_symlinks() {
        let root = tempfile::tempdir().unwrap();
        for dir in ["src", "nested", ".git", "target", "node_modules", "ignored"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        std::fs::write(root.path().join(".gitignore"), "ignored/\n*.log\n").unwrap();
        std::fs::write(root.path().join(".ignore"), "ignore-rule.txt\n").unwrap();
        std::fs::write(root.path().join("nested/.gitignore"), "secret.txt\n").unwrap();
        for path in [
            "src/a.rs",
            "nested/secret.txt",
            ".git/data",
            "target/data",
            "node_modules/data",
            "ignored/data",
            "file.log",
            "ignore-rule.txt",
        ] {
            std::fs::write(root.path().join(path), "needle\n").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("src/a.rs"), root.path().join("linked.rs"))
            .unwrap();
        let result = execute(request(root.path(), "needle"), || false).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, PathBuf::from("src/a.rs"));
        assert_eq!(result.hits[0].line, 1);
    }

    #[test]
    fn ide_search_bounds_hits_files_and_unicode_excerpts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("large.txt"),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        std::fs::write(
            root.path().join("matches.txt"),
            "needle\n".repeat(MAX_HITS + 3),
        )
        .unwrap();
        let result = execute(request(root.path(), "needle"), || false).unwrap();
        assert_eq!(result.hits.len(), MAX_HITS);
        assert!(result.truncated);
        assert_eq!(result.skipped_files, 1);
        let snippet = excerpt(&format!("{}needle", "界".repeat(500)), 1500);
        assert!(snippet.starts_with('…'));
        assert!(snippet.contains("needle"));
        assert!(snippet.chars().count() <= MAX_SNIPPET_CHARS + 2);
    }

    #[test]
    fn ide_search_matches_literal_case_and_maps_utf16_columns() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "no match\n😀 İ NÉEDLE [a.*]\n").unwrap();
        let mut query = request(root.path(), "néedle");
        assert!(execute(query.clone(), || false).unwrap().hits.is_empty());
        query.case_sensitive = false;
        let result = execute(query, || false).unwrap();
        assert_eq!(result.hits[0].line, 2);
        assert_eq!(result.hits[0].character, 5);
        let result = execute(request(root.path(), "[a.*]"), || false).unwrap();
        assert_eq!(result.hits.len(), 1);
        assert!(execute(request(root.path(), "needle"), || true).is_none());
    }

    #[test]
    fn ide_search_line_numbers_match_editor_cr_lf_and_crlf_coordinates() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("a.txt"),
            "first\r\nsecond\r😀 needle\nlast",
        )
        .unwrap();
        let result = execute(request(root.path(), "needle"), || false).unwrap();
        assert_eq!(result.hits[0].line, 3);
        assert_eq!(result.hits[0].character, 3);
    }

    #[test]
    fn ide_search_rejects_stale_responses_and_result_navigation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "needle\n").unwrap();
        let mut state = IdeSearchState::default();
        state.change_workspace(root.path().to_str().unwrap());
        state.query = "needle".into();
        let requested = state.request();
        let response = execute(requested.clone(), || false).unwrap();
        assert!(state.apply(response));
        let action = state.result_action(0);
        assert!(state.navigation(&action, &state.workspace).is_some());
        assert!(state.navigation(&action, "/another-project").is_none());
        state.invalidate();
        assert!(state.navigation(&action, &state.workspace).is_none());
        assert!(!state.apply(execute(requested, || false).unwrap()));
    }

    #[test]
    fn ide_search_worker_delivers_latest_request_off_thread() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "needle\n").unwrap();
        let worker = IdeSearchWorker::spawn().unwrap();
        worker.submit(request(root.path(), "old"));
        let mut next = request(root.path(), "needle");
        next.generation = 2;
        worker.submit(next);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = worker.poll() {
                if result.request.generation == 2 {
                    assert_eq!(result.hits.len(), 1);
                    break;
                }
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}
