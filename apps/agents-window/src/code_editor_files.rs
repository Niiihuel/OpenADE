//! Bounded background file I/O for editor documents.
//!
//! Loads may be superseded per tab. An admitted save is never superseded: its
//! response retains the snapshot identity, even when another tab is selected.
//! No file contents, filesystem metadata, or rope construction are read on
//! the UI thread. The host must still check response scopes before showing a
//! load, and apply saved snapshots to their original retained document.
//! Saves recheck disk identity and bytes immediately before atomic replacement.
//! This detects observed external changes, but is not a filesystem compare-and-
//! swap: a noncooperating writer can still change the target in the tiny window
//! between that final check and rename.

use openide_editor::{BufferSnapshot, EditorLimits, TextBuffer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

#[path = "code_editor_recovery.rs"]
mod recovery;
pub use recovery::RecoveredDraft;

pub const MAX_EDITABLE_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const EDITOR_FILE_QUEUE_CAPACITY: usize = 8;
pub type EditorFileDigest = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorFileScope {
    pub tab_id: u64,
    pub generation: u64,
    pub workspace: PathBuf,
    pub relative_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorFileOperation {
    Load,
    Save,
    SaveAs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorFileSubmitError {
    QueueFull,
    Unavailable,
}

impl fmt::Display for EditorFileSubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::QueueFull => {
                "Hay operaciones de archivos pendientes. Inténtalo de nuevo cuando terminen."
            }
            Self::Unavailable => "No está disponible el servicio de archivos del editor.",
        })
    }
}

impl std::error::Error for EditorFileSubmitError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorFileError {
    InvalidScope,
    SymlinkPath,
    NotRegularFile,
    TooLarge,
    BinaryFile,
    InvalidUtf8,
    ExternalChange,
    Buffer(String),
    Io {
        kind: io::ErrorKind,
        message: String,
    },
}

impl fmt::Display for EditorFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScope => {
                f.write_str("El archivo debe estar dentro de la carpeta del proyecto.")
            }
            Self::SymlinkPath => {
                f.write_str("Todavía no se pueden editar archivos mediante enlaces simbólicos.")
            }
            Self::NotRegularFile => {
                f.write_str("Este elemento no es un archivo de texto editable.")
            }
            Self::TooLarge => f.write_str("El archivo supera el límite de 16 MiB del editor."),
            Self::BinaryFile => {
                f.write_str("Los archivos binarios no se pueden editar como texto.")
            }
            Self::InvalidUtf8 => {
                f.write_str("El archivo no usa UTF-8 válido. Su contenido se conserva intacto.")
            }
            Self::ExternalChange => {
                f.write_str("El archivo cambió en el disco. Vuelve a cargarlo antes de guardar.")
            }
            Self::Buffer(message) => write!(f, "No se pudo preparar el documento: {message}"),
            Self::Io { message, .. } => write!(f, "No se pudo acceder al archivo: {message}"),
        }
    }
}

impl std::error::Error for EditorFileError {}

impl From<io::Error> for EditorFileError {
    fn from(error: io::Error) -> Self {
        Self::Io {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum EditorFileResponse {
    RecoveryLoaded {
        drafts: Vec<RecoveredDraft>,
        errors: Vec<String>,
        available: bool,
    },
    RecoveryStored {
        scope: EditorFileScope,
        token: u64,
        result: Result<(), String>,
    },
    Inspected {
        scope: EditorFileScope,
        result: Result<Option<(TextBuffer, EditorFileDigest)>, EditorFileError>,
    },
    SavedAs {
        scope: EditorFileScope,
        destination: PathBuf,
        snapshot: BufferSnapshot,
        canonical_workspace: PathBuf,
        canonical_path: PathBuf,
        digest: EditorFileDigest,
        durability_warning: Option<String>,
    },
    Loaded {
        scope: EditorFileScope,
        canonical_workspace: PathBuf,
        canonical_path: PathBuf,
        buffer: TextBuffer,
        digest: EditorFileDigest,
    },
    Saved {
        scope: EditorFileScope,
        canonical_workspace: PathBuf,
        canonical_path: PathBuf,
        document_id: u64,
        revision: u64,
        snapshot: BufferSnapshot,
        digest: EditorFileDigest,
        /// Bytes have been committed, but synchronizing the directory failed.
        /// This is a successful save with a durability warning, not a retryable
        /// failure that would obscure the new disk digest.
        durability_warning: Option<String>,
    },
    Failed {
        scope: EditorFileScope,
        operation: EditorFileOperation,
        document_id: Option<u64>,
        revision: Option<u64>,
        error: EditorFileError,
    },
}

enum FileRequest {
    InitRecovery {
        root: PathBuf,
    },
    PersistRecovery {
        scope: EditorFileScope,
        base_digest: Option<EditorFileDigest>,
        snapshot: Option<BufferSnapshot>,
        token: u64,
    },
    Inspect {
        scope: EditorFileScope,
        known_digest: Option<EditorFileDigest>,
    },
    SaveAs {
        scope: EditorFileScope,
        destination: PathBuf,
        snapshot: BufferSnapshot,
        storage_root: PathBuf,
    },
    Load {
        scope: EditorFileScope,
        token: u64,
    },
    Save {
        scope: EditorFileScope,
        snapshot: BufferSnapshot,
        expected_digest: EditorFileDigest,
        storage_root: Option<PathBuf>,
    },
}

type LatestLoads = Arc<Mutex<HashMap<u64, u64>>>;

#[derive(Debug)]
pub struct EditorFileWorker {
    requests: mpsc::SyncSender<FileRequest>,
    responses: mpsc::Receiver<EditorFileResponse>,
    latest_loads: LatestLoads,
    next_load_token: AtomicU64,
}

impl EditorFileWorker {
    pub fn new() -> io::Result<Self> {
        let (requests, pending) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let (completed, responses) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let latest_loads = Arc::new(Mutex::new(HashMap::new()));
        let worker_loads = Arc::clone(&latest_loads);
        std::thread::Builder::new()
            .name("openide-editor-files".into())
            .spawn(move || {
                run_file_worker(pending, completed, worker_loads);
            })?;
        Ok(Self {
            requests,
            responses,
            latest_loads,
            next_load_token: AtomicU64::new(1),
        })
    }

    pub fn try_load(&self, scope: EditorFileScope) -> Result<(), EditorFileSubmitError> {
        let token = self.next_load_token.fetch_add(1, Ordering::Relaxed);
        let mut latest = self
            .latest_loads
            .lock()
            .map_err(|_| EditorFileSubmitError::Unavailable)?;
        let tab_id = scope.tab_id;
        self.requests
            .try_send(FileRequest::Load { scope, token })
            .map_err(submit_error)?;
        // Keep the lock across admission so the worker cannot inspect an
        // admitted request before its cancellation token has been published.
        latest.insert(tab_id, token);
        Ok(())
    }

    pub fn try_init_recovery(&self, root: PathBuf) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::InitRecovery { root })
            .map_err(submit_error)
    }
    pub fn try_persist_recovery(
        &self,
        scope: EditorFileScope,
        base_digest: Option<EditorFileDigest>,
        snapshot: Option<BufferSnapshot>,
        token: u64,
    ) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::PersistRecovery {
                scope,
                base_digest,
                snapshot,
                token,
            })
            .map_err(submit_error)
    }
    pub fn try_inspect(
        &self,
        scope: EditorFileScope,
        known_digest: Option<EditorFileDigest>,
    ) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::Inspect {
                scope,
                known_digest,
            })
            .map_err(submit_error)
    }
    pub fn try_save_as(
        &self,
        scope: EditorFileScope,
        destination: PathBuf,
        snapshot: BufferSnapshot,
        storage_root: PathBuf,
    ) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::SaveAs {
                scope,
                destination,
                snapshot,
                storage_root,
            })
            .map_err(submit_error)
    }

    /// No-history saves exist only for isolated file-worker tests.
    #[cfg(test)]
    pub fn try_save(
        &self,
        scope: EditorFileScope,
        snapshot: BufferSnapshot,
        expected_digest: EditorFileDigest,
    ) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::Save {
                scope,
                snapshot,
                expected_digest,
                storage_root: None,
            })
            .map_err(submit_error)
    }

    /// The worker publishes durable boundaries for all matching chat baselines
    /// before its atomic rename. Admission and response polling never do I/O.
    pub fn try_save_with_history(
        &self,
        scope: EditorFileScope,
        snapshot: BufferSnapshot,
        expected_digest: EditorFileDigest,
        storage_root: PathBuf,
    ) -> Result<(), EditorFileSubmitError> {
        self.requests
            .try_send(FileRequest::Save {
                scope,
                snapshot,
                expected_digest,
                storage_root: Some(storage_root),
            })
            .map_err(submit_error)
    }

    pub fn try_recv(&self) -> Option<EditorFileResponse> {
        self.responses.try_recv().ok()
    }

    pub fn cancel_loads(&self, tab_id: u64) {
        if let Ok(mut latest) = self.latest_loads.lock() {
            latest.remove(&tab_id);
        }
    }

    pub fn cancel_all_loads(&self) {
        if let Ok(mut latest) = self.latest_loads.lock() {
            latest.clear();
        }
    }
}

fn submit_error(error: mpsc::TrySendError<FileRequest>) -> EditorFileSubmitError {
    match error {
        mpsc::TrySendError::Full(_) => EditorFileSubmitError::QueueFull,
        mpsc::TrySendError::Disconnected(_) => EditorFileSubmitError::Unavailable,
    }
}

fn run_file_worker(
    pending: mpsc::Receiver<FileRequest>,
    completed: mpsc::SyncSender<EditorFileResponse>,
    latest: LatestLoads,
) {
    let mut recovery_store = None;
    let mut inspection_cache = HashMap::new();
    while let Ok(request) = pending.recv() {
        let response = match request {
            FileRequest::InitRecovery { root } => match recovery::RecoveryStore::open(root) {
                Ok(store) => {
                    let (drafts, errors) = store.load();
                    recovery_store = Some(store);
                    EditorFileResponse::RecoveryLoaded {
                        drafts,
                        errors,
                        available: true,
                    }
                }
                Err(error) => EditorFileResponse::RecoveryLoaded {
                    drafts: Vec::new(),
                    errors: vec![error.to_string()],
                    available: false,
                },
            },
            FileRequest::PersistRecovery {
                scope,
                base_digest,
                snapshot,
                token,
            } => {
                let result = recovery_store
                    .as_ref()
                    .ok_or_else(|| "La recuperación del editor no está disponible".to_owned())
                    .and_then(|store| {
                        match snapshot {
                            Some(snapshot) => store.store(&scope, base_digest, &snapshot),
                            None => store.remove(&scope),
                        }
                        .map_err(|e| e.to_string())
                    });
                EditorFileResponse::RecoveryStored {
                    scope,
                    token,
                    result,
                }
            }
            FileRequest::Inspect {
                scope,
                known_digest,
            } => {
                let result = inspect_file(&scope, known_digest, &mut inspection_cache);
                EditorFileResponse::Inspected { scope, result }
            }
            FileRequest::SaveAs {
                scope,
                destination,
                snapshot,
                storage_root,
            } => match create_file_with_history(&scope, &destination, &snapshot, &storage_root) {
                Ok(saved) => EditorFileResponse::SavedAs {
                    scope,
                    destination,
                    snapshot,
                    canonical_workspace: saved.canonical_workspace,
                    canonical_path: saved.canonical_path,
                    digest: saved.digest,
                    durability_warning: saved.durability_warning,
                },
                Err(error) => EditorFileResponse::Failed {
                    scope,
                    operation: EditorFileOperation::SaveAs,
                    document_id: Some(snapshot.document_id()),
                    revision: Some(snapshot.revision()),
                    error,
                },
            },
            FileRequest::Load { scope, token } => {
                if !load_is_current(&latest, scope.tab_id, token) {
                    continue;
                }
                let response = load_response(scope.clone());
                let Ok(mut loads) = latest.lock() else {
                    continue;
                };
                if loads.get(&scope.tab_id) != Some(&token) {
                    continue;
                }
                loads.remove(&scope.tab_id);
                drop(loads);
                response
            }
            FileRequest::Save {
                scope,
                snapshot,
                expected_digest,
                storage_root,
            } => save_response(scope, snapshot, expected_digest, storage_root.as_deref()),
        };
        // Backpressure caps completed buffers as well as pending jobs. If the
        // UI closes, continue draining already admitted saves even though it
        // can no longer receive their responses.
        let _ = completed.send(response);
    }
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
type InspectionCache =
    HashMap<(PathBuf, PathBuf), (FileStamp, EditorFileDigest, std::time::Instant)>;
fn inspect_file(
    scope: &EditorFileScope,
    known: Option<EditorFileDigest>,
    cache: &mut InspectionCache,
) -> Result<Option<(TextBuffer, EditorFileDigest)>, EditorFileError> {
    let metadata = ResolvedFile::new(scope)?.open_target()?.metadata()?;
    let stamp = FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    };
    let key = (scope.workspace.clone(), scope.relative_path.clone());
    if cache.get(&key).is_some_and(|(old, digest, at)| {
        old == &stamp && known == Some(*digest) && at.elapsed() < std::time::Duration::from_secs(30)
    }) {
        return Ok(None);
    }
    let loaded = load_file(scope)?;
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(key, (stamp, loaded.digest, std::time::Instant::now()));
    Ok((known != Some(loaded.digest)).then_some((loaded.buffer, loaded.digest)))
}

fn load_is_current(latest: &LatestLoads, tab_id: u64, token: u64) -> bool {
    latest
        .lock()
        .is_ok_and(|loads| loads.get(&tab_id) == Some(&token))
}

fn load_response(scope: EditorFileScope) -> EditorFileResponse {
    match load_file(&scope) {
        Ok(loaded) => EditorFileResponse::Loaded {
            scope,
            canonical_workspace: loaded.canonical_workspace,
            canonical_path: loaded.canonical_path,
            buffer: loaded.buffer,
            digest: loaded.digest,
        },
        Err(error) => EditorFileResponse::Failed {
            scope,
            operation: EditorFileOperation::Load,
            document_id: None,
            revision: None,
            error,
        },
    }
}

fn save_response(
    scope: EditorFileScope,
    snapshot: BufferSnapshot,
    expected_digest: EditorFileDigest,
    storage_root: Option<&Path>,
) -> EditorFileResponse {
    let outcome = match storage_root {
        Some(storage_root) => {
            save_file_with_history(&scope, &snapshot, expected_digest, storage_root)
        }
        #[cfg(test)]
        None => save_file(&scope, &snapshot, expected_digest),
        #[cfg(not(test))]
        None => {
            Err(io::Error::other("Falta el registro histórico para guardar de forma segura").into())
        }
    };
    match outcome {
        Ok(saved) => EditorFileResponse::Saved {
            scope,
            canonical_workspace: saved.canonical_workspace,
            canonical_path: saved.canonical_path,
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
            snapshot,
            digest: saved.digest,
            durability_warning: saved.durability_warning,
        },
        Err(error) => EditorFileResponse::Failed {
            scope,
            operation: EditorFileOperation::Save,
            document_id: Some(snapshot.document_id()),
            revision: Some(snapshot.revision()),
            error,
        },
    }
}

#[derive(Debug)]
struct LoadedFile {
    canonical_workspace: PathBuf,
    canonical_path: PathBuf,
    buffer: TextBuffer,
    digest: EditorFileDigest,
}

#[derive(Debug)]
struct SavedFile {
    canonical_workspace: PathBuf,
    canonical_path: PathBuf,
    digest: EditorFileDigest,
    durability_warning: Option<String>,
}

fn load_file(scope: &EditorFileScope) -> Result<LoadedFile, EditorFileError> {
    let resolved = ResolvedFile::new(scope)?;
    let bytes = read_bounded(resolved.open_target()?)?;
    if bytes
        .iter()
        .any(|byte| *byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c))
    {
        return Err(EditorFileError::BinaryFile);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| EditorFileError::InvalidUtf8)?;
    let buffer = TextBuffer::with_limits(
        text,
        EditorLimits {
            max_document_bytes: MAX_EDITABLE_FILE_BYTES,
            ..EditorLimits::default()
        },
    )
    .map_err(|error| EditorFileError::Buffer(error.to_string()))?;
    Ok(LoadedFile {
        canonical_workspace: resolved.canonical_workspace,
        canonical_path: resolved.canonical_path,
        buffer,
        digest: digest_bytes(&bytes),
    })
}

fn read_bounded(file: File) -> Result<Vec<u8>, EditorFileError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(EditorFileError::NotRegularFile);
    }
    if metadata.len() > MAX_EDITABLE_FILE_BYTES as u64 {
        return Err(EditorFileError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_EDITABLE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EDITABLE_FILE_BYTES {
        return Err(EditorFileError::TooLarge);
    }
    Ok(bytes)
}

fn digest_bytes(bytes: &[u8]) -> EditorFileDigest {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod review_write_tests {
    use super::*;

    #[test]
    fn guarded_review_checks_again_after_callback_for_replace_delete_and_create() {
        for (expected, replacement) in [
            (Some("after"), Some("before")),
            (Some("after"), None),
            (None, Some("before")),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = temp.path().join("project");
            let storage = temp.path().join("store");
            fs::create_dir(&workspace).unwrap();
            fs::create_dir(&storage).unwrap();
            let path = workspace.join("file.txt");
            if let Some(expected) = expected {
                fs::write(&path, expected).unwrap();
            }
            let guard =
                crate::session_changes_editor::lock_workspace(&storage, &workspace).unwrap();
            let result = write_review_file_guarded(
                &guard,
                Path::new("file.txt"),
                expected,
                replacement,
                || fs::write(&path, "concurrent local edit"),
            );
            assert!(
                result.is_err(),
                "concurrent write must win for {expected:?} -> {replacement:?}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "concurrent local edit");
            assert_eq!(
                fs::read_dir(&workspace).unwrap().count(),
                1,
                "temporary files cleaned up"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn guarded_review_rejects_symlinks_and_preserves_existing_file_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        let storage = temp.path().join("store");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&storage).unwrap();
        let path = workspace.join("file.txt");
        fs::write(&path, "after").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let guard = crate::session_changes_editor::lock_workspace(&storage, &workspace).unwrap();
        write_review_file_guarded(
            &guard,
            Path::new("file.txt"),
            Some("after"),
            Some("before"),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        symlink(&outside, workspace.join("linked.txt")).unwrap();
        assert!(
            write_review_file_guarded(
                &guard,
                Path::new("linked.txt"),
                Some("outside"),
                None,
                || Ok(())
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), "outside");
    }
}

#[cfg(test)]
fn save_file(
    scope: &EditorFileScope,
    snapshot: &BufferSnapshot,
    expected_digest: EditorFileDigest,
) -> Result<SavedFile, EditorFileError> {
    save_file_using(scope, snapshot, expected_digest, write_atomic_snapshot)
}

#[cfg(test)]
fn save_file_using(
    scope: &EditorFileScope,
    snapshot: &BufferSnapshot,
    expected_digest: EditorFileDigest,
    write_snapshot: impl FnOnce(
        &mut TemporaryFile,
        &BufferSnapshot,
    ) -> Result<EditorFileDigest, EditorFileError>,
) -> Result<SavedFile, EditorFileError> {
    save_file_commit_using(scope, snapshot, expected_digest, write_snapshot, || Ok(()))
}

fn save_file_with_history(
    scope: &EditorFileScope,
    snapshot: &BufferSnapshot,
    expected_digest: EditorFileDigest,
    storage_root: &Path,
) -> Result<SavedFile, EditorFileError> {
    // Validate document scope before creating any history infrastructure.
    let resolved = ResolvedFile::new(scope)?;
    let guard =
        crate::session_changes_editor::lock_workspace(storage_root, &resolved.canonical_workspace)?;
    save_file_commit_using(
        scope,
        snapshot,
        expected_digest,
        write_atomic_snapshot,
        || {
            crate::session_changes_editor::mark_workspace_save(&guard)
                .map_err(EditorFileError::from)
        },
    )
}

fn save_file_commit_using(
    scope: &EditorFileScope,
    snapshot: &BufferSnapshot,
    expected_digest: EditorFileDigest,
    write_snapshot: impl FnOnce(
        &mut TemporaryFile,
        &BufferSnapshot,
    ) -> Result<EditorFileDigest, EditorFileError>,
    before_commit: impl FnOnce() -> Result<(), EditorFileError>,
) -> Result<SavedFile, EditorFileError> {
    if snapshot.byte_count() > MAX_EDITABLE_FILE_BYTES {
        return Err(EditorFileError::TooLarge);
    }
    let resolved = ResolvedFile::new(scope)?;
    let original = resolved.open_target()?;
    let original_metadata = original.metadata()?;
    if original_metadata.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "El archivo es de solo lectura; utiliza Guardar como",
        )
        .into());
    }
    verify_write_access(&resolved)?;
    if digest_bytes(&read_bounded(original.try_clone()?)?) != expected_digest {
        return Err(EditorFileError::ExternalChange);
    }
    let mut temporary = resolved.create_temporary()?;
    temporary
        .file
        .set_permissions(original_metadata.permissions())?;
    let digest = write_snapshot(&mut temporary, snapshot)?;
    preserve_metadata(&original, &original_metadata, &temporary.file)?;
    temporary.file.sync_all()?;
    // Every boundary must be durable before the actual file replacement.
    // Then revalidate scope/content, because history enumeration can take time.
    before_commit()?;
    resolved.assert_still_bound(scope)?;
    let current = resolved.open_target()?;
    let current_metadata = current.metadata()?;
    if !same_target(&original_metadata, &current_metadata)
        || digest_bytes(&read_bounded(current)?) != expected_digest
    {
        return Err(EditorFileError::ExternalChange);
    }
    temporary.commit(&resolved)?;
    let durability_warning = resolved.sync_parent().err().map(|error| {
        format!("El archivo se guardó, pero no se pudo sincronizar la carpeta: {error}")
    });
    Ok(SavedFile {
        canonical_workspace: resolved.canonical_workspace,
        canonical_path: resolved.canonical_path,
        digest,
        durability_warning,
    })
}

/// Whether [`write_review_file_guarded`] would write exactly these bytes.
pub(crate) fn review_text_round_trips(bytes: &[u8]) -> bool {
    let mut written = Vec::with_capacity(bytes.len());
    bytes.len() <= MAX_EDITABLE_FILE_BYTES
        && std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| TextBuffer::new(text).ok())
            .is_some_and(|buffer| buffer.snapshot().write_to(&mut written).is_ok())
        && written == bytes
}

/// Worker-only review write while the caller retains the workspace lock.
/// Shares the editor's anchored paths, metadata preservation and last-moment
/// identity/content checks. Creation is exclusive; deletion never follows links.
pub(crate) fn write_review_file_guarded(
    guard: &crate::session_changes_editor::WorkspaceGuard,
    relative_path: &Path,
    expected: Option<&str>,
    replacement: Option<&str>,
    before_commit: impl FnOnce() -> io::Result<()>,
) -> Result<Option<String>, EditorFileError> {
    let scope = EditorFileScope {
        tab_id: 0,
        generation: 0,
        workspace: guard.workspace().to_path_buf(),
        relative_path: relative_path.to_path_buf(),
    };
    if expected.is_some_and(|s| s.len() > MAX_EDITABLE_FILE_BYTES)
        || replacement.is_some_and(|s| s.len() > MAX_EDITABLE_FILE_BYTES)
    {
        return Err(EditorFileError::TooLarge);
    }
    if let (Some(expected), Some(replacement)) = (expected, replacement) {
        let snapshot = TextBuffer::new(replacement)
            .map_err(|error| EditorFileError::Buffer(error.to_string()))?
            .snapshot();
        return save_file_commit_using(
            &scope,
            &snapshot,
            digest_bytes(expected.as_bytes()),
            write_atomic_snapshot,
            || {
                crate::session_changes_editor::mark_workspace_save(guard)?;
                before_commit().map_err(EditorFileError::from)
            },
        )
        .map(|saved| saved.durability_warning);
    }
    let resolved = ResolvedFile::new(&scope)?;
    let observed = match resolved.open_target() {
        Ok(file) => Some(file),
        Err(EditorFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => None,
        Err(error) => return Err(error),
    };
    if observed.is_some() != expected.is_some() {
        return Err(EditorFileError::ExternalChange);
    }
    if let (Some(original), Some(expected)) = (&observed, expected) {
        if digest_bytes(&read_bounded(original.try_clone()?)?) != digest_bytes(expected.as_bytes())
        {
            return Err(EditorFileError::ExternalChange);
        }
        if original.metadata()?.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "El archivo es de solo lectura",
            )
            .into());
        }
        verify_write_access(&resolved)?;
    }
    let mut staged = replacement
        .map(|text| -> Result<TemporaryFile, EditorFileError> {
            let snapshot = TextBuffer::new(text)
                .map_err(|error| EditorFileError::Buffer(error.to_string()))?
                .snapshot();
            let mut temporary = resolved.create_temporary()?;
            write_atomic_snapshot(&mut temporary, &snapshot)?;
            Ok(temporary)
        })
        .transpose()?;
    crate::session_changes_editor::mark_workspace_save(guard)?;
    before_commit()?;
    resolved.assert_still_bound(&scope)?;
    if let (Some(original), Some(expected)) = (&observed, expected) {
        let current = resolved.open_target()?;
        if !same_target(&original.metadata()?, &current.metadata()?)
            || digest_bytes(&read_bounded(current)?) != digest_bytes(expected.as_bytes())
        {
            return Err(EditorFileError::ExternalChange);
        }
    }
    match (expected, replacement) {
        (Some(_), None) => {
            #[cfg(unix)]
            {
                let name = c_name(&resolved.name)?;
                if unsafe { libc::unlinkat(resolved.parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }
            #[cfg(not(unix))]
            fs::remove_file(&resolved.canonical_path)?;
        }
        (None, Some(_)) => {
            let temporary = staged.as_mut().expect("replacement staged");
            #[cfg(unix)]
            {
                let from = c_name(&temporary.name)?;
                let to = c_name(&resolved.name)?;
                if unsafe {
                    libc::linkat(
                        temporary.parent.as_raw_fd(),
                        from.as_ptr(),
                        resolved.parent.as_raw_fd(),
                        to.as_ptr(),
                        0,
                    )
                } != 0
                {
                    return Err(io::Error::last_os_error().into());
                }
            }
            #[cfg(not(unix))]
            fs::hard_link(&temporary.path, &resolved.canonical_path)?;
        }
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => unreachable!("ordinary save handled above"),
    }
    Ok(resolved.sync_parent().err().map(|error| {
        format!("El cambio se guardó, pero no se pudo sincronizar la carpeta: {error}")
    }))
}

fn verify_write_access(target: &ResolvedFile) -> Result<(), EditorFileError> {
    #[cfg(unix)]
    {
        let name = c_name(&target.name)?;
        // Opening without O_TRUNC checks filesystem ACLs as well as mode bits,
        // without changing content or relying on access(2)'s real-ID semantics.
        let fd = unsafe {
            libc::openat(
                target.parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(EditorFileError::NotRegularFile);
        }
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .write(true)
            .open(&target.canonical_path)?;
    }
    Ok(())
}

fn preserve_metadata(
    original: &File,
    metadata: &Metadata,
    staged: &File,
) -> Result<(), EditorFileError> {
    #[cfg(unix)]
    {
        let stage_metadata = staged.metadata()?;
        if (stage_metadata.uid(), stage_metadata.gid()) != (metadata.uid(), metadata.gid()) {
            // Never silently change the owner/group through atomic replacement.
            if unsafe { libc::fchown(staged.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
    }
    staged.set_permissions(metadata.permissions())?;
    #[cfg(target_os = "linux")]
    {
        // ACLs and user metadata live in xattrs on Linux. Refuse the save if
        // they cannot be copied; leaving the original intact is preferable to
        // silently widening access or losing metadata.
        let size = unsafe { libc::flistxattr(original.as_raw_fd(), std::ptr::null_mut(), 0) };
        if size < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTSUP) {
                return Ok(());
            }
            return Err(error.into());
        }
        if size > 65536 {
            return Err(io::Error::other("Demasiados atributos del archivo").into());
        }
        let mut names = vec![0u8; size as usize];
        if size > 0
            && unsafe {
                libc::flistxattr(original.as_raw_fd(), names.as_mut_ptr().cast(), names.len())
            } != size
        {
            return Err(io::Error::other(
                "Los atributos del archivo cambiaron durante el guardado",
            )
            .into());
        }
        for name in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let name = std::ffi::CString::new(name).map_err(|_| EditorFileError::InvalidScope)?;
            let len = unsafe {
                libc::fgetxattr(original.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0)
            };
            if len < 0 {
                return Err(io::Error::last_os_error().into());
            }
            if len > 65536 {
                return Err(io::Error::other("Atributo de archivo demasiado grande").into());
            }
            let mut value = vec![0u8; len as usize];
            if unsafe {
                libc::fgetxattr(
                    original.as_raw_fd(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            } != len
            {
                return Err(io::Error::other("Un atributo cambió durante el guardado").into());
            }
            if unsafe {
                libc::fsetxattr(
                    staged.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error().into());
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = original;
    Ok(())
}

// Creation is an exclusive publish, never a check-then-overwriting rename.
// This also implements explicit recovery of a deleted file through Save As.
fn create_file_with_history(
    scope: &EditorFileScope,
    destination: &Path,
    snapshot: &BufferSnapshot,
    storage_root: &Path,
) -> Result<SavedFile, EditorFileError> {
    if snapshot.byte_count() > MAX_EDITABLE_FILE_BYTES {
        return Err(EditorFileError::TooLarge);
    }
    let target_scope = EditorFileScope {
        relative_path: destination.to_path_buf(),
        ..scope.clone()
    };
    let resolved = ResolvedFile::new(&target_scope)?;
    match resolved.open_target() {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Ya existe un archivo en esa ruta; elige otro nombre",
            )
            .into());
        }
        Err(EditorFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => {}
        Err(error) => return Err(error),
    }
    let guard =
        crate::session_changes_editor::lock_workspace(storage_root, &resolved.canonical_workspace)?;
    let mut temporary = resolved.create_temporary()?;
    let digest = write_atomic_snapshot(&mut temporary, snapshot)?;
    crate::session_changes_editor::mark_workspace_save(&guard)?;
    resolved.assert_still_bound(&target_scope)?;
    #[cfg(unix)]
    {
        let from = c_name(&temporary.name)?;
        let to = c_name(&resolved.name)?;
        // SAFETY: both names and the anchored parent descriptor remain valid.
        // linkat fails EEXIST even if another writer wins after our check.
        if unsafe {
            libc::linkat(
                temporary.parent.as_raw_fd(),
                from.as_ptr(),
                resolved.parent.as_raw_fd(),
                to.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    #[cfg(not(unix))]
    fs::hard_link(&temporary.path, &resolved.canonical_path)?;
    let durability_warning = resolved
        .sync_parent()
        .err()
        .map(|e| format!("El archivo se creó, pero no se pudo sincronizar la carpeta: {e}"));
    // Drop removes only the private staging name; the published hard link stays.
    drop(temporary);
    Ok(SavedFile {
        canonical_workspace: resolved.canonical_workspace,
        canonical_path: resolved.canonical_path,
        digest,
        durability_warning,
    })
}

fn write_atomic_snapshot(
    temporary: &mut TemporaryFile,
    snapshot: &BufferSnapshot,
) -> Result<EditorFileDigest, EditorFileError> {
    let mut writer = DigestWriter {
        file: &mut temporary.file,
        digest: Sha256::new(),
    };
    snapshot.write_to(&mut writer)?;
    let digest = writer.digest.finalize().into();
    temporary.file.sync_all()?;
    Ok(digest)
}

struct DigestWriter<'a> {
    file: &'a mut File,
    digest: Sha256,
}

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.digest.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn relative_components(path: &Path) -> Result<Vec<std::ffi::OsString>, EditorFileError> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => components.push(name.to_os_string()),
            Component::CurDir => {}
            _ => return Err(EditorFileError::InvalidScope),
        }
    }
    if components.is_empty() {
        return Err(EditorFileError::InvalidScope);
    }
    Ok(components)
}

#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::MetadataExt,
    io::{AsRawFd, FromRawFd},
};

struct ResolvedFile {
    canonical_workspace: PathBuf,
    canonical_path: PathBuf,
    parent: File,
    name: std::ffi::OsString,
}

impl ResolvedFile {
    fn new(scope: &EditorFileScope) -> Result<Self, EditorFileError> {
        if !scope.workspace.is_absolute() {
            return Err(EditorFileError::InvalidScope);
        }
        let components = relative_components(&scope.relative_path)?;
        let canonical_workspace = fs::canonicalize(&scope.workspace)?;
        // The workspace itself is part of the authority, not another link to
        // follow anew on each save. Require the canonical root provided by the
        // host so swapping a workspace alias cannot redirect a document save.
        if canonical_workspace != scope.workspace {
            return Err(EditorFileError::SymlinkPath);
        }
        let mut canonical_path = canonical_workspace.clone();
        for component in &components {
            canonical_path.push(component);
        }
        let mut parent = open_directory(&canonical_workspace)?;
        for component in &components[..components.len() - 1] {
            parent = open_child_directory(&parent, component)?;
        }
        Ok(Self {
            canonical_workspace,
            canonical_path,
            parent,
            name: components.last().expect("validated nonempty path").clone(),
        })
    }

    fn open_target(&self) -> Result<File, EditorFileError> {
        open_regular_file(&self.parent, &self.name, &self.canonical_path)
    }

    fn create_temporary(&self) -> Result<TemporaryFile, EditorFileError> {
        let name = std::ffi::OsString::from(format!(".openide-save-{}.tmp", uuid::Uuid::new_v4()));
        let path = self.canonical_path.with_file_name(&name);
        let parent = self.parent.try_clone()?;
        let file = create_sibling(&self.parent, &name, &path)?;
        Ok(TemporaryFile {
            file,
            parent,
            name,
            path,
            committed: false,
        })
    }

    fn assert_still_bound(&self, scope: &EditorFileScope) -> Result<(), EditorFileError> {
        let current = Self::new(scope)?;
        if current.canonical_workspace != self.canonical_workspace
            || !same_directory(&self.parent.metadata()?, &current.parent.metadata()?)
        {
            return Err(EditorFileError::ExternalChange);
        }
        Ok(())
    }

    fn sync_parent(&self) -> io::Result<()> {
        self.parent.sync_all()
    }
}

struct TemporaryFile {
    file: File,
    parent: File,
    name: std::ffi::OsString,
    path: PathBuf,
    committed: bool,
}

impl TemporaryFile {
    fn commit(&mut self, target: &ResolvedFile) -> Result<(), EditorFileError> {
        rename_sibling(&self.parent, &self.name, &self.path, target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.committed {
            remove_sibling(&self.parent, &self.name, &self.path);
        }
    }
}

#[cfg(unix)]
fn c_name(name: &std::ffi::OsStr) -> Result<std::ffi::CString, EditorFileError> {
    std::ffi::CString::new(name.as_bytes()).map_err(|_| EditorFileError::InvalidScope)
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<File, EditorFileError> {
    // Walk from the filesystem root, never following a substituted ancestor.
    let root = std::ffi::CString::new("/").expect("literal has no NUL");
    // SAFETY: root is a valid terminated string; ownership of a successful fd
    // is transferred immediately to File exactly once.
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: open returned an owned, valid descriptor.
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = open_child_directory(&directory, name)?,
            _ => return Err(EditorFileError::InvalidScope),
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_child_directory(parent: &File, name: &std::ffi::OsStr) -> Result<File, EditorFileError> {
    open_child(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )
}

#[cfg(unix)]
fn open_child(
    parent: &File,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
) -> Result<File, EditorFileError> {
    let name = c_name(name)?;
    // SAFETY: parent descriptor and terminated relative component are valid.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstatat initializes stat only on success. AT_SYMLINK_NOFOLLOW
        // inspects a link itself without reading its target.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            // SAFETY: successful fstatat initialized the entire structure.
            if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFLNK {
                return Err(EditorFileError::SymlinkPath);
            }
        }
        return Err(error.into());
    }
    // SAFETY: fd is newly owned and has not been wrapped before.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_regular_file(
    parent: &File,
    name: &std::ffi::OsStr,
    _path: &Path,
) -> Result<File, EditorFileError> {
    // O_NONBLOCK prevents special files such as FIFOs from hanging the worker
    // before their file type can be rejected. It has no effect on regular I/O.
    let file = open_child(
        parent,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
    )?;
    if !file.metadata()?.is_file() {
        return Err(EditorFileError::NotRegularFile);
    }
    Ok(file)
}

#[cfg(unix)]
fn create_sibling(
    parent: &File,
    name: &std::ffi::OsStr,
    _path: &Path,
) -> Result<File, EditorFileError> {
    let name = c_name(name)?;
    // SAFETY: parent and name valid; O_CREAT includes the required mode. O_EXCL
    // rejects an existing file or symlink instead of modifying it.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn rename_sibling(
    parent: &File,
    name: &std::ffi::OsStr,
    _path: &Path,
    target: &ResolvedFile,
) -> Result<(), EditorFileError> {
    let source = c_name(name)?;
    let target_name = c_name(&target.name)?;
    // SAFETY: both pinned parent descriptors and terminated names are valid.
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            source.as_ptr(),
            target.parent.as_raw_fd(),
            target_name.as_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn remove_sibling(parent: &File, name: &std::ffi::OsStr, _path: &Path) {
    if let Ok(name) = c_name(name) {
        // SAFETY: pinned parent descriptor and terminated name are valid.
        let _ = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
    }
}

#[cfg(unix)]
fn same_directory(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(unix)]
fn same_target(a: &Metadata, b: &Metadata) -> bool {
    same_directory(a, b) && a.mode() == b.mode()
}

#[cfg(not(unix))]
fn checked_path(path: &Path) -> Result<(), EditorFileError> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(EditorFileError::SymlinkPath);
    }
    Ok(())
}

#[cfg(not(unix))]
fn open_directory(path: &Path) -> Result<File, EditorFileError> {
    let mut checked = PathBuf::new();
    for component in path.components() {
        checked.push(component.as_os_str());
        checked_path(&checked)?;
    }
    Ok(File::open(path)?)
}

#[cfg(not(unix))]
fn open_child_directory(_parent: &File, _name: &std::ffi::OsStr) -> Result<File, EditorFileError> {
    // The desktop Linux implementation pins each directory by descriptor.
    // Other platforms require their equivalent secure directory handle API.
    Err(EditorFileError::Io {
        kind: io::ErrorKind::Unsupported,
        message: "La edición segura de archivos todavía no está disponible en este sistema.".into(),
    })
}

#[cfg(not(unix))]
fn open_regular_file(
    _parent: &File,
    _name: &std::ffi::OsStr,
    path: &Path,
) -> Result<File, EditorFileError> {
    checked_path(path)?;
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(EditorFileError::NotRegularFile);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn create_sibling(
    _parent: &File,
    _name: &std::ffi::OsStr,
    path: &Path,
) -> Result<File, EditorFileError> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

#[cfg(not(unix))]
fn rename_sibling(
    _parent: &File,
    _name: &std::ffi::OsStr,
    path: &Path,
    target: &ResolvedFile,
) -> Result<(), EditorFileError> {
    checked_path(&target.canonical_path)?;
    Ok(fs::rename(path, &target.canonical_path)?)
}

#[cfg(not(unix))]
fn remove_sibling(_parent: &File, _name: &std::ffi::OsStr, path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(not(unix))]
fn same_directory(a: &Metadata, b: &Metadata) -> bool {
    a.is_dir() && b.is_dir() && a.modified().ok() == b.modified().ok()
}

#[cfg(not(unix))]
fn same_target(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
        && a.permissions().readonly() == b.permissions().readonly()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::{EditKind, Selection};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn scope(root: &Path, path: &str) -> EditorFileScope {
        EditorFileScope {
            tab_id: 7,
            generation: 12,
            workspace: root.to_path_buf(),
            relative_path: path.into(),
        }
    }

    fn text(snapshot: &BufferSnapshot) -> String {
        snapshot.text_range(0..snapshot.char_count()).unwrap()
    }

    fn response(worker: &EditorFileWorker) -> EditorFileResponse {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(response) = worker.try_recv() {
                return response;
            }
            assert!(Instant::now() < deadline, "file worker response timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn load_save_preserves_unicode_crlf_and_original_permissions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unicode.rs");
        let original = "// café 🧑🏽‍💻\r\nfn main() {\r\n}\r\n";
        fs::write(&path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let loaded = load_file(&scope(root.path(), "unicode.rs")).unwrap();
        assert_eq!(text(&loaded.buffer.snapshot()), original);
        assert_eq!(loaded.digest, digest_bytes(original.as_bytes()));
        assert!(!loaded.buffer.is_dirty());
        let saved = save_file(
            &scope(root.path(), "unicode.rs"),
            &loaded.buffer.snapshot(),
            loaded.digest,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), original.as_bytes());
        assert_eq!(saved.digest, loaded.digest);
        assert!(saved.durability_warning.is_none());
        #[cfg(unix)]
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn load_rejects_binary_invalid_utf8_directories_and_oversize_without_changing_bytes() {
        let root = tempfile::tempdir().unwrap();
        for (name, bytes, error) in [
            ("binary", vec![0, 1, 2], EditorFileError::BinaryFile),
            ("encoding", vec![0xff, 0xfe], EditorFileError::InvalidUtf8),
        ] {
            let path = root.path().join(name);
            fs::write(&path, &bytes).unwrap();
            assert_eq!(load_file(&scope(root.path(), name)).unwrap_err(), error);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        fs::create_dir(root.path().join("directory")).unwrap();
        assert_eq!(
            load_file(&scope(root.path(), "directory")).unwrap_err(),
            EditorFileError::NotRegularFile
        );
        let oversized = File::create(root.path().join("large")).unwrap();
        oversized
            .set_len(MAX_EDITABLE_FILE_BYTES as u64 + 1)
            .unwrap();
        assert_eq!(
            load_file(&scope(root.path(), "large")).unwrap_err(),
            EditorFileError::TooLarge
        );
    }

    #[test]
    fn editable_limit_accepts_sixteen_mib_using_explicit_core_limits() {
        let root = tempfile::tempdir().unwrap();
        let text = "x".repeat(MAX_EDITABLE_FILE_BYTES);
        fs::write(root.path().join("boundary"), &text).unwrap();
        let loaded = load_file(&scope(root.path(), "boundary")).unwrap();
        assert_eq!(
            loaded.buffer.snapshot().byte_count(),
            MAX_EDITABLE_FILE_BYTES
        );
        assert_eq!(
            loaded.buffer.limits().max_document_bytes,
            MAX_EDITABLE_FILE_BYTES
        );
        assert_eq!(loaded.digest, digest_bytes(text.as_bytes()));
    }

    #[test]
    fn scoped_paths_reject_traversal_and_symlink_escape_for_load_and_save() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("safe"), "safe").unwrap();
        fs::write(outside.path().join("outside"), "outside").unwrap();
        for path in ["", ".", "../outside", "a/../../outside", "/etc/passwd"] {
            assert_eq!(
                load_file(&scope(root.path(), path)).unwrap_err(),
                EditorFileError::InvalidScope
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
            std::os::unix::fs::symlink(outside.path().join("outside"), root.path().join("link"))
                .unwrap();
            let snapshot = TextBuffer::new("edited").unwrap().snapshot();
            for path in ["escape/outside", "link"] {
                assert_eq!(
                    load_file(&scope(root.path(), path)).unwrap_err(),
                    EditorFileError::SymlinkPath
                );
                assert_eq!(
                    save_file(
                        &scope(root.path(), path),
                        &snapshot,
                        digest_bytes(b"outside")
                    )
                    .unwrap_err(),
                    EditorFileError::SymlinkPath
                );
            }
            let linked_workspace = scope(&root.path().join("escape"), "outside");
            assert_eq!(
                load_file(&linked_workspace).unwrap_err(),
                EditorFileError::SymlinkPath
            );
            assert_eq!(
                save_file(&linked_workspace, &snapshot, digest_bytes(b"outside")).unwrap_err(),
                EditorFileError::SymlinkPath
            );
            assert_eq!(
                fs::read(outside.path().join("outside")).unwrap(),
                b"outside"
            );
        }
    }

    #[test]
    fn external_change_never_overwrites_and_leaves_no_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, "original").unwrap();
        let loaded = load_file(&scope(root.path(), "file")).unwrap();
        let edited = TextBuffer::new("edited").unwrap().snapshot();
        fs::write(&path, "external edit").unwrap();
        assert_eq!(
            save_file(&scope(root.path(), "file"), &edited, loaded.digest).unwrap_err(),
            EditorFileError::ExternalChange
        );
        assert_eq!(fs::read(&path).unwrap(), b"external edit");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn readonly_file_is_not_replaced_even_in_a_writable_parent() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("readonly");
        fs::write(&path, "original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        let loaded = load_file(&scope(root.path(), "readonly")).unwrap();
        let error = save_file(
            &scope(root.path(), "readonly"),
            &TextBuffer::new("changed").unwrap().snapshot(),
            loaded.digest,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EditorFileError::Io {
                kind: io::ErrorKind::PermissionDenied,
                ..
            }
        ));
        assert_eq!(fs::read(path).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_save_preserves_linux_user_attributes_and_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("attributed");
        fs::write(&path, "original").unwrap();
        let file = File::open(&path).unwrap();
        let name = std::ffi::CString::new("user.openide-test").unwrap();
        let attribute = b"private metadata";
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                attribute.as_ptr().cast(),
                attribute.len(),
                0,
            )
        };
        if result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOTSUP) {
            return;
        }
        assert_eq!(result, 0, "{}", io::Error::last_os_error());
        let before = file.metadata().unwrap();
        let loaded = load_file(&scope(root.path(), "attributed")).unwrap();
        save_file(
            &scope(root.path(), "attributed"),
            &TextBuffer::new("changed").unwrap().snapshot(),
            loaded.digest,
        )
        .unwrap();
        let replaced = File::open(&path).unwrap();
        let after = replaced.metadata().unwrap();
        assert_eq!((before.uid(), before.gid()), (after.uid(), after.gid()));
        let mut actual = [0u8; 64];
        let len = unsafe {
            libc::fgetxattr(
                replaced.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        };
        assert_eq!(&actual[..len as usize], attribute);
        assert_eq!(fs::read_to_string(path).unwrap(), "changed");
    }

    #[test]
    fn concurrent_exclusive_create_never_overwrites_the_winner() {
        let root = tempfile::tempdir().unwrap();
        let history = root.path().join("history");
        fs::create_dir(&history).unwrap();
        let mut jobs = Vec::new();
        for text in ["first", "second"] {
            let scope = scope(root.path(), "source");
            let history = history.clone();
            jobs.push(std::thread::spawn(move || {
                create_file_with_history(
                    &scope,
                    Path::new("created"),
                    &TextBuffer::new(text).unwrap().snapshot(),
                    &history,
                )
            }));
        }
        let results = jobs
            .into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(EditorFileError::Io {
                        kind: io::ErrorKind::AlreadyExists,
                        ..
                    })
                ))
                .count(),
            1
        );
        let text = fs::read_to_string(root.path().join("created")).unwrap();
        assert!(text == "first" || text == "second");
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".openide-save-")
        }));
    }

    #[test]
    fn failed_atomic_write_removes_partial_sibling_and_preserves_original() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, "original").unwrap();
        let mut temporary_path = PathBuf::new();
        let error = save_file_using(
            &scope(root.path(), "file"),
            &TextBuffer::new("edited").unwrap().snapshot(),
            digest_bytes(b"original"),
            |temporary, _| {
                temporary_path = temporary.path.clone();
                temporary.file.write_all(b"partial")?;
                Err(io::Error::new(io::ErrorKind::WriteZero, "synthetic failed write").into())
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EditorFileError::Io {
                kind: io::ErrorKind::WriteZero,
                ..
            }
        ));
        assert!(!temporary_path.exists());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn change_after_temporary_write_is_rechecked_before_rename() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        fs::write(&path, "original").unwrap();
        let snapshot = TextBuffer::new("edited").unwrap().snapshot();
        let error = save_file_using(
            &scope(root.path(), "file"),
            &snapshot,
            digest_bytes(b"original"),
            |temporary, snapshot| {
                let digest = write_atomic_snapshot(temporary, snapshot)?;
                fs::write(&path, "external edit during save")?;
                Ok(digest)
            },
        )
        .unwrap_err();
        assert_eq!(error, EditorFileError::ExternalChange);
        assert_eq!(fs::read(&path).unwrap(), b"external edit during save");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_substitution_during_save_never_overwrites_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let outside_path = outside.path().join("outside");
        fs::write(&path, "original").unwrap();
        fs::write(&outside_path, "outside").unwrap();
        let snapshot = TextBuffer::new("edited").unwrap().snapshot();
        let error = save_file_using(
            &scope(root.path(), "file"),
            &snapshot,
            digest_bytes(b"original"),
            |temporary, snapshot| {
                let digest = write_atomic_snapshot(temporary, snapshot)?;
                fs::remove_file(&path)?;
                std::os::unix::fs::symlink(&outside_path, &path)?;
                Ok(digest)
            },
        )
        .unwrap_err();
        assert_eq!(error, EditorFileError::SymlinkPath);
        assert_eq!(fs::read(&outside_path).unwrap(), b"outside");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn substituted_parent_directory_rejects_commit_and_cleans_anchored_temporary() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let parent = root.path().join("subdir");
        let moved = outside.path().join("moved");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("file"), "original").unwrap();
        let snapshot = TextBuffer::new("edited").unwrap().snapshot();
        let error = save_file_using(
            &scope(root.path(), "subdir/file"),
            &snapshot,
            digest_bytes(b"original"),
            |temporary, snapshot| {
                let digest = write_atomic_snapshot(temporary, snapshot)?;
                fs::rename(&parent, &moved)?;
                fs::create_dir(&parent)?;
                fs::write(parent.join("file"), "replacement")?;
                Ok(digest)
            },
        )
        .unwrap_err();
        assert_eq!(error, EditorFileError::ExternalChange);
        assert_eq!(fs::read(parent.join("file")).unwrap(), b"replacement");
        assert_eq!(fs::read(moved.join("file")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_rejected_without_waiting_for_a_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pipe");
        let c_path = c_name(path.as_os_str()).unwrap();
        // SAFETY: private fixture path is a valid CString; mkfifo creates no
        // process or communication with another program.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        assert_eq!(
            load_file(&scope(root.path(), "pipe")).unwrap_err(),
            EditorFileError::NotRegularFile
        );
    }

    #[test]
    fn save_response_retains_original_document_and_revision_after_later_edit() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "original").unwrap();
        let mut loaded = load_file(&scope(root.path(), "file")).unwrap();
        loaded
            .buffer
            .edit(0..8, "saved", Selection::caret(5), EditKind::Replace, 1)
            .unwrap();
        let snapshot = loaded.buffer.snapshot();
        let original_id = snapshot.document_id();
        let saved_revision = snapshot.revision();
        let worker = EditorFileWorker::new().unwrap();
        worker
            .try_save(scope(root.path(), "file"), snapshot, loaded.digest)
            .unwrap();
        loaded
            .buffer
            .edit(
                0..5,
                "later edit",
                Selection::caret(10),
                EditKind::Replace,
                2,
            )
            .unwrap();
        let unrelated = TextBuffer::new("another selected document").unwrap();
        match response(&worker) {
            EditorFileResponse::Saved {
                scope: saved_scope,
                document_id,
                revision,
                snapshot,
                digest,
                ..
            } => {
                assert_eq!(saved_scope.generation, 12);
                assert_eq!(document_id, original_id);
                assert_eq!(revision, saved_revision);
                assert_ne!(unrelated.document_id(), document_id);
                loaded.buffer.mark_saved_snapshot(&snapshot).unwrap();
                assert!(loaded.buffer.is_dirty());
                assert_eq!(text(&loaded.buffer.snapshot()), "later edit");
                assert_eq!(digest, digest_bytes(b"saved"));
            }
            other => panic!("unexpected response: {other:?}"),
        }
        assert_eq!(fs::read(root.path().join("file")).unwrap(), b"saved");
    }

    #[test]
    fn bounded_admission_and_cancelled_loads_never_discard_an_admitted_save() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "original").unwrap();
        let (requests, pending) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let (completed, responses) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let latest_loads = Arc::new(Mutex::new(HashMap::new()));
        let worker = EditorFileWorker {
            requests,
            responses,
            latest_loads: Arc::clone(&latest_loads),
            next_load_token: AtomicU64::new(1),
        };
        let saved_scope = scope(root.path(), "file");
        worker
            .try_save(
                saved_scope.clone(),
                TextBuffer::new("saved").unwrap().snapshot(),
                digest_bytes(b"original"),
            )
            .unwrap();
        for generation in 0..EDITOR_FILE_QUEUE_CAPACITY - 1 {
            let mut stale = saved_scope.clone();
            stale.generation = generation as u64;
            worker.try_load(stale).unwrap();
        }
        assert_eq!(
            worker.try_load(saved_scope.clone()),
            Err(EditorFileSubmitError::QueueFull)
        );
        worker.cancel_all_loads();
        let join = std::thread::spawn(move || run_file_worker(pending, completed, latest_loads));
        match response(&worker) {
            EditorFileResponse::Saved { scope, .. } => assert_eq!(scope, saved_scope),
            other => panic!("admitted save lost: {other:?}"),
        }
        drop(worker);
        join.join().unwrap();
        assert_eq!(fs::read(root.path().join("file")).unwrap(), b"saved");
    }

    #[test]
    fn same_tab_latest_load_wins_and_other_tabs_are_independent() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("first"), "first").unwrap();
        fs::write(root.path().join("second"), "second").unwrap();
        let (requests, pending) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let (completed, responses) = mpsc::sync_channel(EDITOR_FILE_QUEUE_CAPACITY);
        let latest_loads = Arc::new(Mutex::new(HashMap::new()));
        let worker = EditorFileWorker {
            requests,
            responses,
            latest_loads: Arc::clone(&latest_loads),
            next_load_token: AtomicU64::new(1),
        };
        worker.try_load(scope(root.path(), "first")).unwrap();
        let mut current = scope(root.path(), "second");
        current.generation = 13;
        worker.try_load(current.clone()).unwrap();
        let mut independent = scope(root.path(), "first");
        independent.tab_id = 8;
        worker.try_load(independent.clone()).unwrap();
        let join = std::thread::spawn(move || run_file_worker(pending, completed, latest_loads));
        for expected in [current, independent] {
            match response(&worker) {
                EditorFileResponse::Loaded { scope, buffer, .. } => {
                    assert_eq!(scope, expected);
                    assert_eq!(
                        text(&buffer.snapshot()),
                        if expected.tab_id == 7 {
                            "second"
                        } else {
                            "first"
                        }
                    );
                }
                other => panic!("unexpected load response: {other:?}"),
            }
        }
        assert!(worker.try_recv().is_none());
        drop(worker);
        join.join().unwrap();
    }

    fn history_baseline(store: &Path, root: &Path, session: &str) {
        let event = openide_core::AgentEvent::new(
            session,
            1,
            openide_core::AgentEventKind::SessionStarted {
                workspace: root.into(),
            },
        );
        crate::session_changes::capture_baseline(
            store,
            session,
            root,
            std::slice::from_ref(&event),
        )
        .unwrap();
        let mut bytes = serde_json::to_vec(&event).unwrap();
        bytes.push(b'\n');
        fs::write(store.join(format!("{session}.jsonl")), bytes).unwrap();
    }

    #[test]
    fn editor_history_worker_save_marks_each_chat_before_commit_without_fake_journal_events() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let store = temp.path().join("store");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&store).unwrap();
        fs::write(root.join("file"), "original\r\n").unwrap();
        history_baseline(&store, &root, "first");
        history_baseline(&store, &root, "second");
        let journal = fs::read(store.join("first.jsonl")).unwrap();
        let loaded = load_file(&scope(&root, "file")).unwrap();
        let snapshot = TextBuffer::new("manual edit\r\n").unwrap().snapshot();
        let worker = EditorFileWorker::new().unwrap();
        worker
            .try_save_with_history(
                scope(&root, "file"),
                snapshot.clone(),
                loaded.digest,
                store.clone(),
            )
            .unwrap();
        let saved = response(&worker);
        assert!(
            matches!(saved,EditorFileResponse::Saved {revision,document_id,..}
            if revision==snapshot.revision() && document_id==snapshot.document_id())
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"manual edit\r\n");
        for session in ["first", "second"] {
            assert_eq!(
                crate::session_changes_editor::editor_save_boundary(&store, session, &root)
                    .unwrap(),
                Some(1)
            );
        }
        assert_eq!(fs::read(store.join("first.jsonl")).unwrap(), journal);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn editor_history_failed_marker_never_changes_document_and_retry_keeps_manual_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let store = temp.path().join("store");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&store).unwrap();
        fs::write(root.join("file"), "original").unwrap();
        history_baseline(&store, &root, "session");
        let marker = store.join("session-changes-editor/session.json");
        fs::write(&marker, "bad marker").unwrap();
        let digest = digest_bytes(b"original");
        let snapshot = TextBuffer::new("manual").unwrap().snapshot();
        assert!(save_file_with_history(&scope(&root, "file"), &snapshot, digest, &store).is_err());
        assert_eq!(fs::read(root.join("file")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_file(&marker).unwrap();
        save_file_with_history(&scope(&root, "file"), &snapshot, digest, &store).unwrap();
        assert_eq!(fs::read(root.join("file")).unwrap(), b"manual");
        assert_eq!(
            crate::session_changes_editor::editor_save_boundary(&store, "session", &root).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn editor_history_content_conflict_or_missing_store_does_not_publish_new_effect() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let store = temp.path().join("store");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&store).unwrap();
        fs::write(root.join("file"), "original").unwrap();
        history_baseline(&store, &root, "session");
        let snapshot = TextBuffer::new("manual").unwrap().snapshot();
        fs::write(root.join("file"), "external").unwrap();
        assert_eq!(
            save_file_with_history(
                &scope(&root, "file"),
                &snapshot,
                digest_bytes(b"original"),
                &store
            )
            .unwrap_err(),
            EditorFileError::ExternalChange
        );
        assert_eq!(
            crate::session_changes_editor::editor_save_boundary(&store, "session", &root).unwrap(),
            None
        );
        assert!(
            save_file_with_history(
                &scope(&root, "file"),
                &snapshot,
                digest_bytes(b"external"),
                &temp.path().join("missing-store")
            )
            .is_err()
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"external");
    }
}
