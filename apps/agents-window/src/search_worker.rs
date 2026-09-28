//! Background search for the command center. Session logs and project trees
//! can be large, so neither scan belongs in the UI paint loop.

use crate::files;
use openide_core::{SessionDiagnostic, SessionListKind, SessionQuery, SessionSort, SessionStore};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SendError, Sender, TryRecvError};
use std::thread;

pub const MAX_SESSION_HITS: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub workspace: PathBuf,
    /// Search text after the command center has removed its scope prefix.
    /// An empty string returns recent sessions and skips the file scan.
    pub query: String,
    /// Monotonically increasing revision owned by the caller. Increase this
    /// whenever the query or workspace changes.
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHit {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub workspace: PathBuf,
    /// Excerpts from user-visible prompt and response text only.
    pub snippets: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResponse {
    pub workspace: PathBuf,
    pub query: String,
    pub generation: u64,
    pub sessions: Vec<SessionHit>,
    pub session_total: usize,
    pub session_has_more: bool,
    pub session_diagnostics: Vec<SessionDiagnostic>,
    pub session_error: Option<String>,
    /// Workspace-relative paths. Validate each path again when opening it.
    pub files: Vec<PathBuf>,
    pub file_matches: usize,
    pub files_truncated: bool,
    pub file_error: Option<String>,
}

impl SearchResponse {
    /// A response is safe to apply only to the exact view that requested it.
    pub fn matches(&self, generation: u64, workspace: &Path) -> bool {
        self.generation == generation && self.workspace == workspace
    }
}

/// A single serial worker. Pending keystroke requests are coalesced before
/// each scan. An already running scan cannot be interrupted; its obsolete
/// response is discarded by `poll_latest` on the UI thread.
pub struct SearchWorker {
    request_tx: Sender<SearchRequest>,
    result_rx: Receiver<SearchResponse>,
}

impl SearchWorker {
    pub fn spawn(session_root: PathBuf) -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-command-search".to_owned())
            .spawn(move || run_worker(session_root, request_rx, result_tx))?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn submit(&self, request: SearchRequest) -> Result<(), SendError<SearchRequest>> {
        self.request_tx.send(request)
    }

    /// Drain completed scans, retaining only a result for the current view.
    /// Call this from the UI update loop without blocking.
    pub fn poll_latest(&self, generation: u64, workspace: &Path) -> Option<SearchResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.matches(generation, workspace) {
                latest = Some(response);
            }
        }
        latest
    }
}

fn run_worker(
    session_root: PathBuf,
    request_rx: Receiver<SearchRequest>,
    result_tx: Sender<SearchResponse>,
) {
    while let Ok(first) = request_rx.recv() {
        let request = latest_pending(first, &request_rx);
        let response = execute(&session_root, request);
        if result_tx.send(response).is_err() {
            return;
        }
    }
}

fn latest_pending(
    mut request: SearchRequest,
    request_rx: &Receiver<SearchRequest>,
) -> SearchRequest {
    loop {
        match request_rx.try_recv() {
            Ok(next) if next.generation >= request.generation => request = next,
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return request,
        }
    }
}

fn execute(session_root: &Path, request: SearchRequest) -> SearchResponse {
    let SearchRequest {
        workspace,
        query,
        generation,
    } = request;
    let mut response = SearchResponse {
        workspace: workspace.clone(),
        query: query.clone(),
        generation,
        sessions: Vec::new(),
        session_total: 0,
        session_has_more: false,
        session_diagnostics: Vec::new(),
        session_error: None,
        files: Vec::new(),
        file_matches: 0,
        files_truncated: false,
        file_error: None,
    };

    match SessionStore::open(session_root).and_then(|store| {
        store.query_sessions(&SessionQuery {
            // ZCode's task search spans projects; file search below remains
            // scoped to the workspace currently open in the window.
            workspaces: Vec::new(),
            kind: SessionListKind::All,
            sort: SessionSort::Updated,
            search: (!query.trim().is_empty()).then(|| query.clone()),
            offset: 0,
            limit: MAX_SESSION_HITS,
        })
    }) {
        Ok(found) => {
            response.session_total = found.total;
            response.session_has_more = found.has_more;
            response.session_diagnostics = found.diagnostics;
            response.sessions = found
                .items
                .into_iter()
                .map(|item| SessionHit {
                    id: item.session.id,
                    title: item.title,
                    backend: item.session.backend,
                    workspace: item.session.workspace,
                    snippets: item.search_snippets,
                    pinned: item.session.pinned,
                    archived: item.session.archived,
                })
                .collect();
        }
        Err(error) => response.session_error = Some(error.to_string()),
    }

    if !query.trim().is_empty() {
        match files::search_project_files(&workspace, &query) {
            Ok(found) => {
                response.files = found.hits;
                response.file_matches = found.matched_files;
                response.files_truncated = found.truncated;
            }
            Err(error) => response.file_error = Some(error.to_string()),
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn request(workspace: &Path, query: &str, generation: u64) -> SearchRequest {
        SearchRequest {
            workspace: workspace.to_path_buf(),
            query: query.to_owned(),
            generation,
        }
    }

    #[test]
    fn coalesces_pending_keystrokes_to_newest_generation() {
        let workspace = Path::new("/workspace");
        let (tx, rx) = mpsc::channel();
        tx.send(request(workspace, "old", 2)).unwrap();
        tx.send(request(workspace, "new", 4)).unwrap();
        tx.send(request(workspace, "late old", 3)).unwrap();
        let latest = latest_pending(request(workspace, "first", 1), &rx);
        assert_eq!(latest.generation, 4);
        assert_eq!(latest.query, "new");
    }

    #[test]
    fn polling_discards_stale_generations_and_other_workspaces() {
        let workspace = Path::new("/workspace");
        let (request_tx, _) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = SearchWorker {
            request_tx,
            result_rx,
        };
        for (generation, path) in [
            (1, Path::new("/workspace")),
            (2, Path::new("/other")),
            (2, Path::new("/workspace")),
        ] {
            result_tx
                .send(SearchResponse {
                    workspace: path.to_path_buf(),
                    query: String::new(),
                    generation,
                    sessions: Vec::new(),
                    session_total: 0,
                    session_has_more: false,
                    session_diagnostics: Vec::new(),
                    session_error: None,
                    files: Vec::new(),
                    file_matches: 0,
                    files_truncated: false,
                    file_error: None,
                })
                .unwrap();
        }
        assert_eq!(worker.poll_latest(2, workspace).unwrap().generation, 2);
        assert!(worker.poll_latest(2, workspace).is_none());
    }

    #[test]
    fn worker_searches_sessions_and_project_files_off_thread() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        let other_workspace = temp.path().join("other-project");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&other_workspace).unwrap();
        fs::write(workspace.join("needle.rs"), "fn main() {}").unwrap();
        fs::write(other_workspace.join("other-needle.rs"), "fn main() {}").unwrap();
        let session_root = temp.path().join("sessions");
        let store = SessionStore::open(session_root.clone()).unwrap();
        let session = store.create(&workspace, "codex").unwrap();
        store.rename(&session.id, Some("Needle task")).unwrap();
        let other_session = store.create(&other_workspace, "native").unwrap();
        store
            .rename(&other_session.id, Some("Other needle task"))
            .unwrap();

        let worker = SearchWorker::spawn(session_root).unwrap();
        worker.submit(request(&workspace, "needle", 7)).unwrap();
        let found = worker
            .result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(found.generation, 7);
        assert_eq!(found.sessions.len(), 2);
        assert!(found.sessions.iter().any(|hit| {
            hit.id == session.id
                && hit.title == "Needle task"
                && hit.backend == "codex"
                && hit.workspace == workspace
        }));
        assert!(found.sessions.iter().any(|hit| {
            hit.id == other_session.id
                && hit.title == "Other needle task"
                && hit.workspace == other_workspace
        }));
        assert_eq!(found.files, vec![PathBuf::from("needle.rs")]);
        assert_eq!(found.file_matches, 1);
        assert!(found.session_error.is_none());
        assert!(found.file_error.is_none());
    }
}
