//! Read-only file changes reconstructed from this session's confirmed tools.
//!
//! Context checkpoints do not contain a file baseline. A separate bounded
//! snapshot is captured before the first Build worker executes effects; old chats deliberately
//! remain unavailable. Projection never compares the whole working tree with
//! HEAD, executes a tool, or writes Git's index/objects/refs.

use crate::child_process::command_no_window;
use crate::git_review::{ReviewFile, ReviewLine, ReviewLineKind};
use base64::{engine::general_purpose::STANDARD, Engine};
use openide_core::{AgentEvent, AgentEventKind, ToolCall};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "session_changes_tool_snapshots.rs"]
mod tool_snapshots;
pub(crate) use tool_snapshots::with_file_snapshots;

/// The file captures of calls a rewind removed, taken out of the store.
pub(crate) fn take_tool_snapshots(
    storage_root: &Path,
    session_id: &str,
    calls: &[&str],
) -> io::Result<Vec<(String, Vec<u8>)>> {
    tool_snapshots::take_calls(storage_root, session_id, calls)
}

/// Captures returned to the store when a rewind is undone.
pub(crate) fn restore_tool_snapshots(
    storage_root: &Path,
    files: &[(String, Vec<u8>)],
) -> io::Result<()> {
    tool_snapshots::restore_calls(storage_root, files)
}
#[path = "session_changes_revert.rs"]
mod revert;
pub(crate) use revert::{
    reapply_reverted_files, revert_files, revert_preflight, OpaqueEffects, RevertFile,
    RevertPreflight, RevertSkip,
};
#[path = "session_file_review.rs"]
mod file_review;
pub(crate) use file_review::{
    decide_file_review, load_file_review, load_latest_file_review, FileReview, FileReviewDecision,
    FileReviewDecisionTarget,
};

const VERSION: u16 = 1;
const MAX_BASELINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_BASELINE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_VISITS: usize = 8192;
const MAX_DIRECTORY_INVENTORY_BYTES: usize = 512 * 1024;
const MAX_PATH_BYTES: usize = 1024;
const MAX_EVENTS: usize = 20_000;
const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PENDING_TOOLS: usize = 256;
const MAX_CHANGED_FILES: usize = 128;
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_PATCH_FILES: usize = 32;
const MAX_PATCH_HUNKS: usize = 128;
const MAX_DIFF_BYTES: usize = 128 * 1024;
const MAX_ALL_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIFF_LINES: usize = 2000;
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
const PROJECTION_BUDGET: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaselineInfo {
    pub captured_at_ms: u64,
    pub branch: String,
    pub head: Option<String>,
    pub captured_files: usize,
    pub omitted_files: usize,
    pub inventory_complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BaselineCapture {
    Created(BaselineInfo),
    Existing(BaselineInfo),
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChangesAvailability {
    Complete,
    Limited,
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChangeTotals {
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    /// False if an unavailable/binary/truncated file prevents a complete count.
    pub exact: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFileChange {
    pub file: ReviewFile,
    pub last_tool_sequence: u64,
    /// Some means this path has no trustworthy baseline/replay or textual diff.
    pub unavailable: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionChanges {
    pub session_id: String,
    pub workspace: PathBuf,
    pub baseline: Option<BaselineInfo>,
    pub availability: ChangesAvailability,
    pub totals: ChangeTotals,
    pub files: Vec<SessionFileChange>,
    /// Bounded, deduplicated explanations of incomplete coverage.
    pub coverage_notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SessionChangesRequest {
    pub storage_root: PathBuf,
    pub workspace: PathBuf,
    pub session_id: String,
    pub generation: u64,
    /// Share the host's already loaded journal, rather than copying every delta.
    /// None reads the journal in the background with its own byte/event caps.
    pub events: Option<Arc<Vec<AgentEvent>>>,
    /// Only what the last turn changed, against the files as it found them.
    pub last_turn: bool,
}

#[derive(Debug)]
pub struct SessionChangesResponse {
    pub workspace: PathBuf,
    pub session_id: String,
    pub generation: u64,
    pub result: Result<SessionChanges, String>,
}

impl SessionChangesResponse {
    pub fn matches(&self, generation: u64, session_id: &str, workspace: &Path) -> bool {
        self.generation == generation
            && self.session_id == session_id
            && self.workspace == workspace
    }
}

/// The host represents loading from submit until a matching response arrives.
/// One pending request and one completed result are retained; newer work
/// replaces either slot. Git and baseline I/O happen outside the mailbox lock.
pub struct SessionChangesWorker {
    mailbox: Arc<Mailbox>,
}

#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    wake: Condvar,
}

#[derive(Default)]
struct MailboxState {
    pending: Option<SessionChangesRequest>,
    ready: Option<SessionChangesResponse>,
    stopped: bool,
}

impl SessionChangesWorker {
    pub fn spawn() -> io::Result<Self> {
        let mailbox = Arc::new(Mailbox::default());
        let background = mailbox.clone();
        thread::Builder::new()
            .name("openide-session-changes".into())
            .spawn(move || loop {
                let request = {
                    let Ok(mut state) = background.state.lock() else {
                        return;
                    };
                    while state.pending.is_none() && !state.stopped {
                        let Ok(next) = background.wake.wait(state) else {
                            return;
                        };
                        state = next;
                    }
                    if state.stopped {
                        return;
                    }
                    state.pending.take().expect("checked pending request")
                };
                let result = match &request.events {
                    Some(events) => load_changes(
                        &request.storage_root,
                        &request.session_id,
                        &request.workspace,
                        events,
                        request.last_turn,
                    ),
                    None => load_stored_changes(
                        &request.storage_root,
                        &request.session_id,
                        &request.workspace,
                        request.last_turn,
                    ),
                }
                .map_err(|error| error.to_string());
                let Ok(mut state) = background.state.lock() else {
                    return;
                };
                if state.stopped {
                    return;
                }
                state.ready = Some(SessionChangesResponse {
                    workspace: request.workspace,
                    session_id: request.session_id,
                    generation: request.generation,
                    result,
                });
            })?;
        Ok(Self { mailbox })
    }

    pub fn submit(&self, request: SessionChangesRequest) -> io::Result<()> {
        let mut state = self
            .mailbox
            .state
            .lock()
            .map_err(|_| invalid("Session changes lock failed"))?;
        state.pending = Some(request);
        state.ready = None;
        self.mailbox.wake.notify_one();
        Ok(())
    }

    pub fn poll_latest(
        &self,
        generation: u64,
        session_id: &str,
        workspace: &Path,
    ) -> Option<SessionChangesResponse> {
        let response = self.mailbox.state.lock().ok()?.ready.take()?;
        response
            .matches(generation, session_id, workspace)
            .then_some(response)
    }
}

impl Drop for SessionChangesWorker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.mailbox.state.lock() {
            state.stopped = true;
            state.pending = None;
            state.ready = None;
            self.mailbox.wake.notify_one();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    version: u16,
    session_id: String,
    workspace: PathBuf,
    captured_at_ms: u64,
    branch: String,
    head: Option<String>,
    inventory_complete: bool,
    files: BTreeMap<String, BaselineFile>,
    /// Complete immediate-child listings establish absence locally even when
    /// another subtree exceeds the global capture budget. Empty on old v1
    /// sidecars; skipping that field preserves their original checksum.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    directory_entries: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum BaselineFile {
    Present { base64: String },
    Omitted { reason: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    sha256: String,
    payload: Baseline,
}

impl Baseline {
    fn info(&self) -> BaselineInfo {
        let captured_files = self
            .files
            .values()
            .filter(|file| matches!(file, BaselineFile::Present { .. }))
            .count();
        BaselineInfo {
            captured_at_ms: self.captured_at_ms,
            branch: self.branch.clone(),
            head: self.head.clone(),
            captured_files,
            omitted_files: self.files.len() - captured_files,
            inventory_complete: self.inventory_complete,
        }
    }

    fn original(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self.files.get(path) {
            Some(BaselineFile::Present { base64 }) => STANDARD
                .decode(base64)
                .map(Some)
                .map_err(|_| "Dato no disponible: baseline inválido".into()),
            Some(BaselineFile::Omitted { reason }) => Err(reason.clone()),
            None if self.locally_absent(path) => Ok(None),
            None if !self.inventory_complete => {
                Err("Dato no disponible: inventario inicial incompleto".into())
            }
            None => {
                // An omitted directory/symlink also makes every descendant unknown.
                if self.files.iter().any(|(prefix, file)| {
                    matches!(file, BaselineFile::Omitted { .. })
                        && path
                            .strip_prefix(prefix)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                }) {
                    Err("Dato no disponible: carpeta omitida del baseline".into())
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn locally_absent(&self, path: &str) -> bool {
        let mut parent = String::new();
        for name in path.split('/') {
            if self
                .directory_entries
                .get(&parent)
                .is_some_and(|children| !children.contains(name))
            {
                return true;
            }
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(name);
        }
        false
    }
}

/// Call in the worker preparation step, before its first filesystem effect.
/// Failure is display-data unavailability, never permission to retry a tool.
/// A SessionStarted-only journal is eligible; any admitted/started turn is not.
pub fn capture_baseline(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    prior_events: &[AgentEvent],
) -> io::Result<BaselineCapture> {
    let root = workspace.canonicalize()?;
    let _guard = crate::session_changes_editor::lock_workspace(storage_root, &root)?;
    if let Some(baseline) = read_baseline(storage_root, session_id, &root)? {
        return Ok(BaselineCapture::Existing(baseline.info()));
    }
    if prior_events
        .iter()
        .any(|event| !matches!(event.kind, AgentEventKind::SessionStarted { .. }))
    {
        return Ok(BaselineCapture::Unavailable {
            reason: "Dato no disponible: este chat empezó sin un baseline de archivos".into(),
        });
    }
    if prior_events.iter().any(|event| event.session_id != session_id
        || matches!(&event.kind, AgentEventKind::SessionStarted { workspace } if workspace.canonicalize().ok().as_ref() != Some(&root))) {
        return Err(invalid("El journal no corresponde al chat y carpeta del baseline"));
    }
    if !root.is_dir() {
        return Err(invalid("La carpeta del chat no existe"));
    }
    let head = read_head(&root);
    let branch = read_branch(&root, head.is_some());
    let mut baseline = Baseline {
        version: VERSION,
        session_id: session_id.into(),
        workspace: root.clone(),
        captured_at_ms: now_ms(),
        branch,
        head,
        inventory_complete: true,
        files: BTreeMap::new(),
        directory_entries: BTreeMap::new(),
    };
    let mut visits = 0;
    let mut retained = 0;
    scan_directory(&root, &root, 0, &mut visits, &mut retained, &mut baseline)?;
    let info = baseline.info();
    let payload = serde_json::to_vec(&baseline).map_err(io::Error::other)?;
    let encoded = serde_json::to_vec(&Envelope {
        sha256: digest(&payload),
        payload: baseline,
    })
    .map_err(io::Error::other)?;
    if encoded.len() as u64 > MAX_BASELINE_FILE_BYTES {
        return Err(invalid("El baseline supera su límite de almacenamiento"));
    }
    let path = baseline_path(storage_root, session_id)?;
    let directory = path.parent().expect("baseline has parent");
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let outcome = (|| -> io::Result<bool> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
        Ok(true)
    })();
    let _ = fs::remove_file(&temporary);
    if outcome? {
        Ok(BaselineCapture::Created(info))
    } else {
        let existing = read_baseline(storage_root, session_id, &root)?
            .ok_or_else(|| invalid("El baseline publicado no existe"))?;
        Ok(BaselineCapture::Existing(existing.info()))
    }
}

fn scan_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    visits: &mut usize,
    retained: &mut usize,
    baseline: &mut Baseline,
) -> io::Result<()> {
    if depth > 64 {
        baseline.inventory_complete = false;
        return Ok(());
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => {
            baseline.inventory_complete = false;
            return Ok(());
        }
    };
    // Collect only bounded directory entries, so traversal is deterministic
    // without allocating an unbounded directory listing.
    let mut entries_bounded = Vec::new();
    let mut directory_complete = true;
    for entry in entries {
        if *visits >= MAX_VISITS {
            baseline.inventory_complete = false;
            directory_complete = false;
            break;
        }
        *visits += 1;
        match entry {
            Ok(entry) => entries_bounded.push(entry),
            Err(_) => {
                baseline.inventory_complete = false;
                directory_complete = false;
            }
        }
    }
    entries_bounded.sort_by_key(|entry| entry.file_name());
    if directory_complete {
        let relative = directory
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .map(|path| path.replace(std::path::MAIN_SEPARATOR, "/"));
        let names = entries_bounded
            .iter()
            .map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| normalized_path(name).ok())
            })
            .collect::<Option<BTreeSet<_>>>();
        if let (Some(relative), Some(names)) = (relative, names) {
            let directory = if relative.is_empty() {
                Ok(relative)
            } else {
                normalized_path(&relative)
            };
            if let Ok(directory) = directory {
                let bytes = baseline
                    .directory_entries
                    .iter()
                    .map(|(path, names)| path.len() + names.iter().map(String::len).sum::<usize>())
                    .sum::<usize>()
                    + directory.len()
                    + names.iter().map(String::len).sum::<usize>();
                if bytes <= MAX_DIRECTORY_INVENTORY_BYTES {
                    baseline.directory_entries.insert(directory, names);
                }
            }
        }
    }
    for entry in entries_bounded {
        if baseline.files.len() >= MAX_ENTRIES {
            baseline.inventory_complete = false;
            break;
        }
        let path = entry.path();
        let Some(relative) = path.strip_prefix(root).ok().and_then(|path| path.to_str()) else {
            baseline.inventory_complete = false;
            continue;
        };
        let relative = relative.replace(std::path::MAIN_SEPARATOR, "/");
        let relative = match normalized_path(&relative) {
            Ok(relative) => relative,
            Err(_) => {
                baseline.inventory_complete = false;
                continue;
            }
        };
        let omitted = |reason: &str| BaselineFile::Omitted {
            reason: format!("Dato no disponible: {reason}"),
        };
        if baseline.files.contains_key(&relative) {
            baseline.inventory_complete = false;
            baseline
                .files
                .insert(relative, omitted("rutas iniciales con identidad ambigua"));
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                baseline
                    .files
                    .insert(relative, omitted("archivo no legible al iniciar"));
                continue;
            }
        };
        let name = entry.file_name();
        if is_link(&metadata)
            || name == ".git"
            || name == "node_modules"
            || name == "target"
            || name == ".venv"
        {
            baseline
                .files
                .insert(relative, omitted("ruta excluida del snapshot inicial"));
        } else if metadata.is_dir() {
            scan_directory(root, &path, depth + 1, visits, retained, baseline)?;
        } else if !metadata.is_file()
            || metadata.len() > MAX_FILE_BYTES as u64
            || *retained + metadata.len() as usize > MAX_BASELINE_BYTES
        {
            baseline.files.insert(
                relative,
                omitted("archivo omitido por tipo o límite de tamaño"),
            );
        } else {
            match read_workspace_file(root, &relative) {
                Ok(bytes) if retained.saturating_add(bytes.len()) <= MAX_BASELINE_BYTES => {
                    *retained += bytes.len();
                    baseline.files.insert(
                        relative,
                        BaselineFile::Present {
                            base64: STANDARD.encode(bytes),
                        },
                    );
                }
                _ => {
                    baseline.files.insert(
                        relative,
                        omitted("archivo no legible o modificado durante el snapshot"),
                    );
                }
            }
        }
    }
    Ok(())
}

fn baseline_path(storage_root: &Path, session_id: &str) -> io::Result<PathBuf> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(invalid("ID del chat inválido"));
    }
    Ok(storage_root
        .join("session-changes")
        .join(format!("{session_id}.json")))
}

/// Remove this sidecar only after the host durably deletes the chat itself.
pub fn remove_baseline(storage_root: &Path, session_id: &str) -> io::Result<()> {
    match fs::remove_file(baseline_path(storage_root, session_id)?) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    crate::session_changes_editor::remove_marker(storage_root, session_id)?;
    tool_snapshots::remove_session(storage_root, session_id)?;
    file_review::remove_session(storage_root, session_id)
}

fn read_baseline(
    storage_root: &Path,
    session_id: &str,
    root: &Path,
) -> io::Result<Option<Baseline>> {
    read_baseline_file(storage_root, session_id, Some(root))
}

fn read_baseline_file(
    storage_root: &Path,
    session_id: &str,
    expected_root: Option<&Path>,
) -> io::Result<Option<Baseline>> {
    baseline_path(storage_root, session_id)?;
    let Some(file) = crate::session_changes_editor::open_baseline_file(storage_root, session_id)?
    else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    if is_link(&metadata) || !metadata.is_file() || metadata.len() > MAX_BASELINE_FILE_BYTES {
        return Err(invalid("Baseline inválido o demasiado grande"));
    }
    let mut encoded = Vec::new();
    file.take(MAX_BASELINE_FILE_BYTES + 1)
        .read_to_end(&mut encoded)?;
    if encoded.len() as u64 > MAX_BASELINE_FILE_BYTES {
        return Err(invalid("Baseline demasiado grande"));
    }
    let envelope: Envelope = serde_json::from_slice(&encoded).map_err(io::Error::other)?;
    let baseline = envelope.payload;
    if baseline.version != VERSION
        || baseline.session_id != session_id
        || expected_root.is_some_and(|root| baseline.workspace != root)
        || !baseline.workspace.is_absolute()
        || baseline.files.len() > MAX_ENTRIES
        || baseline.branch.len() > 256
        || baseline.head.as_ref().is_some_and(|head| head.len() > 128)
        || digest(&serde_json::to_vec(&baseline).map_err(io::Error::other)?) != envelope.sha256
    {
        return Err(invalid("Baseline no corresponde a este chat y carpeta"));
    }
    let mut retained = 0usize;
    let mut inventory_bytes = 0usize;
    let mut inventory_entries = 0usize;
    for (directory, names) in &baseline.directory_entries {
        if !directory.is_empty() {
            normalized_path(directory).map_err(io::Error::other)?;
        }
        inventory_bytes += directory.len();
        inventory_entries += names.len();
        for name in names {
            normalized_path(name).map_err(io::Error::other)?;
            if name.contains('/') {
                return Err(invalid("Inventario de carpeta inválido"));
            }
            inventory_bytes += name.len();
        }
    }
    if inventory_entries > MAX_VISITS
        || baseline.directory_entries.len() > MAX_VISITS + 1
        || inventory_bytes > MAX_DIRECTORY_INVENTORY_BYTES
    {
        return Err(invalid("Inventario de carpetas demasiado grande"));
    }
    for (path, entry) in &baseline.files {
        normalized_path(path).map_err(io::Error::other)?;
        match entry {
            BaselineFile::Present { base64 } => {
                if base64.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
                    return Err(invalid("Archivo del baseline demasiado grande"));
                }
                let bytes = STANDARD.decode(base64).map_err(io::Error::other)?;
                retained = retained.saturating_add(bytes.len());
                if bytes.len() > MAX_FILE_BYTES || retained > MAX_BASELINE_BYTES {
                    return Err(invalid("Contenido del baseline demasiado grande"));
                }
            }
            BaselineFile::Omitted { reason } if reason.len() > 256 => {
                return Err(invalid("Metadatos del baseline demasiado grandes"));
            }
            _ => {}
        }
    }
    // Older Windows sidecars kept the spelling returned by read_dir. Match
    // the executor's case-insensitive path identity only AFTER checking the
    // stored payload's checksum, rejecting ambiguous normalized aliases.
    #[cfg(windows)]
    let baseline = {
        let mut baseline = baseline;
        let mut files = BTreeMap::new();
        for (path, entry) in std::mem::take(&mut baseline.files) {
            let path = normalized_path(&path).map_err(io::Error::other)?;
            if files.insert(path, entry).is_some() {
                return Err(invalid("Baseline tiene rutas de archivo ambiguas"));
            }
        }
        baseline.files = files;
        baseline
    };
    Ok(Some(baseline))
}

fn read_workspace_file(root: &Path, relative: &str) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    let file = {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut parent = File::open(root)?;
        let parts: Vec<_> = Path::new(relative).components().collect();
        for (index, part) in parts.iter().enumerate() {
            let Component::Normal(name) = part else {
                return Err(invalid("Ruta inicial inválida"));
            };
            use std::os::unix::ffi::OsStrExt;
            let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if index + 1 < parts.len() {
                    libc::O_DIRECTORY
                } else {
                    0
                };
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            parent = unsafe { File::from_raw_fd(fd) };
        }
        parent
    };
    #[cfg(not(unix))]
    let file = {
        let mut path = root.to_path_buf();
        for component in Path::new(relative).components() {
            let Component::Normal(name) = component else {
                return Err(invalid("Ruta inicial inválida"));
            };
            path.push(name);
            if is_link(&fs::symlink_metadata(&path)?) {
                return Err(invalid("Ruta inicial enlazada"));
            }
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            );
        }
        options.open(path)?
    };
    let before = file.metadata()?;
    if !before.is_file() || is_link(&before) || before.len() > MAX_FILE_BYTES as u64 {
        return Err(invalid("Archivo inicial no regular o demasiado grande"));
    }
    let mut bytes = Vec::new();
    let mut reader = file.take((MAX_FILE_BYTES + 1) as u64);
    reader.read_to_end(&mut bytes)?;
    let after = reader.get_ref().metadata()?;
    if bytes.len() > MAX_FILE_BYTES
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(invalid("Archivo modificado durante la captura"));
    }
    Ok(bytes)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0;
    }
    #[cfg(not(windows))]
    false
}

fn read_head(root: &Path) -> Option<String> {
    let (bytes, truncated, success) =
        git_output(root, &["rev-parse", "--verify", "HEAD"], 128).ok()?;
    if !success || truncated {
        return None;
    }
    let head = String::from_utf8(bytes).ok()?.trim().to_owned();
    (matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(head)
}

fn read_branch(root: &Path, has_head: bool) -> String {
    if let Ok((bytes, false, true)) =
        git_output(root, &["symbolic-ref", "--quiet", "--short", "HEAD"], 256)
    {
        if let Ok(branch) = String::from_utf8(bytes) {
            let branch = branch.trim();
            if !branch.is_empty() && !branch.chars().any(char::is_control) {
                return branch.into();
            }
        }
    }
    if has_head {
        "HEAD separado"
    } else {
        "Rama desconocida"
    }
    .into()
}

#[derive(Clone)]
struct ReplayedFile {
    content: Result<Option<Vec<u8>>, String>,
    last_sequence: u64,
    /// State before an opaque effect cannot be used as a later patch's input.
    epoch: usize,
}

#[cfg(test)]
/// A bounded observer of the store's JSONL format. An incomplete/corrupt tail
/// is an error and is never repaired. This reader does not acquire a worker
/// lease, and does not block a native writer on a full-history store load.
pub fn load_stored_session_changes(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
) -> io::Result<SessionChanges> {
    load_stored_changes(storage_root, session_id, workspace, false)
}

fn load_stored_changes(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    last_turn: bool,
) -> io::Result<SessionChanges> {
    baseline_path(storage_root, session_id)?;
    if read_baseline(storage_root, session_id, &workspace.canonicalize()?)?.is_none() {
        return unavailable_changes(
            session_id,
            workspace,
            "Dato no disponible: este chat no tiene un baseline de archivos",
        );
    }
    match read_stored_journal(storage_root, session_id)? {
        StoredJournal::Events(events) => {
            load_changes(storage_root, session_id, workspace, &events, last_turn)
        }
        StoredJournal::Unavailable(reason) => unavailable_changes(session_id, workspace, &reason),
    }
}

enum StoredJournal {
    Events(Vec<AgentEvent>),
    Unavailable(String),
}

fn read_stored_journal(storage_root: &Path, session_id: &str) -> io::Result<StoredJournal> {
    read_stored_journal_with_budget(storage_root, session_id, MAX_JOURNAL_BYTES)
        .map(|(journal, _)| journal)
}

fn read_stored_journal_with_budget(
    storage_root: &Path,
    session_id: &str,
    available_bytes: u64,
) -> io::Result<(StoredJournal, u64)> {
    let read_limit = available_bytes.min(MAX_JOURNAL_BYTES);
    baseline_path(storage_root, session_id)?;
    let file = match crate::session_changes_editor::open_journal_file(storage_root, session_id) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((StoredJournal::Events(Vec::new()), 0));
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("Journal de cambios no es un archivo regular"));
    }
    let mut bytes = Vec::new();
    file.take(read_limit + 1).read_to_end(&mut bytes)?;
    let read_bytes = bytes.len() as u64;
    if read_bytes > read_limit {
        return Ok((
            StoredJournal::Unavailable(
                "Dato no disponible: el journal supera el límite de lectura de cambios".into(),
            ),
            read_bytes,
        ));
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(invalid(
            "Journal de cambios tiene un registro incompleto; reintentá la lectura",
        ));
    }
    let mut events = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        if events.len() == MAX_EVENTS {
            return Ok((
                StoredJournal::Unavailable(
                    "Dato no disponible: el journal supera el límite de eventos de cambios".into(),
                ),
                read_bytes,
            ));
        }
        events.push(serde_json::from_slice::<AgentEvent>(line).map_err(io::Error::other)?);
    }
    Ok((StoredJournal::Events(events), read_bytes))
}

/// Enumerate every baseline, never silently omit a matching chat. Called only
/// under the shared workspace guard. Bounded failures cancel the save.
pub(crate) fn editor_save_boundaries(
    storage_root: &Path,
    workspace: &Path,
    deadline: Instant,
) -> io::Result<Vec<(String, u64)>> {
    const MAX_SESSIONS: usize = 2048;
    const MAX_DIRECTORY_VISITS: usize = 8192;
    const MAX_READ_BYTES: u64 = 128 * 1024 * 1024;
    let directory = storage_root.join("session-changes");
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(invalid(
            "El registro de baselines del guardado no es un directorio seguro",
        ));
    }
    let check_budget = |bytes: u64| -> io::Result<()> {
        if bytes > MAX_READ_BYTES || Instant::now() >= deadline {
            Err(invalid(
                "El registro histórico supera su límite de lectura/tiempo; reintentá guardar",
            ))
        } else {
            Ok(())
        }
    };
    let mut ids = Vec::new();
    let mut read_bytes = 0u64;
    for (visits, entry) in fs::read_dir(&directory)?.enumerate() {
        if visits >= MAX_DIRECTORY_VISITS {
            return Err(invalid(
                "Demasiadas entradas históricas para guardar con evidencia",
            ));
        }
        check_budget(read_bytes)?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(invalid("Nombre histórico no Unicode"));
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        baseline_path(storage_root, id)?;
        if ids.len() == MAX_SESSIONS {
            return Err(invalid("Demasiados baselines para guardar con evidencia"));
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        read_bytes = read_bytes.saturating_add(metadata.len());
        check_budget(read_bytes)?;
        ids.push(id.to_owned());
    }
    ids.sort();
    let mut boundaries = Vec::new();
    for id in ids {
        check_budget(read_bytes)?;
        // Scope is trusted only after full checksum/content validation. A bad
        // foreign sidecar cannot be silently classified away by its header.
        let baseline = read_baseline_file(storage_root, &id, None)?
            .ok_or_else(|| invalid("Un baseline desapareció durante el guardado"))?;
        if baseline.workspace != workspace {
            continue;
        }
        let (journal, journal_bytes) = read_stored_journal_with_budget(
            storage_root,
            &id,
            MAX_READ_BYTES.saturating_sub(read_bytes),
        )?;
        read_bytes = read_bytes.saturating_add(journal_bytes);
        check_budget(read_bytes)?;
        let events = match journal {
            StoredJournal::Events(events) => events,
            StoredJournal::Unavailable(reason) => return Err(invalid(&reason)),
        };
        validate_journal(&id, workspace, &events)?;
        check_budget(read_bytes)?;
        let after_sequence = editor_boundary_sequence(&events)?;
        // Valid existing markers are retained by the publisher; malformed or
        // wrong-scope markers fail before the file effect.
        crate::session_changes_editor::editor_save_boundary(storage_root, &id, workspace)?;
        boundaries.push((id, after_sequence));
    }
    check_budget(read_bytes)?;
    Ok(boundaries)
}

fn editor_boundary_sequence(events: &[AgentEvent]) -> io::Result<u64> {
    let mut pending = HashMap::<&str, u64>::new();
    let mut hooks = HashMap::<(&str, &str, &str), u64>::new();
    let mut cutoff = events.last().map_or(0, |event| event.sequence);
    for event in events {
        match &event.kind {
            AgentEventKind::ToolStarted { tool_call }
                if matches!(tool_call.name.as_str(), "write_file" | "apply_patch")
                    || opaque_effect(&tool_call.name) =>
            {
                let previous = pending.insert(tool_call.id.as_str(), event.sequence);
                if previous.is_some() {
                    return Err(invalid(
                        "Herramientas pendientes ambiguas al registrar el guardado",
                    ));
                }
            }
            AgentEventKind::ToolFinished { tool_call_id, .. } => {
                pending.remove(tool_call_id.as_str());
            }
            AgentEventKind::HookStarted {
                tool_call_id,
                hook_id,
                phase,
            } => {
                hooks.insert((tool_call_id, hook_id, phase), event.sequence);
            }
            AgentEventKind::HookFinished {
                tool_call_id,
                hook_id,
                phase,
                ..
            } => {
                hooks.remove(&(tool_call_id.as_str(), hook_id.as_str(), phase.as_str()));
            }
            _ => {}
        }
    }
    for sequence in pending.values().chain(hooks.values()) {
        cutoff = cutoff.min(sequence.saturating_sub(1));
    }
    Ok(cutoff)
}

fn unavailable_changes(
    session_id: &str,
    workspace: &Path,
    reason: &str,
) -> io::Result<SessionChanges> {
    Ok(SessionChanges {
        session_id: session_id.into(),
        workspace: workspace.into(),
        baseline: None,
        availability: ChangesAvailability::Unavailable {
            reason: reason.into(),
        },
        totals: ChangeTotals::default(),
        files: Vec::new(),
        coverage_notes: Vec::new(),
    })
}

/// The chat's changes since its baseline, or with `last_turn` only those of
/// its last turn: the files as the turns before it left them are then what
/// each one is compared against.
fn load_changes(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    events: &[AgentEvent],
    last_turn: bool,
) -> io::Result<SessionChanges> {
    let started = Instant::now();
    let root = workspace.canonicalize()?;
    let mut result = SessionChanges {
        session_id: session_id.into(),
        workspace: workspace.into(),
        baseline: None,
        availability: ChangesAvailability::Complete,
        totals: ChangeTotals {
            exact: true,
            ..ChangeTotals::default()
        },
        files: Vec::new(),
        coverage_notes: Vec::new(),
    };
    let Some(baseline) = read_baseline(storage_root, session_id, &root)? else {
        result.availability = ChangesAvailability::Unavailable {
            reason: "Dato no disponible: este chat no tiene un baseline de archivos".into(),
        };
        result.totals.exact = false;
        return Ok(result);
    };
    result.baseline = Some(baseline.info());
    let ReplayProjection {
        replayed,
        mut notes,
        ..
    } = replay_session(
        storage_root,
        session_id,
        &root,
        &baseline,
        events,
        started,
        None,
    )?;
    let turn_start = events
        .iter()
        .rposition(|event| matches!(event.kind, AgentEventKind::TurnStarted { .. }))
        .filter(|_| last_turn);
    let found = match turn_start {
        Some(start) => replay_session(storage_root, session_id, &root, &baseline, &events[..start], started, None)?.replayed,
        None => BTreeMap::new(),
    };
    let mut total_diff_bytes = 0usize;
    for (path, replayed) in replayed {
        let original = match found.get(&path) {
            Some(before) => before.content.clone(),
            None => baseline.original(&path),
        };
        if original.as_ref().ok() == replayed.content.as_ref().ok() && original.is_ok() {
            continue;
        }
        let mut change = SessionFileChange {
            file: ReviewFile {
                path: path.clone(),
                ..ReviewFile::default()
            },
            last_tool_sequence: replayed.last_sequence,
            unavailable: None,
            truncated: false,
        };
        match (original, replayed.content) {
            (Ok(before), Ok(after)) => {
                change.file.status = match (before.is_some(), after.is_some()) {
                    (false, true) => "Nuevo",
                    (true, false) => "Eliminado",
                    _ => "Modificado",
                }
                .into();
                let diff = if started.elapsed() >= PROJECTION_BUDGET {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "se alcanzó el tiempo máximo de proyección del chat",
                    ))
                } else {
                    snapshot_diff(
                        storage_root,
                        &path,
                        before.as_deref().unwrap_or_default(),
                        after.as_deref().unwrap_or_default(),
                        MAX_DIFF_BYTES.min(MAX_ALL_DIFF_BYTES.saturating_sub(total_diff_bytes)),
                    )
                };
                match diff {
                    Ok((file, truncated, bytes)) => {
                        change.file = file;
                        change.file.status = match (before.is_some(), after.is_some()) {
                            (false, true) => "Nuevo",
                            (true, false) => "Eliminado",
                            _ => "Modificado",
                        }
                        .into();
                        change.truncated = truncated;
                        total_diff_bytes += bytes;
                    }
                    Err(error) => change.unavailable = Some(format!("Dato no disponible: {error}")),
                }
            }
            (Err(reason), _) | (_, Err(reason)) => {
                change.file.status = "Dato no disponible".into();
                change.unavailable = Some(reason);
            }
        }
        result.totals.exact &= change.unavailable.is_none();
        result.totals.files += 1;
        result.totals.additions += change.file.additions;
        result.totals.deletions += change.file.deletions;
        if change.unavailable.is_some() || change.truncated {
            notes.insert("Cobertura limitada: algunos archivos no tienen diff completo".into());
        }
        result.files.push(change);
    }
    if !notes.is_empty() {
        result.availability = ChangesAvailability::Limited;
        result.totals.exact = false;
    }
    result.coverage_notes = notes.into_iter().take(16).collect();
    Ok(result)
}

const MAX_TOOL_DIFF_FILES: usize = 16;

pub(super) fn load_stored_tool_diff_projection(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    tool_call_id: &str,
) -> io::Result<ToolDiffProjection> {
    let started = Instant::now();
    let root = workspace.canonicalize()?;
    baseline_path(storage_root, session_id)?;
    if tool_call_id.is_empty()
        || tool_call_id.len() > 1024
        || tool_call_id.chars().any(char::is_control)
    {
        return Err(invalid("Identificador de herramienta inválido"));
    }
    let events = match read_stored_journal(storage_root, session_id)? {
        StoredJournal::Events(events) => events,
        StoredJournal::Unavailable(reason) => return Ok(ToolDiffProjection::unavailable(reason)),
    };
    // Validate the entire available journal even though replay stops at the
    // selected historical Finished. Corrupt later records are not ignored.
    validate_journal(session_id, &root, &events)?;
    let starts = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
        AgentEventKind::ToolStarted { tool_call } if tool_call.id == tool_call_id)
        })
        .count();
    let finishes = events
        .iter()
        .filter(|event| {
            matches!(&event.kind,
        AgentEventKind::ToolFinished { tool_call_id: id, .. } if id == tool_call_id)
        })
        .count();
    if starts != 1 || finishes != 1 {
        return Ok(ToolDiffProjection::unavailable(
            "Dato no disponible: la edición no tiene una entrada y un resultado confirmado únicos",
        ));
    }
    let (call_start, call) = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } if tool_call.id == tool_call_id => {
                Some((event, tool_call))
            }
            _ => None,
        })
        .expect("one validated tool start");
    let successful_sequence = events.iter().find_map(|event| match &event.kind {
        AgentEventKind::ToolFinished {
            tool_call_id: id,
            is_error: false,
            ..
        } if id == tool_call_id => Some(event.sequence),
        _ => None,
    });
    let recorded_root = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } => Some(workspace.as_path()),
            _ => None,
        })
        .unwrap_or(&root);
    let exact = match successful_sequence {
        Some(sequence) => tool_snapshots::load_transition(
            storage_root,
            session_id,
            &root,
            recorded_root,
            call,
            call_start,
            sequence,
        )?,
        None => None,
    };
    let (selected, mut notes) = if let Some(exact) = exact {
        (Some(exact), BTreeSet::new())
    } else {
        let Some(baseline) = read_baseline(storage_root, session_id, &root)? else {
            return Ok(ToolDiffProjection::unavailable(
                "Dato no disponible: este chat no tiene un baseline de archivos",
            ));
        };
        let replay = replay_session(
            storage_root,
            session_id,
            &root,
            &baseline,
            &events,
            started,
            Some(tool_call_id),
        )?;
        (replay.selected, replay.notes)
    };
    let Some(transition) = selected else {
        return Ok(ToolDiffProjection::unavailable(
            "Dato no disponible: no se pudo alcanzar el resultado confirmado de esta edición",
        ));
    };
    let mut projection = ToolDiffProjection {
        sequence: transition.sequence,
        files: Vec::new(),
        unavailable: transition.unavailable.clone(),
        coverage_notes: Vec::new(),
    };
    let mut paths: BTreeSet<_> = transition.before.keys().cloned().collect();
    // A confirmed move is represented as one old-path -> new-path diff.
    // Untrusted replay never manufactures a rename from current disk state.
    if transition.unavailable.is_none() {
        for (destination, source) in &transition.renames {
            if transition
                .after
                .get(source)
                .is_some_and(|content| matches!(content, Ok(None)))
                && transition
                    .after
                    .get(destination)
                    .is_some_and(|content| matches!(content, Ok(Some(_))))
            {
                paths.remove(source);
            }
        }
    }
    if paths.len() > MAX_TOOL_DIFF_FILES {
        notes.insert(
            "Cobertura limitada: esta edición supera el límite de 16 archivos de diff".into(),
        );
    }
    let deadline = started + PROJECTION_BUDGET;
    let mut total_bytes = 0usize;
    let mut total_lines = 0usize;
    for path in paths.into_iter().take(MAX_TOOL_DIFF_FILES) {
        let old_path = transition
            .renames
            .get(&path)
            .filter(|source| {
                transition.unavailable.is_none()
                    && transition
                        .after
                        .get(*source)
                        .is_some_and(|content| matches!(content, Ok(None)))
            })
            .cloned();
        let before = transition
            .before
            .get(old_path.as_deref().unwrap_or(&path))
            .cloned()
            .unwrap_or_else(|| {
                Err("Dato no disponible: no hay contenido anterior verificable".into())
            });
        let after = transition.after.get(&path).cloned().unwrap_or_else(|| {
            Err("Dato no disponible: no hay contenido posterior verificable".into())
        });
        if before.is_ok() && before.as_ref().ok() == after.as_ref().ok() && old_path.is_none() {
            continue;
        }
        let mut change = SessionFileChange {
            file: ReviewFile {
                path: path.clone(),
                old_path: old_path.clone(),
                ..ReviewFile::default()
            },
            last_tool_sequence: transition.sequence,
            unavailable: None,
            truncated: false,
        };
        match (before, after) {
            (Ok(before), Ok(after)) => {
                let status = if old_path.is_some() {
                    "Renombrado"
                } else {
                    match (before.is_some(), after.is_some()) {
                        (false, true) => "Nuevo",
                        (true, false) => "Eliminado",
                        _ => "Modificado",
                    }
                };
                let diff = if total_lines >= MAX_DIFF_LINES || total_bytes >= MAX_ALL_DIFF_BYTES {
                    Err(invalid(
                        "se alcanzó el límite agregado de líneas o bytes del diff",
                    ))
                } else if Instant::now() >= deadline {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "se alcanzó el tiempo máximo de diff de la edición",
                    ))
                } else {
                    snapshot_diff_until(
                        storage_root,
                        &path,
                        before.as_deref().unwrap_or_default(),
                        after.as_deref().unwrap_or_default(),
                        MAX_DIFF_BYTES.min(MAX_ALL_DIFF_BYTES - total_bytes),
                        deadline,
                    )
                };
                match diff {
                    Ok((mut file, truncated, bytes)) => {
                        if let Some(source) = &old_path {
                            for row in &mut file.lines {
                                if row.text.starts_with("diff --git ") {
                                    row.text = format!("diff --git a/{source} b/{path}");
                                } else if row.text.starts_with("--- a/") {
                                    row.text = format!("--- a/{source}");
                                }
                            }
                        }
                        file.old_path = old_path;
                        file.status = status.into();
                        change.truncated =
                            truncated || file.lines.len() > MAX_DIFF_LINES - total_lines;
                        file.lines.truncate(MAX_DIFF_LINES - total_lines);
                        total_lines += file.lines.len();
                        total_bytes += bytes;
                        change.file = file;
                    }
                    Err(error) => {
                        change.file.status = status.into();
                        change.unavailable = Some(format!("Dato no disponible: {error}"));
                    }
                }
            }
            (Err(reason), Ok(Some(after))) if transition.allow_after_snapshot => {
                // A full write proves these bytes, not that the file was new.
                // Keep the unavailable diff reason and show neutral content.
                match confirmed_text_snapshot(
                    &path,
                    &after,
                    MAX_DIFF_LINES.saturating_sub(total_lines),
                    MAX_DIFF_BYTES.min(MAX_ALL_DIFF_BYTES.saturating_sub(total_bytes)),
                ) {
                    Ok((file, truncated, bytes)) => {
                        total_lines += file.lines.len();
                        total_bytes += bytes;
                        change.file = file;
                        change.truncated = truncated;
                        change.unavailable = Some(format!(
                            "Contenido escrito confirmado · versión anterior no disponible. {reason}"
                        ));
                    }
                    Err(error) => {
                        change.file.status = "Dato no disponible".into();
                        change.unavailable = Some(format!("{reason}. {error}"));
                    }
                }
            }
            (Err(reason), _) | (_, Err(reason)) => {
                change.file.status = "Dato no disponible".into();
                change.unavailable = Some(reason);
            }
        }
        if change.unavailable.is_some() || change.truncated {
            notes.insert(
                "Cobertura limitada: algunos archivos de esta edición no tienen un diff completo"
                    .into(),
            );
        }
        projection.files.push(change);
    }
    projection.coverage_notes = notes.into_iter().take(16).collect();
    Ok(projection)
}

pub(super) struct ToolDiffProjection {
    pub sequence: u64,
    pub files: Vec<SessionFileChange>,
    pub unavailable: Option<String>,
    pub coverage_notes: Vec<String>,
}

impl ToolDiffProjection {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            sequence: 0,
            files: Vec::new(),
            unavailable: Some(reason.into()),
            coverage_notes: Vec::new(),
        }
    }
}

struct ToolTransition {
    sequence: u64,
    before: BTreeMap<String, Result<Option<Vec<u8>>, String>>,
    after: BTreeMap<String, Result<Option<Vec<u8>>, String>>,
    renames: BTreeMap<String, String>,
    unavailable: Option<String>,
    allow_after_snapshot: bool,
}

impl ToolTransition {
    fn unavailable(sequence: u64, reason: impl Into<String>) -> Self {
        Self {
            sequence,
            before: BTreeMap::new(),
            after: BTreeMap::new(),
            renames: BTreeMap::new(),
            unavailable: Some(reason.into()),
            allow_after_snapshot: false,
        }
    }

    fn finish(
        mut self,
        replayed: &BTreeMap<String, ReplayedFile>,
        sequence: u64,
        failure: Option<String>,
    ) -> Self {
        self.sequence = sequence;
        for path in self.before.keys() {
            let after = match &failure {
                Some(reason) => Err(reason.clone()),
                None => replayed.get(path).filter(|file| file.last_sequence == sequence)
                    .map(|file| file.content.clone())
                    .unwrap_or_else(|| Err("Dato no disponible: el resultado no confirma el contenido de esta ruta".into())),
            };
            self.after.insert(path.clone(), after);
        }
        if failure.is_some() {
            self.unavailable = failure;
        }
        self
    }
}

fn validate_journal(session_id: &str, root: &Path, events: &[AgentEvent]) -> io::Result<()> {
    if events.len() > MAX_EVENTS {
        return Err(invalid("Journal de cambios supera el límite de eventos"));
    }
    let mut previous = 0u64;
    for event in events {
        if event.session_id != session_id || previous.checked_add(1) != Some(event.sequence) {
            return Err(invalid(
                "Journal de cambios no corresponde al chat o está fuera de orden",
            ));
        }
        if matches!(&event.kind, AgentEventKind::SessionStarted { workspace }
            if workspace.canonicalize().ok().as_deref() != Some(root))
        {
            return Err(invalid("Journal de cambios corresponde a otra carpeta"));
        }
        previous = event.sequence;
    }
    Ok(())
}

fn call_paths(call: &ToolCall, roots: &[&Path]) -> Result<BTreeSet<String>, String> {
    match call.name.as_str() {
        "write_file" => Ok(BTreeSet::from([normalized_tool_path(
            call.arguments
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or("Dato no disponible: escritura sin ruta")?,
            roots,
        )?])),
        "apply_patch" => {
            let patch = call
                .arguments
                .get("patch")
                .and_then(|value| value.as_str())
                .ok_or("Dato no disponible: patch sin contenido")?;
            let paths = patch_paths(patch);
            if paths.is_empty() {
                Err("Dato no disponible: patch sin rutas verificables".into())
            } else {
                Ok(paths)
            }
        }
        _ => Err("Dato no disponible: herramienta sin rutas de edición verificables".into()),
    }
}

fn patch_renames(call: &ToolCall) -> BTreeMap<String, String> {
    let Some(patch) = call.arguments.get("patch").and_then(|value| value.as_str()) else {
        return BTreeMap::new();
    };
    if patch.len() > MAX_PATCH_BYTES {
        return BTreeMap::new();
    }
    let mut source = None;
    let mut renames = BTreeMap::new();
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            source = normalized_path(path).ok();
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            if let (Some(source), Ok(destination)) = (source.take(), normalized_path(path)) {
                renames.insert(destination, source);
            }
        } else if line.starts_with("*** Add File: ") || line.starts_with("*** Delete File: ") {
            source = None;
        }
    }
    renames
}

fn tool_before(
    baseline: &Baseline,
    replayed: &BTreeMap<String, ReplayedFile>,
    call: &ToolCall,
    epoch: usize,
    pending: &HashMap<&str, &ToolCall>,
    parallel_overlap: bool,
    roots: &[&Path],
) -> ToolTransition {
    let paths = match call_paths(call, roots) {
        Ok(paths) => paths,
        Err(reason) => return ToolTransition::unavailable(0, reason),
    };
    let overlaps = parallel_overlap
        || pending
            .values()
            .filter(|other| matches!(other.name.as_str(), "write_file" | "apply_patch"))
            .any(|other| call_paths(other, roots).map_or(true, |other| !paths.is_disjoint(&other)));
    let additions: BTreeSet<_> = if call.name == "apply_patch" {
        call.arguments
            .get("patch")
            .and_then(|value| value.as_str())
            .into_iter()
            .flat_map(str::lines)
            .filter_map(|line| line.strip_prefix("*** Add File: "))
            .filter_map(|path| normalized_path(path).ok())
            .collect()
    } else {
        BTreeSet::new()
    };
    let before = paths.into_iter().map(|path| {
        let content = if overlaps {
            Err("Dato no disponible: otra edición de estas rutas sigue sin resultado confirmado".into())
        } else if additions.contains(&path) {
            // Only reached for a successful patch. The executor rejects Add
            // when its target exists; replay still validates the entire patch.
            Ok(None)
        } else { file_before(baseline, replayed, &path, epoch) };
        (path, content)
    }).collect();
    ToolTransition {
        sequence: 0,
        before,
        after: BTreeMap::new(),
        renames: patch_renames(call),
        unavailable: None,
        allow_after_snapshot: !overlaps && epoch == 0 && call.name == "write_file",
    }
}

struct ReplayProjection {
    replayed: BTreeMap<String, ReplayedFile>,
    notes: BTreeSet<String>,
    selected: Option<ToolTransition>,
}

fn replay_session(
    storage_root: &Path,
    session_id: &str,
    root: &Path,
    baseline: &Baseline,
    events: &[AgentEvent],
    started: Instant,
    target_tool: Option<&str>,
) -> io::Result<ReplayProjection> {
    validate_journal(session_id, root, events)?;
    // Validation proves the recorded workspace aliases this baseline root.
    // Strip either exact component prefix without inspecting the current file:
    // a later deletion, rename or symlink must not rewrite historical evidence.
    let recorded_root = events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::SessionStarted { workspace } if workspace.is_absolute() => {
                Some(workspace.as_path())
            }
            _ => None,
        })
        .unwrap_or(root);
    let path_roots = [root, recorded_root];
    let mut selected = None;
    let mut selected_parallel_overlap = false;
    let mut replayed = BTreeMap::<String, ReplayedFile>::new();
    let mut pending = HashMap::<&str, &ToolCall>::new();
    let mut post_hook_epochs = HashMap::<&str, usize>::new();
    let mut notes = BTreeSet::<String>::new();
    let mut effect_epoch = 0usize;
    let metadata_sequences = openide_harness::project_instruction_metadata_sequences(events, root);
    let external_boundary = crate::session_changes_external::external_runtime_was_prepared(
        storage_root,
        session_id,
        root,
    )?;
    let mut external_barrier_applied = false;
    let editor_boundary =
        crate::session_changes_editor::editor_save_boundary(storage_root, session_id, root)?;
    if editor_boundary.is_some_and(|cut| cut > events.last().map_or(0, |event| event.sequence)) {
        return Err(invalid(
            "El marcador de guardado supera el journal disponible; reintentá la lectura",
        ));
    }
    let mut editor_barrier_applied = false;
    if editor_boundary.is_some() && target_tool.is_none() {
        notes.insert("Cobertura limitada: hubo guardados manuales del editor; la comparación neta no representa cambios exclusivos de la IA".into());
    }
    if external_boundary.is_some() && target_tool.is_none() {
        notes.insert("Cobertura limitada: se prepararon servicios externos sin snapshots verificables de sus efectos de archivo".into());
    }
    // Native manual-command operations have their own durable sidecars. Their
    // absence from AgentEvent does not prove that no process touched files.
    for suffix in ["native-command-operations", "native-command-output"] {
        match fs::symlink_metadata(storage_root.join(format!("{session_id}.{suffix}.json"))) {
            Ok(_) => {
                effect_epoch = 1;
                notes.insert("Cobertura limitada: hay operaciones de comandos manuales fuera del journal de archivos".into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut previous_sequence = 0u64;
    for event in events.iter().take(MAX_EVENTS) {
        if started.elapsed() >= PROJECTION_BUDGET {
            notes.insert(
                "Cobertura limitada: se alcanzó el tiempo máximo de proyección del chat".into(),
            );
            break;
        }
        if event.session_id != session_id
            || previous_sequence.checked_add(1) != Some(event.sequence)
        {
            return Err(invalid(
                "Journal de cambios no corresponde al chat o está fuera de orden",
            ));
        }
        previous_sequence = event.sequence;
        // Preserve historical file effects confirmed before MCP was enabled.
        // Its durable startup boundary cuts only later patch replay.
        if !external_barrier_applied
            && external_boundary.is_some_and(|sequence| event.sequence > sequence)
        {
            effect_epoch = effect_epoch.saturating_add(1);
            external_barrier_applied = true;
            if target_tool.is_some() {
                notes.insert("Cobertura limitada: se prepararon servicios externos sin snapshots verificables de sus efectos de archivo".into());
            }
        }
        if !editor_barrier_applied
            && editor_boundary.is_some_and(|sequence| event.sequence > sequence)
        {
            effect_epoch = effect_epoch.saturating_add(1);
            editor_barrier_applied = true;
            // A post-tool hook epoch recorded before the manual save must not
            // exempt a still-pending call from this newer boundary.
            for epoch in post_hook_epochs.values_mut() {
                *epoch = effect_epoch;
            }
            notes.insert("Cobertura limitada: hubo guardados manuales del editor; la comparación neta no representa cambios exclusivos de la IA".into());
        }
        match &event.kind {
            AgentEventKind::SessionStarted {
                workspace: event_root,
            } if event_root.canonicalize().ok().as_deref() != Some(root) => {
                return Err(invalid("Journal de cambios corresponde a otra carpeta"));
            }
            AgentEventKind::ToolStarted { tool_call } => {
                if let Some(target) = target_tool {
                    // Finished order alone does not establish physical effect
                    // order for overlapping edit lifetimes on the same paths.
                    if matches!(tool_call.name.as_str(), "write_file" | "apply_patch") {
                        let target_call = if tool_call.id == target {
                            Some(tool_call)
                        } else {
                            pending.get(target).copied()
                        };
                        if let Some(target_call) = target_call {
                            let target_paths = call_paths(target_call, &path_roots);
                            let other_calls: Vec<_> = if tool_call.id == target {
                                pending
                                    .values()
                                    .copied()
                                    .filter(|call| {
                                        matches!(call.name.as_str(), "write_file" | "apply_patch")
                                    })
                                    .collect()
                            } else {
                                vec![tool_call]
                            };
                            selected_parallel_overlap |= other_calls.iter().any(|other| {
                                match (&target_paths, call_paths(other, &path_roots)) {
                                    (Ok(target_paths), Ok(other_paths)) => {
                                        !target_paths.is_disjoint(&other_paths)
                                    }
                                    _ => true,
                                }
                            });
                        }
                    }
                }
                post_hook_epochs.remove(tool_call.id.as_str());
                if opaque_effect(&tool_call.name) {
                    effect_epoch = effect_epoch.saturating_add(1);
                    notes.insert("Cobertura limitada: comandos, subagentes y otras herramientas no tienen evidencia de cambios por archivo".into());
                }
                if pending.len() < MAX_PENDING_TOOLS {
                    if pending.insert(&tool_call.id, tool_call).is_some() {
                        effect_epoch = effect_epoch.saturating_add(1);
                        notes.insert("Cobertura limitada: una entrada de herramienta reemplaza un efecto sin resultado confirmado".into());
                    }
                } else {
                    notes.insert("Dato no disponible: demasiadas herramientas simultáneas".into());
                }
            }
            AgentEventKind::ToolFinished {
                tool_call_id,
                is_error,
                ..
            } => {
                let Some(call) = pending.remove(tool_call_id.as_str()) else {
                    effect_epoch = effect_epoch.saturating_add(1);
                    notes.insert(
                        "Cobertura limitada: hay resultados sin entrada de herramienta verificable"
                            .into(),
                    );
                    continue;
                };
                let tool_epoch = post_hook_epochs
                    .remove(tool_call_id.as_str())
                    .unwrap_or(effect_epoch);
                let is_selected = target_tool == Some(tool_call_id.as_str());
                if call.name == "write_file" || call.name == "apply_patch" {
                    if *is_error {
                        if is_selected {
                            selected = Some(ToolTransition::unavailable(
                                event.sequence,
                                "Dato no disponible: la edición falló y no tiene un efecto de archivo confirmado",
                            ));
                        }
                        if !metadata_sequences.contains(&event.sequence) {
                            effect_epoch = effect_epoch.saturating_add(1);
                            notes.insert("Cobertura limitada: una escritura fallida no demuestra que ningún archivo haya cambiado".into());
                        }
                        if is_selected {
                            break;
                        }
                        continue;
                    }
                    let before = is_selected.then(|| {
                        tool_before(
                            baseline,
                            &replayed,
                            call,
                            tool_epoch,
                            &pending,
                            selected_parallel_overlap,
                            &path_roots,
                        )
                    });
                    let outcome = replay_call(
                        baseline,
                        &mut replayed,
                        call,
                        event.sequence,
                        tool_epoch,
                        &path_roots,
                    );
                    if let Err(reason) = &outcome {
                        notes.insert(reason.clone());
                    }
                    if let Some(before) = before {
                        selected = Some(before.finish(&replayed, event.sequence, outcome.err()));
                        break;
                    }
                } else if opaque_effect(&call.name) {
                    effect_epoch = effect_epoch.saturating_add(1);
                    notes.insert("Cobertura limitada: comandos, subagentes y otras herramientas no tienen evidencia de cambios por archivo".into());
                }
                if is_selected {
                    selected = Some(ToolTransition::unavailable(
                        event.sequence,
                        "Dato no disponible: esta herramienta no tiene una edición de archivos reconstruible",
                    ));
                    break;
                }
            }
            AgentEventKind::ProcessOutput { .. } => {
                effect_epoch = effect_epoch.saturating_add(1);
                notes.insert("Cobertura limitada: la salida del proceso no prueba qué archivos cambió el agente".into());
            }
            AgentEventKind::HookStarted {
                tool_call_id,
                phase,
                ..
            } => {
                if metadata_sequences.contains(&event.sequence) {
                    continue;
                }
                if phase != "pre_tool_use" && pending.contains_key(tool_call_id.as_str()) {
                    post_hook_epochs.entry(tool_call_id).or_insert(effect_epoch);
                }
                effect_epoch = effect_epoch.saturating_add(1);
                notes.insert("Cobertura limitada: los hooks pueden modificar archivos y no tienen un snapshot de sus efectos".into());
            }
            AgentEventKind::HookFinished { .. } => {
                if metadata_sequences.contains(&event.sequence) {
                    continue;
                }
                effect_epoch = effect_epoch.saturating_add(1);
                notes.insert("Cobertura limitada: los hooks pueden modificar archivos y no tienen un snapshot de sus efectos".into());
            }
            _ => {}
        }
    }
    if events.len() > MAX_EVENTS {
        notes.insert("Cobertura limitada: se alcanzó el límite de eventos del chat".into());
    }
    if pending.values().any(|call| {
        matches!(call.name.as_str(), "write_file" | "apply_patch") || opaque_effect(&call.name)
    }) {
        notes.insert("Cobertura limitada: hay herramientas con posibles efectos de archivo sin resultado confirmado".into());
    }
    Ok(ReplayProjection {
        replayed,
        notes,
        selected,
    })
}

fn normalized_path(value: &str) -> Result<String, String> {
    #[cfg(windows)]
    let value = value.replace('\\', "/");
    #[cfg(windows)]
    let value = value.as_str();
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || Path::new(value).is_absolute()
    {
        return Err("Dato no disponible: ruta de archivo inválida".into());
    }
    let mut parts = Vec::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => {
                parts.push(part.to_str().ok_or("Dato no disponible: ruta no UTF-8")?)
            }
            Component::CurDir => {}
            _ => return Err("Dato no disponible: ruta fuera de la carpeta del chat".into()),
        }
    }
    if parts.is_empty() {
        return Err("Dato no disponible: ruta de archivo vacía".into());
    }
    let path = parts.join("/");
    #[cfg(windows)]
    let path = path.to_lowercase();
    Ok(path)
}

fn normalized_tool_path(value: &str, roots: &[&Path]) -> Result<String, String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return normalized_path(value);
    }
    if value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err("Dato no disponible: ruta de archivo inválida".into());
    }
    let relative = roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .ok_or("Dato no disponible: ruta fuera de la carpeta del chat")?;
    normalized_path(
        relative
            .to_str()
            .ok_or("Dato no disponible: ruta no UTF-8")?,
    )
}

fn opaque_effect(name: &str) -> bool {
    // The runner intercepts update_plan before execute_with_hooks; it only
    // journals PlanUpdated metadata and cannot modify workspace files.
    !matches!(
        name,
        "write_file" | "apply_patch" | "read_file" | "list_files" | "search_files" | "update_plan"
    )
}

fn file_before(
    baseline: &Baseline,
    replayed: &BTreeMap<String, ReplayedFile>,
    path: &str,
    effect_epoch: usize,
) -> Result<Option<Vec<u8>>, String> {
    if effect_epoch > 0 {
        Err(
            "Dato no disponible: hay actores o efectos sin snapshots de archivos verificables"
                .into(),
        )
    } else if let Some(file) = replayed.get(path).filter(|file| file.epoch == effect_epoch) {
        file.content.clone()
    } else {
        baseline.original(path)
    }
}

fn replay_call(
    baseline: &Baseline,
    replayed: &mut BTreeMap<String, ReplayedFile>,
    call: &ToolCall,
    sequence: u64,
    effect_epoch: usize,
    roots: &[&Path],
) -> Result<(), String> {
    let updates = if call.name == "write_file" {
        let path = normalized_tool_path(
            call.arguments
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or("Dato no disponible: escritura sin ruta")?,
            roots,
        )?;
        let content = call
            .arguments
            .get("content")
            .and_then(|value| value.as_str())
            .ok_or("Dato no disponible: escritura sin contenido")?;
        if content.len() > MAX_FILE_BYTES {
            return Err("Dato no disponible: escritura supera el límite de proyección".into());
        }
        BTreeMap::from([(path, Some(content.as_bytes().to_vec()))])
    } else {
        let patch = call
            .arguments
            .get("patch")
            .and_then(|value| value.as_str())
            .ok_or("Dato no disponible: patch sin contenido")?;
        match replay_patch(baseline, replayed, patch, effect_epoch) {
            Ok(updates) => updates,
            Err(reason) => {
                // Keep explicit affected paths unknown, rather than falling back
                // to a Git diff of unrelated working-tree modifications.
                for path in patch_paths(patch).into_iter().take(MAX_PATCH_FILES * 2) {
                    if replayed.contains_key(&path) || replayed.len() < MAX_CHANGED_FILES {
                        replayed.insert(
                            path,
                            ReplayedFile {
                                content: Err(reason.clone()),
                                last_sequence: sequence,
                                epoch: effect_epoch,
                            },
                        );
                    }
                }
                return Err(reason);
            }
        }
    };
    let new_count = updates
        .keys()
        .filter(|path| !replayed.contains_key(*path))
        .count();
    let retained = replayed
        .iter()
        .filter(|(path, _)| !updates.contains_key(*path))
        .map(|(_, file)| {
            file.content
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .map_or(0, Vec::len)
        })
        .sum::<usize>()
        .saturating_add(
            updates
                .values()
                .map(|content| content.as_ref().map_or(0, Vec::len))
                .sum(),
        );
    if replayed.len() + new_count > MAX_CHANGED_FILES || retained > MAX_BASELINE_BYTES {
        for path in updates.keys() {
            if replayed.contains_key(path) {
                replayed.insert(
                    path.clone(),
                    ReplayedFile {
                        content: Err(
                            "Dato no disponible: límite de archivos o bytes de replay".into()
                        ),
                        last_sequence: sequence,
                        epoch: effect_epoch,
                    },
                );
            }
        }
        return Err(
            "Cobertura limitada: se alcanzó el límite de archivos o bytes de cambios del chat"
                .into(),
        );
    }
    for (path, content) in updates {
        replayed.insert(
            path,
            ReplayedFile {
                content: Ok(content),
                last_sequence: sequence,
                epoch: effect_epoch,
            },
        );
    }
    Ok(())
}

#[derive(Default)]
struct PatchHunk {
    anchor: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    at_eof: bool,
}

fn patch_paths(patch: &str) -> BTreeSet<String> {
    if patch.len() > MAX_PATCH_BYTES {
        return BTreeSet::new();
    }
    patch
        .lines()
        .filter_map(|line| {
            [
                "*** Add File: ",
                "*** Delete File: ",
                "*** Update File: ",
                "*** Move to: ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
        })
        .filter_map(|path| normalized_path(path).ok())
        .take(MAX_PATCH_FILES * 2)
        .collect()
}

/// Pure replay of the host's own bounded patch dialect. Exact context/unique
/// anchors and newline rules match openide-harness; nothing touches disk here.
fn replay_patch(
    baseline: &Baseline,
    replayed: &BTreeMap<String, ReplayedFile>,
    patch: &str,
    effect_epoch: usize,
) -> Result<BTreeMap<String, Option<Vec<u8>>>, String> {
    let unavailable = || {
        "Dato no disponible: el patch no se puede reconstruir contra el baseline del chat"
            .to_owned()
    };
    if patch.is_empty() || patch.len() > MAX_PATCH_BYTES || patch.contains('\0') {
        return Err(unavailable());
    }
    let lines: Vec<_> = patch.lines().collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err(unavailable());
    }
    let mut updates = BTreeMap::<String, Option<Vec<u8>>>::new();
    let mut index = 1;
    let end = lines.len() - 1;
    let mut operations = 0;
    let mut hunk_count = 0;
    let is_marker = |line: &str| {
        line.starts_with("*** Add File: ")
            || line.starts_with("*** Delete File: ")
            || line.starts_with("*** Update File: ")
    };
    while index < end {
        operations += 1;
        if operations > MAX_PATCH_FILES {
            return Err(unavailable());
        }
        let line = lines[index];
        let (kind, raw_path) = ["*** Add File: ", "*** Delete File: ", "*** Update File: "]
            .iter()
            .enumerate()
            .find_map(|(kind, prefix)| line.strip_prefix(prefix).map(|path| (kind, path)))
            .ok_or_else(unavailable)?;
        let path = normalized_path(raw_path)?;
        let original = updates
            .get(&path)
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| file_before(baseline, replayed, &path, effect_epoch));
        // A confirmed Add carries its complete new content and the executor
        // verified absence. It can establish a new state after opaque effects,
        // just like a full write_file; Update/Delete still need an exact input.
        let original = if kind == 0 {
            original.unwrap_or(None)
        } else {
            original?
        };
        index += 1;
        let replacement = match kind {
            0 => {
                if original.is_some() {
                    return Err(unavailable());
                }
                let mut added = Vec::new();
                while index < end && !is_marker(lines[index]) {
                    added.push(lines[index].strip_prefix('+').ok_or_else(unavailable)?);
                    index += 1;
                }
                if added.is_empty() {
                    return Err(unavailable());
                }
                Some(format!("{}\n", added.join("\n")).into_bytes())
            }
            1 => {
                if original.is_none() {
                    return Err(unavailable());
                }
                None
            }
            _ => {
                let move_to = lines
                    .get(index)
                    .and_then(|line| line.strip_prefix("*** Move to: "))
                    .map(normalized_path)
                    .transpose()?;
                if move_to.is_some() {
                    index += 1;
                }
                let mut hunks = Vec::new();
                let mut active = PatchHunk::default();
                let mut has_active = false;
                while index < end && !is_marker(lines[index]) {
                    let line = lines[index];
                    if line == "@@" || line.starts_with("@@ ") {
                        if has_active {
                            hunks.push(active);
                            active = PatchHunk::default();
                        }
                        active.anchor = line.strip_prefix("@@ ").map(str::to_owned);
                        has_active = true;
                    } else if line == "*** End of File" {
                        if !has_active || active.at_eof {
                            return Err(unavailable());
                        }
                        active.at_eof = true;
                    } else {
                        if active.at_eof {
                            return Err(unavailable());
                        }
                        has_active = true;
                        let (prefix, text) = line.split_at_checked(1).ok_or_else(unavailable)?;
                        match prefix {
                            " " => {
                                active.old.push(text.into());
                                active.new.push(text.into());
                            }
                            "-" => active.old.push(text.into()),
                            "+" => active.new.push(text.into()),
                            _ => return Err(unavailable()),
                        }
                    }
                    index += 1;
                }
                if has_active {
                    hunks.push(active);
                }
                hunk_count += hunks.len();
                if hunks.is_empty() || hunk_count > MAX_PATCH_HUNKS {
                    return Err(unavailable());
                }
                let source = original
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .ok_or_else(unavailable)?;
                let replacement = apply_hunks(source, &hunks)?;
                if let Some(destination) = move_to {
                    let destination_original = updates
                        .get(&destination)
                        .cloned()
                        .map(Ok)
                        .unwrap_or_else(|| {
                            file_before(baseline, replayed, &destination, effect_epoch)
                        })?;
                    if path == destination || destination_original.is_some() {
                        return Err(unavailable());
                    }
                    updates.insert(destination, Some(replacement));
                    None
                } else {
                    Some(replacement)
                }
            }
        };
        if replacement
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_FILE_BYTES)
        {
            return Err(unavailable());
        }
        updates.insert(path, replacement);
    }
    Ok(updates)
}

fn apply_hunks(source: &str, hunks: &[PatchHunk]) -> Result<Vec<u8>, String> {
    let unavailable =
        || "Dato no disponible: el contexto del patch difiere del baseline o es ambiguo".to_owned();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let final_newline = source.is_empty() || source.ends_with('\n');
    let mut lines: Vec<String> = source
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).into())
        .collect();
    if source.ends_with('\n') {
        lines.pop();
    }
    if source.is_empty() {
        lines.clear();
    }
    let mut cursor = 0usize;
    for hunk in hunks {
        let from = if let Some(anchor) = &hunk.anchor {
            let positions: Vec<_> = lines
                .iter()
                .enumerate()
                .skip(cursor)
                .filter_map(|(index, line)| (line == anchor).then_some(index))
                .take(2)
                .collect();
            if positions.len() != 1 {
                return Err(unavailable());
            }
            positions[0] + 1
        } else {
            cursor
        };
        let start = if hunk.old.is_empty() {
            if hunk.anchor.is_some() {
                from
            } else {
                lines.len()
            }
        } else {
            unique_hunk_position(&lines, &hunk.old, from, hunk.at_eof).ok_or_else(unavailable)?
        };
        if hunk.at_eof && start + hunk.old.len() != lines.len() {
            return Err(unavailable());
        }
        lines.splice(start..start + hunk.old.len(), hunk.new.iter().cloned());
        cursor = start + hunk.new.len();
    }
    let mut result = lines.join(newline);
    if final_newline && !lines.is_empty() {
        result.push_str(newline);
    }
    if result.len() > MAX_FILE_BYTES {
        return Err(unavailable());
    }
    Ok(result.into_bytes())
}

/// KMP over exact lines keeps repeated-context patches linear in line count;
/// checking every full candidate slice would make replay quadratic.
fn unique_hunk_position(
    lines: &[String],
    pattern: &[String],
    from: usize,
    at_eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    let mut prefixes = vec![0usize; pattern.len()];
    let mut matched = 0usize;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefixes[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefixes[index] = matched;
    }
    matched = 0;
    let mut found = None;
    for (index, line) in lines.iter().enumerate().skip(from) {
        while matched > 0 && line != &pattern[matched] {
            matched = prefixes[matched - 1];
        }
        if line == &pattern[matched] {
            matched += 1;
        }
        if matched == pattern.len() {
            if !at_eof || index + 1 == lines.len() {
                if found.is_some() {
                    return None;
                }
                found = Some(index + 1 - pattern.len());
            }
            matched = prefixes[matched - 1];
        }
    }
    found
}

struct TemporaryDiff(PathBuf);
impl Drop for TemporaryDiff {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn confirmed_text_snapshot(
    path: &str,
    bytes: &[u8],
    line_budget: usize,
    byte_budget: usize,
) -> io::Result<(ReviewFile, bool, usize)> {
    if bytes.contains(&0) {
        return Err(invalid("Contenido binario no representable como texto"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("Contenido no UTF-8"))?;
    let mut file = ReviewFile {
        path: path.into(),
        status: "Contenido confirmado".into(),
        ..ReviewFile::default()
    };
    let mut used = 0usize;
    let mut truncated = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        if file.lines.len() >= line_budget || used >= byte_budget {
            truncated = true;
            break;
        }
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        let mut end = content.len().min(byte_budget.saturating_sub(used + 1));
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        let visible = &content[..end];
        file.lines.push(ReviewLine {
            kind: ReviewLineKind::Context,
            text: format!(" {visible}"),
            old_number: None,
            new_number: Some(index + 1),
        });
        used += visible.len() + 1;
        if end < content.len() {
            truncated = true;
            break;
        }
    }
    Ok((file, truncated, used))
}

fn snapshot_diff(
    storage_root: &Path,
    path: &str,
    before: &[u8],
    after: &[u8],
    byte_limit: usize,
) -> io::Result<(ReviewFile, bool, usize)> {
    snapshot_diff_until(
        storage_root,
        path,
        before,
        after,
        byte_limit,
        Instant::now() + PROJECTION_BUDGET,
    )
}

fn snapshot_diff_until(
    storage_root: &Path,
    path: &str,
    before: &[u8],
    after: &[u8],
    byte_limit: usize,
    deadline: Instant,
) -> io::Result<(ReviewFile, bool, usize)> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "se alcanzó el tiempo máximo del diff",
        ));
    }
    if before == after {
        return Ok((
            ReviewFile {
                path: path.into(),
                ..ReviewFile::default()
            },
            false,
            0,
        ));
    }
    if before.contains(&0)
        || after.contains(&0)
        || std::str::from_utf8(before).is_err()
        || std::str::from_utf8(after).is_err()
    {
        return Err(invalid(
            "diff textual no disponible para este archivo binario",
        ));
    }
    // Exact per-tool snapshots also work for chats without an initial
    // baseline, so their diff scratch directory may not exist yet.
    let _directory = crate::session_changes_editor::Directory::open_named(
        storage_root,
        "session-changes",
        true,
    )?
    .ok_or_else(|| invalid("No se pudo abrir el directorio privado del diff"))?;
    let parent = storage_root.join("session-changes");
    let temporary = TemporaryDiff(parent.join(format!(".diff-{}", uuid::Uuid::new_v4())));
    fs::create_dir(&temporary.0)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary.0, fs::Permissions::from_mode(0o700))?;
    }
    write_private(&temporary.0.join("before"), before)?;
    write_private(&temporary.0.join("after"), after)?;
    let (numstat, truncated, _) = git_output_until(
        &temporary.0,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "--",
            "before",
            "after",
        ],
        1024,
        deadline,
    )?;
    if truncated {
        return Err(invalid("totales del diff exceden su límite"));
    }
    let numstat = std::str::from_utf8(&numstat).map_err(io::Error::other)?;
    let mut columns = numstat.split('\t');
    let additions = columns
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid("Git no devolvió totales válidos"))?;
    let deletions = columns
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid("Git no devolvió totales válidos"))?;
    let (bytes, mut truncated, _) = git_output_until(
        &temporary.0,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--unified=3",
            "--",
            "before",
            "after",
        ],
        byte_limit,
        deadline,
    )?;
    let text = String::from_utf8_lossy(&bytes);
    let mut file = ReviewFile {
        path: path.into(),
        additions,
        deletions,
        ..ReviewFile::default()
    };
    let mut numbers: Option<(usize, usize)> = None;
    for line in text.lines() {
        if file.lines.len() == MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        let kind = if line.starts_with("@@ ") {
            ReviewLineKind::Hunk
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            ReviewLineKind::Meta
        } else if line.starts_with('+') {
            ReviewLineKind::Added
        } else if line.starts_with('-') {
            ReviewLineKind::Removed
        } else if line.starts_with(' ') {
            ReviewLineKind::Context
        } else {
            ReviewLineKind::Meta
        };
        if kind == ReviewLineKind::Hunk {
            let mut pieces = line.split_whitespace();
            let _ = pieces.next();
            let old = pieces
                .next()
                .and_then(|value| value.strip_prefix('-'))
                .and_then(|value| value.split(',').next())
                .and_then(|value| value.parse().ok());
            let new = pieces
                .next()
                .and_then(|value| value.strip_prefix('+'))
                .and_then(|value| value.split(',').next())
                .and_then(|value| value.parse().ok());
            numbers = old.zip(new);
        }
        let mut row = ReviewLine {
            kind,
            text: line.into(),
            old_number: None,
            new_number: None,
        };
        if let Some((old, new)) = numbers.as_mut() {
            match kind {
                ReviewLineKind::Added => {
                    row.new_number = Some(*new);
                    *new += 1;
                }
                ReviewLineKind::Removed => {
                    row.old_number = Some(*old);
                    *old += 1;
                }
                ReviewLineKind::Context => {
                    row.old_number = Some(*old);
                    row.new_number = Some(*new);
                    *old += 1;
                    *new += 1;
                }
                _ => {}
            }
        }
        // Git's temporary-file headers are implementation details; display the
        // session-relative path instead, without exposing the storage location.
        if line.starts_with("diff ") {
            row.text = format!("diff --git a/{path} b/{path}");
        } else if line.starts_with("--- ") {
            row.text = format!("--- a/{path}");
        } else if line.starts_with("+++ ") {
            row.text = format!("+++ b/{path}");
        }
        file.lines.push(row);
    }
    Ok((file, truncated, bytes.len()))
}

fn git_output(root: &Path, args: &[&str], limit: usize) -> io::Result<(Vec<u8>, bool, bool)> {
    git_output_until(root, args, limit, Instant::now() + GIT_TIMEOUT)
}

fn git_output_until(
    root: &Path,
    args: &[&str],
    limit: usize,
    outer_deadline: Instant,
) -> io::Result<(Vec<u8>, bool, bool)> {
    let deadline = outer_deadline.min(Instant::now() + GIT_TIMEOUT);
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "se alcanzó el tiempo máximo del diff",
        ));
    }
    let mut child = command_no_window("git")
        .args(args)
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped Git stdout");
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("openide-session-diff-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(limit.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = send.send(result);
        });
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let mut bytes = match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(error);
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git no respondió al preparar el diff del chat",
            ));
        }
    };
    let truncated = bytes.len() > limit;
    if truncated {
        let _ = child.kill();
        bytes.truncate(limit);
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Git no terminó de preparar el diff del chat",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = reader.join();
    Ok((bytes, truncated, status.success()))
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tool_diff_deadline_tests {
    use super::*;

    fn partial_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Vec<AgentEvent>) {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("store");
        let workspace = temp.path().join("project");
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(workspace.join("a-crowded")).unwrap();
        for index in 0..MAX_ENTRIES + 1 {
            fs::write(workspace.join(format!("a-crowded/{index:05}")), b"").unwrap();
        }
        fs::write(workspace.join("z-existing.txt"), b"private initial bytes\n").unwrap();
        let events = vec![AgentEvent::new(
            "session",
            1,
            AgentEventKind::SessionStarted {
                workspace: workspace.clone(),
            },
        )];
        let capture = capture_baseline(&storage, "session", &workspace, &events).unwrap();
        assert!(matches!(capture, BaselineCapture::Created(info) if !info.inventory_complete));
        (temp, storage, workspace, events)
    }

    fn persist_legacy_baseline(storage: &Path, workspace: &Path) {
        let mut baseline = read_baseline(storage, "session", &workspace.canonicalize().unwrap())
            .unwrap()
            .unwrap();
        baseline.directory_entries.clear();
        let payload = serde_json::to_vec(&baseline).unwrap();
        assert!(!String::from_utf8_lossy(&payload).contains("directory_entries"));
        let envelope = Envelope {
            sha256: digest(&payload),
            payload: baseline,
        };
        fs::write(
            baseline_path(storage, "session").unwrap(),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        assert!(
            read_baseline(storage, "session", &workspace.canonicalize().unwrap())
                .unwrap()
                .is_some()
        );
    }

    async fn execute_and_record(
        storage: &Path,
        workspace: &Path,
        events: &mut Vec<AgentEvent>,
        call: ToolCall,
    ) {
        use openide_harness::{LocalTools, ToolContext, ToolExecutor};
        events.push(AgentEvent::new(
            "session",
            events.len() as u64 + 1,
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
        ));
        let output = LocalTools::new()
            .with_write_access(true)
            .execute(
                &call,
                &ToolContext {
                    workspace: workspace.into(),
                    cancel: tokio_util::sync::CancellationToken::new(),
                },
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        events.push(AgentEvent::new(
            "session",
            events.len() as u64 + 1,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id,
                output: output.content,
                is_error: output.is_error,
            },
        ));
        let mut journal = Vec::new();
        for event in events {
            serde_json::to_writer(&mut journal, event).unwrap();
            journal.push(b'\n');
        }
        fs::write(storage.join("session.jsonl"), journal).unwrap();
    }

    #[tokio::test]
    async fn the_last_turn_is_compared_with_the_files_as_it_found_them() {
        let temp = tempfile::tempdir().unwrap();
        let (storage, workspace) = (temp.path().join("store"), temp.path().join("project"));
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("kept.txt"), b"start\n").unwrap();
        let mut events = vec![AgentEvent::new("session", 1, AgentEventKind::SessionStarted { workspace: workspace.clone() })];
        capture_baseline(&storage, "session", &workspace, &events).unwrap();
        let write = |id: &str, path: &str, content: &str| ToolCall {
            id: id.into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": path, "content": content}),
        };
        let turn = |events: &mut Vec<AgentEvent>| {
            events.push(AgentEvent::new("session", events.len() as u64 + 1, AgentEventKind::TurnStarted { prompt: "x".into() }));
        };
        turn(&mut events);
        execute_and_record(&storage, &workspace, &mut events, write("one", "kept.txt", "start\nfirst turn\n")).await;
        execute_and_record(&storage, &workspace, &mut events, write("two", "early.txt", "early\n")).await;
        turn(&mut events);
        execute_and_record(&storage, &workspace, &mut events, write("three", "kept.txt", "start\nfirst turn\nlast turn\n")).await;
        execute_and_record(&storage, &workspace, &mut events, write("four", "late.txt", "late\n")).await;

        let seen = |last_turn: bool| -> Vec<(String, usize, String)> {
            load_stored_changes(&storage, "session", &workspace, last_turn)
                .unwrap()
                .files
                .into_iter()
                .map(|change| (change.file.path, change.file.additions, change.file.status))
                .collect()
        };
        assert_eq!(
            seen(false),
            [("early.txt".into(), 1, "Nuevo".into()), ("kept.txt".into(), 2, "Modificado".into()), ("late.txt".into(), 1, "Nuevo".into())]
        );
        // What an earlier turn wrote is where the last one started from.
        assert_eq!(seen(true), [("kept.txt".into(), 1, "Modificado".into()), ("late.txt".into(), 1, "Nuevo".into())]);
    }

    #[tokio::test]
    async fn real_new_write_has_complete_diff_when_another_subtree_exhausted_the_baseline() {
        let (_temp, storage, workspace, mut events) = partial_fixture();
        let baseline = read_baseline(&storage, "session", &workspace.canonicalize().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(baseline.original("new.txt").unwrap(), None);
        assert_eq!(baseline.original("new-directory/new.txt").unwrap(), None);
        // Its name was observed but content was not captured: never infer that
        // an existing, unvisited file was absent.
        assert!(baseline.original("z-existing.txt").is_err());
        execute_and_record(&storage, &workspace, &mut events, ToolCall {
            id: "new".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"new.txt","content":"confirmed line\nsecond line\n"}),
        }).await;
        assert_eq!(
            fs::read_to_string(workspace.join("new.txt")).unwrap(),
            "confirmed line\nsecond line\n"
        );
        fs::write(workspace.join("new.txt"), b"unrelated later edit").unwrap();
        let diff =
            load_stored_tool_diff_projection(&storage, "session", &workspace, "new").unwrap();
        assert!(diff.coverage_notes.is_empty());
        assert!(diff.files[0].unavailable.is_none());
        assert_eq!(diff.files[0].file.status, "Nuevo");
        assert_eq!(diff.files[0].file.additions, 2);
        assert!(diff.files[0]
            .file
            .lines
            .iter()
            .any(|line| line.text == "+confirmed line"));
        assert!(!diff.files[0]
            .file
            .lines
            .iter()
            .any(|line| line.text.contains("unrelated")));
    }

    #[tokio::test]
    async fn legacy_partial_baseline_keeps_confirmed_written_text_without_claiming_additions() {
        let (_temp, storage, workspace, mut events) = partial_fixture();
        persist_legacy_baseline(&storage, &workspace);
        for (id, path) in [("created", "new.txt"), ("replaced", "z-existing.txt")] {
            execute_and_record(&storage, &workspace, &mut events, ToolCall {
                id: id.into(), name: "write_file".into(), arguments: serde_json::json!({"path":path,"content":"confirmed 🦀\nsecond line\n"}),
            }).await;
            let diff =
                load_stored_tool_diff_projection(&storage, "session", &workspace, id).unwrap();
            let file = &diff.files[0];
            assert_eq!(file.file.status, "Contenido confirmado");
            assert_eq!((file.file.additions, file.file.deletions), (0, 0));
            assert!(file
                .unavailable
                .as_ref()
                .unwrap()
                .contains("versión anterior"));
            assert_eq!(file.file.lines.len(), 2);
            assert!(file
                .file
                .lines
                .iter()
                .all(|line| line.kind == ReviewLineKind::Context && line.old_number.is_none()));
            assert_eq!(file.file.lines[0].text, " confirmed 🦀");
            assert!(!file
                .file
                .lines
                .iter()
                .any(|line| line.text.contains("private initial")));
            let entry = crate::chat_diff_ui::ToolDiffEntry::from_result(Ok(
                crate::session_tool_diff::load_stored_tool_diff(
                    &storage, "session", &workspace, id,
                )
                .unwrap(),
            ));
            assert_eq!(entry.files[0].lines[0].text, "confirmed 🦀");
            assert_eq!(
                entry.files[0].lines[0].kind,
                openide_ui::CodeDiffLineKind::Context
            );
        }
    }

    #[tokio::test]
    async fn confirmed_patch_add_proves_creation_even_for_a_legacy_partial_baseline() {
        let (_temp, storage, workspace, mut events) = partial_fixture();
        persist_legacy_baseline(&storage, &workspace);
        execute_and_record(&storage, &workspace, &mut events, ToolCall {
            id: "patch-add".into(), name: "apply_patch".into(), arguments: serde_json::json!({"patch":"*** Begin Patch\n*** Add File: new.txt\n+created by patch\n*** End Patch\n"}),
        }).await;
        let diff =
            load_stored_tool_diff_projection(&storage, "session", &workspace, "patch-add").unwrap();
        assert!(diff.files[0].unavailable.is_none());
        assert_eq!(diff.files[0].file.status, "Nuevo");
        assert_eq!(diff.files[0].file.additions, 1);
        assert!(diff.files[0]
            .file
            .lines
            .iter()
            .any(|line| line.text == "+created by patch"));
    }

    #[test]
    fn confirmed_snapshot_is_neutral_unicode_bounded_and_rejects_binary() {
        let (file, truncated, bytes) =
            confirmed_text_snapshot("new.txt", "🦀🦀\nnext\n".as_bytes(), 10, 7).unwrap();
        assert!(truncated);
        assert!(bytes <= 7);
        assert_eq!(file.lines[0].text, " 🦀");
        assert_eq!(file.lines[0].new_number, Some(1));
        assert_eq!((file.additions, file.deletions), (0, 0));
        let (file, truncated, _) =
            confirmed_text_snapshot("new.txt", b"one\ntwo\n", 1, 100).unwrap();
        assert!(truncated);
        assert_eq!(file.lines.len(), 1);
        assert!(confirmed_text_snapshot("binary", b"a\0b", 10, 100).is_err());
    }

    #[test]
    fn editor_history_actual_journal_bytes_respect_remaining_aggregate_budget() {
        let directory = tempfile::tempdir().unwrap();
        let event = AgentEvent::new(
            "session",
            1,
            AgentEventKind::SessionStarted {
                workspace: directory.path().into(),
            },
        );
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        fs::write(directory.path().join("session.jsonl"), &bytes).unwrap();
        let (journal, read) =
            read_stored_journal_with_budget(directory.path(), "session", (bytes.len() - 1) as u64)
                .unwrap();
        assert_eq!(read, bytes.len() as u64);
        assert!(matches!(journal, StoredJournal::Unavailable(_)));
        let (journal, read) =
            read_stored_journal_with_budget(directory.path(), "session", bytes.len() as u64)
                .unwrap();
        assert_eq!(read, bytes.len() as u64);
        assert!(matches!(journal,StoredJournal::Events(events) if events.len()==1));
    }

    #[test]
    fn tool_diff_expired_deadline_does_not_launch_git_or_return_partial_proof() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("session-changes")).unwrap();
        let error = snapshot_diff_until(
            directory.path(),
            "a",
            b"before",
            b"after",
            1000,
            Instant::now() - Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            fs::read_dir(directory.path().join("session-changes"))
                .unwrap()
                .count(),
            0
        );
    }
}
