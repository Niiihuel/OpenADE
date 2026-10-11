//! Durable manual editor-save boundaries, independent of external MCP markers.
//! Baseline capture and editor save share an advisory workspace lock. The
//! first boundary is published before committing a file, for every baseline
//! in this store that belongs to that workspace. No AgentEvent is fabricated.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const VERSION: u8 = 1;
const MAX_MARKER_BYTES: usize = 8 * 1024;
const MAX_WORKSPACE_BYTES: usize = 4096;
const DIRECTORY: &str = "session-changes-editor";
const LOCK_BUDGET: Duration = Duration::from_secs(8);
const SAVE_HISTORY_BUDGET: Duration = Duration::from_secs(8);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u8,
    session_id: String,
    workspace: String,
    after_sequence: u64,
}

/// Worker-only advisory lock; never acquire while building or painting UI.
/// Its inode is never removed, avoiding split locks from a replaced lockfile.
/// Other actors (tools, external editors) do not participate in this protocol.
pub(crate) struct WorkspaceGuard {
    _file: File,
    directory: Directory,
    storage_root: PathBuf,
    workspace: PathBuf,
}

impl WorkspaceGuard {
    /// The caller must retain this guard through a guarded editor commit.
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }
}

pub(crate) fn lock_workspace(storage_root: &Path, workspace: &Path) -> io::Result<WorkspaceGuard> {
    lock_workspace_with_budget(storage_root, workspace, LOCK_BUDGET)
}

fn lock_workspace_with_budget(
    storage_root: &Path,
    workspace: &Path,
    budget: Duration,
) -> io::Result<WorkspaceGuard> {
    let workspace_text = canonical_workspace(workspace)?;
    let directory = Directory::open(storage_root, true)?
        .ok_or_else(|| invalid_data("No existe el registro de guardado del editor"))?;
    let filename = format!(
        ".workspace-{:x}.lock",
        Sha256::digest(workspace_text.as_bytes())
    );
    let file = directory.lock_file(&filename)?;
    let metadata = file.metadata()?;
    require_private_file(&metadata)?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(invalid_data(
            "El lock del workspace no es un archivo privado vacío",
        ));
    }
    directory.sync()?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < budget => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Otra captura o guardado usa esta carpeta; reintentá guardar",
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    Ok(WorkspaceGuard {
        _file: file,
        directory,
        storage_root: storage_root.canonicalize()?,
        workspace: workspace_text.into(),
    })
}

/// Must finish successfully while holding the same guard through the atomic
/// file commit. Partial marker publication is conservative and retryable;
/// failure never grants permission to save without historical evidence.
pub(crate) fn mark_workspace_save(guard: &WorkspaceGuard) -> io::Result<()> {
    guard.directory.require_current(&guard.storage_root)?;
    let deadline = Instant::now() + SAVE_HISTORY_BUDGET;
    let boundaries = crate::session_changes::editor_save_boundaries(
        &guard.storage_root,
        &guard.workspace,
        deadline,
    )?;
    for (session_id, after_sequence) in boundaries {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "El registro histórico del guardado excedió su tiempo máximo; reintentá",
            ));
        }
        mark_session(
            &guard.storage_root,
            &session_id,
            &guard.workspace,
            after_sequence,
        )?;
    }
    guard.directory.require_current(&guard.storage_root)
}

fn mark_session(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    after_sequence: u64,
) -> io::Result<()> {
    let filename = marker_filename(session_id)?;
    let workspace = canonical_workspace(workspace)?;
    let directory = Directory::open(storage_root, true)?
        .ok_or_else(|| invalid_data("No existe el registro de guardado del editor"))?;
    if let Some(marker) = read_marker(&directory, &filename, session_id, Some(&workspace))? {
        // A previous boundary already makes all later effects conservative.
        // A newly discovered earlier pending actor must never weaken it.
        if after_sequence < marker.after_sequence {
            return Err(invalid_data(
                "El guardado precede su límite histórico registrado",
            ));
        }
        return directory.sync();
    }
    let bytes = serde_json::to_vec(&Marker {
        version: VERSION,
        session_id: session_id.into(),
        workspace: workspace.clone(),
        after_sequence,
    })
    .map_err(invalid_data)?;
    let temporary = format!(".{session_id}.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = directory.create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        directory.publish(&temporary, &filename)?;
        directory.sync()
    })();
    let cleanup = directory.remove(&temporary);
    result?;
    cleanup?;
    directory.sync()
}

pub(crate) fn editor_save_boundary(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
) -> io::Result<Option<u64>> {
    let filename = marker_filename(session_id)?;
    let workspace = canonical_workspace(workspace)?;
    let Some(directory) = Directory::open(storage_root, false)? else {
        return Ok(None);
    };
    Ok(
        read_marker(&directory, &filename, session_id, Some(&workspace))?
            .map(|marker| marker.after_sequence),
    )
}

/// A rewind is about to remove the journal from `before_sequence` on. A
/// boundary inside that part would sit past the journal's end and fail every
/// later reading, so it moves to the last sequence that stays. Earlier is
/// the conservative direction. Call before the journal changes: if the
/// rewind then fails, the earlier boundary is still valid.
pub(crate) fn rewind_marker(
    storage_root: &Path,
    session_id: &str,
    before_sequence: u64,
) -> io::Result<()> {
    let filename = marker_filename(session_id)?;
    let Some(directory) = Directory::open(storage_root, false)? else {
        return Ok(());
    };
    let Some(marker) = read_marker(&directory, &filename, session_id, None)? else {
        return Ok(());
    };
    if marker.after_sequence < before_sequence {
        return Ok(());
    }
    directory.remove(&filename)?;
    directory.sync()?;
    mark_session(
        storage_root,
        session_id,
        Path::new(&marker.workspace),
        before_sequence.saturating_sub(1),
    )
}

/// Call only after the baseline and corresponding chat were durably deleted.
pub(crate) fn remove_marker(storage_root: &Path, session_id: &str) -> io::Result<()> {
    let filename = marker_filename(session_id)?;
    let Some(directory) = Directory::open(storage_root, false)? else {
        return Ok(());
    };
    if read_marker(&directory, &filename, session_id, None)?.is_some() {
        directory.remove(&filename)?;
        directory.sync()?;
    }
    Ok(())
}

fn marker_filename(session_id: &str) -> io::Result<String> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid_data(
            "Identidad de sesión inválida para el guardado del editor",
        ));
    }
    Ok(format!("{session_id}.json"))
}

fn canonical_workspace(workspace: &Path) -> io::Result<String> {
    let root = workspace.canonicalize()?;
    let text = root
        .to_str()
        .ok_or_else(|| invalid_data("Workspace no Unicode"))?;
    if !root.is_dir() || text.len() > MAX_WORKSPACE_BYTES || text.chars().any(char::is_control) {
        return Err(invalid_data(
            "Workspace inválido para el guardado del editor",
        ));
    }
    Ok(text.into())
}

fn read_marker(
    directory: &Directory,
    filename: &str,
    session_id: &str,
    workspace: Option<&str>,
) -> io::Result<Option<Marker>> {
    let file = match directory.read(filename) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_MARKER_BYTES as u64 {
        return Err(invalid_data("Marcador del editor no es un archivo acotado"));
    }
    require_private_file(&metadata)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(invalid_data("Marcador del editor demasiado extenso"));
    }
    let marker: Marker = serde_json::from_slice(&bytes).map_err(invalid_data)?;
    if marker.version != VERSION
        || marker.session_id != session_id
        || marker.workspace.is_empty()
        || marker.workspace.len() > MAX_WORKSPACE_BYTES
        || marker.workspace.chars().any(char::is_control)
        || !Path::new(&marker.workspace).is_absolute()
        || workspace.is_some_and(|expected| marker.workspace != expected)
    {
        return Err(invalid_data(
            "Marcador del editor pertenece a otra versión o alcance",
        ));
    }
    Ok(Some(marker))
}

fn invalid_data(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(unix)]
pub(crate) fn require_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
        return Err(invalid_data(
            "Marcador guardado del editor no es un archivo privado",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn require_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    reject_link(metadata)
}

#[cfg(unix)]
pub(crate) struct Directory {
    file: File,
    storage: File,
}

#[cfg(unix)]
impl Directory {
    fn open(storage_root: &Path, create: bool) -> io::Result<Option<Self>> {
        Self::open_named(storage_root, DIRECTORY, create)
    }

    pub(crate) fn open_named(
        storage_root: &Path,
        directory_name: &str,
        create: bool,
    ) -> io::Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let current = open_storage_directory(storage_root)?;
        if create {
            let name = c_string(directory_name.as_ref())?;
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            } else {
                current.sync_all()?;
            }
        }
        let file = match open_directory_at(&current, directory_name.as_ref()) {
            Ok(file) => file,
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(invalid_data(
                "Directorio de marcadores guardado del editor no es privado",
            ));
        }
        Ok(Some(Self {
            file,
            storage: current,
        }))
    }

    fn require_current(&self, storage_root: &Path) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let current = Self::open(storage_root, false)?
            .ok_or_else(|| invalid_data("Desapareció el registro histórico"))?;
        for (before, after) in [
            (self.file.metadata()?, current.file.metadata()?),
            (self.storage.metadata()?, current.storage.metadata()?),
        ] {
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(invalid_data(
                    "El registro histórico cambió de identidad; no se guardó el archivo",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn read(&self, name: &str) -> io::Result<File> {
        open_file_at(&self.file, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
    }

    pub(crate) fn create(&self, name: &str) -> io::Result<File> {
        use std::os::fd::AsRawFd;
        let file = open_file_at(
            &self.file,
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NONBLOCK,
            0o600,
        )?;
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }

    fn lock_file(&self, name: &str) -> io::Result<File> {
        open_file_at(
            &self.file,
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK,
            0o600,
        )
    }

    pub(crate) fn publish(&self, temporary: &str, destination: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let temporary = c_string(temporary.as_ref())?;
        let destination = c_string(destination.as_ref())?;
        if unsafe {
            libc::linkat(
                self.file.as_raw_fd(),
                temporary.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let name = c_string(name.as_ref())?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(unix)]
fn open_storage_directory(storage_root: &Path) -> io::Result<File> {
    use std::path::Component;
    let absolute = if storage_root.is_absolute() {
        storage_root.to_owned()
    } else {
        std::env::current_dir()?.join(storage_root)
    };
    let mut current = open_directory_root()?;
    for part in absolute.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => current = open_directory_at(&current, part)?,
            _ => return Err(invalid_data("Ruta histórica del editor inválida")),
        }
    }
    Ok(current)
}

pub(crate) fn open_baseline_file(
    storage_root: &Path,
    session_id: &str,
) -> io::Result<Option<File>> {
    let name = marker_filename(session_id)?;
    let Some(directory) = Directory::open_named(storage_root, "session-changes", false)? else {
        return Ok(None);
    };
    match directory.read(&name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn open_journal_file(storage_root: &Path, session_id: &str) -> io::Result<File> {
    marker_filename(session_id)?;
    let name = format!("{session_id}.jsonl");
    #[cfg(unix)]
    {
        open_file_at(
            &open_storage_directory(storage_root)?,
            &name,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )
    }
    #[cfg(not(unix))]
    {
        validate_directory_path(storage_root)?;
        let path = storage_root.join(name);
        reject_link(&std::fs::symlink_metadata(&path)?)?;
        portable_options().read(true).open(path)
    }
}

#[cfg(unix)]
fn c_string(value: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_bytes()).map_err(invalid_data)
}

#[cfg(unix)]
fn open_directory_root() -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
}

#[cfg(unix)]
fn open_directory_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_string(name)?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(unix)]
fn open_file_at(directory: &File, name: &str, flags: i32, mode: libc::mode_t) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_string(name.as_ref())?;
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(not(unix))]
pub(crate) struct Directory {
    path: PathBuf,
}

// The portable fallback verifies each path component and opens the final
// Windows file as a reparse point, rejecting it before reading. It inherits
// the session store's user-profile ACL. Unlike Unix openat handles, these
// pathname checks do not guarantee identity against parent-renaming races.
#[cfg(not(unix))]
impl Directory {
    fn open(storage_root: &Path, create: bool) -> io::Result<Option<Self>> {
        Self::open_named(storage_root, DIRECTORY, create)
    }

    pub(crate) fn open_named(
        storage_root: &Path,
        directory_name: &str,
        create: bool,
    ) -> io::Result<Option<Self>> {
        let storage_root = if storage_root.is_absolute() {
            storage_root.to_owned()
        } else {
            std::env::current_dir()?.join(storage_root)
        };
        validate_directory_path(&storage_root)?;
        let path = storage_root.join(directory_name);
        if create {
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        match validate_directory_path(&path) {
            Ok(()) => Ok(Some(Self { path })),
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn require_current(&self, storage_root: &Path) -> io::Result<()> {
        validate_directory_path(storage_root)?;
        validate_directory_path(&self.path)
    }

    fn checked_path(&self, name: &str) -> io::Result<PathBuf> {
        validate_directory_path(&self.path)?;
        let path = self.path.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => reject_link(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(path)
    }

    pub(crate) fn read(&self, name: &str) -> io::Result<File> {
        portable_options().read(true).open(self.checked_path(name)?)
    }

    pub(crate) fn create(&self, name: &str) -> io::Result<File> {
        portable_options()
            .write(true)
            .create_new(true)
            .open(self.checked_path(name)?)
    }

    fn lock_file(&self, name: &str) -> io::Result<File> {
        portable_options()
            .read(true)
            .write(true)
            .create(true)
            .open(self.checked_path(name)?)
    }

    pub(crate) fn publish(&self, temporary: &str, destination: &str) -> io::Result<()> {
        // Hard links publish create-only on NTFS and other supporting local
        // filesystems. Never use rename with replacement on Windows.
        let temporary = self.checked_path(temporary)?;
        let destination = self.checked_path(destination)?;
        std::fs::hard_link(temporary, &destination)?;
        // Windows has no portable directory fsync. Flush the published file;
        // this preserves the strongest durability available through std here.
        portable_options().write(true).open(destination)?.sync_all()
    }

    pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
        match std::fs::remove_file(self.checked_path(name)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        validate_directory_path(&self.path)
    }
}

#[cfg(not(unix))]
fn validate_directory_path(path: &Path) -> io::Result<()> {
    use std::path::Component;
    let mut current = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(_) => current.push(part),
            Component::RootDir | Component::Normal(_) => {
                current.push(part);
                let metadata = std::fs::symlink_metadata(&current)?;
                reject_link(&metadata)?;
                if !metadata.is_dir() {
                    return Err(invalid_data(
                        "Un padre del marcador guardado del editor no es directorio",
                    ));
                }
            }
            Component::CurDir => {}
            _ => {
                return Err(invalid_data(
                    "Ruta del registro guardado del editor no válida",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_link(metadata: &std::fs::Metadata) -> io::Result<()> {
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if metadata.file_type().is_symlink() || reparse {
        return Err(invalid_data(
            "El marcador guardado del editor no admite enlaces ni reparse points",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn portable_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_changes;
    use crate::session_tool_diff::load_stored_tool_diff;
    use openide_core::{AgentEvent, AgentEventKind, ToolCall};
    use serde_json::json;
    use std::fs;
    use std::sync::mpsc;

    struct Fixture {
        _temp: tempfile::TempDir,
        store: PathBuf,
        workspace: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let store = temp.path().join("store");
            let workspace = temp.path().join("workspace");
            fs::create_dir(&store).unwrap();
            fs::create_dir(&workspace).unwrap();
            fs::write(workspace.join("file.rs"), "initial\n").unwrap();
            Self {
                _temp: temp,
                store,
                workspace,
            }
        }
        fn baseline(&self, id: &str) -> Vec<AgentEvent> {
            let events = vec![AgentEvent::new(
                id,
                1,
                AgentEventKind::SessionStarted {
                    workspace: self.workspace.clone(),
                },
            )];
            session_changes::capture_baseline(&self.store, id, &self.workspace, &events).unwrap();
            self.persist(id, &events);
            events
        }
        fn persist(&self, id: &str, events: &[AgentEvent]) {
            let mut bytes = Vec::new();
            for event in events {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.store.join(format!("{id}.jsonl")), bytes).unwrap();
        }
        fn mark(&self) -> io::Result<()> {
            let guard = lock_workspace(&self.store, &self.workspace)?;
            mark_workspace_save(&guard)
        }
        fn cut(&self, id: &str) -> Option<u64> {
            editor_save_boundary(&self.store, id, &self.workspace).unwrap()
        }
    }
    fn push(events: &mut Vec<AgentEvent>, id: &str, kind: AgentEventKind) {
        events.push(AgentEvent::new(id, events.len() as u64 + 1, kind));
    }
    fn start_write(events: &mut Vec<AgentEvent>, id: &str, tool: &str, content: &str) {
        push(
            events,
            id,
            AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: tool.into(),
                    name: "write_file".into(),
                    arguments: json!({"path":"file.rs","content":content}),
                },
            },
        );
    }
    fn finish(events: &mut Vec<AgentEvent>, id: &str, tool: &str) {
        push(
            events,
            id,
            AgentEventKind::ToolFinished {
                tool_call_id: tool.into(),
                output: "ok".into(),
                is_error: false,
            },
        );
    }

    #[test]
    fn editor_history_marks_all_matching_baselines_and_keeps_mcp_independent() {
        let fixture = Fixture::new();
        let mut first = fixture.baseline("first");
        let second = fixture.baseline("second");
        start_write(&mut first, "first", "edit", "AI\n");
        finish(&mut first, "first", "edit");
        fixture.persist("first", &first);
        let other = fixture._temp.path().join("other");
        fs::create_dir(&other).unwrap();
        let foreign = vec![AgentEvent::new(
            "foreign",
            1,
            AgentEventKind::SessionStarted {
                workspace: other.clone(),
            },
        )];
        session_changes::capture_baseline(&fixture.store, "foreign", &other, &foreign).unwrap();
        crate::session_changes_external::mark_external_runtime(
            &fixture.store,
            "first",
            &fixture.workspace,
            2,
        )
        .unwrap();
        fixture.mark().unwrap();
        assert_eq!(fixture.cut("first"), Some(3));
        assert_eq!(fixture.cut("second"), Some(second.last().unwrap().sequence));
        assert_eq!(
            editor_save_boundary(&fixture.store, "foreign", &other).unwrap(),
            None
        );
        assert_eq!(
            crate::session_changes_external::external_runtime_was_prepared(
                &fixture.store,
                "first",
                &fixture.workspace
            )
            .unwrap(),
            Some(2)
        );
        assert_eq!(fs::read(fixture.store.join("first.jsonl")).unwrap(), {
            let mut bytes = Vec::new();
            for event in &first {
                serde_json::to_writer(&mut bytes, event).unwrap();
                bytes.push(b'\n');
            }
            bytes
        });
    }

    #[test]
    fn editor_history_preserves_old_inline_tools_and_limits_new_full_write_and_patch() {
        let fixture = Fixture::new();
        let mut events = fixture.baseline("session");
        start_write(&mut events, "session", "old", "middle\n");
        finish(&mut events, "session", "old");
        fixture.persist("session", &events);
        let old =
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "old").unwrap();
        assert!(!old.limited);
        fixture.mark().unwrap();
        fs::write(fixture.workspace.join("file.rs"), "manual\n").unwrap();
        start_write(&mut events, "session", "new", "final\n");
        finish(&mut events, "session", "new");
        fixture.persist("session", &events);
        assert_eq!(
            old,
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "old").unwrap()
        );
        let new =
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "new").unwrap();
        assert!(new.limited);
        assert!(new.files.iter().any(|file| file.unavailable.is_some()));
        assert!(
            new.coverage_notes
                .iter()
                .any(|note| note.contains("manuales"))
        );
        let cumulative = session_changes::load_stored_session_changes(
            &fixture.store,
            "session",
            &fixture.workspace,
        )
        .unwrap();
        assert!(
            cumulative
                .files
                .iter()
                .any(|file| file.unavailable.is_none())
        );
        assert!(
            cumulative
                .coverage_notes
                .iter()
                .any(|note| note.contains("no representa cambios exclusivos"))
        );
        push(
            &mut events,
            "session",
            AgentEventKind::ToolStarted {
                tool_call: ToolCall {
                    id: "patch".into(),
                    name: "apply_patch".into(),
                    arguments: json!({"patch":"*** Begin Patch\n*** Update File: file.rs\n@@\n-final\n+patched\n*** End Patch"}),
                },
            },
        );
        finish(&mut events, "session", "patch");
        fixture.persist("session", &events);
        let patch =
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "patch").unwrap();
        assert!(patch.limited);
        assert!(
            patch.unavailable.is_some()
                || patch.files.iter().any(|file| file.unavailable.is_some())
        );
        // A subsequent save never replaces or advances the first cut.
        fixture.mark().unwrap();
        assert_eq!(fixture.cut("session"), Some(3));
    }

    #[test]
    fn editor_history_pending_edit_and_post_hook_cut_before_started_not_last_delta() {
        let fixture = Fixture::new();
        let mut events = fixture.baseline("session");
        start_write(&mut events, "session", "old", "middle\n");
        finish(&mut events, "session", "old");
        start_write(&mut events, "session", "pending", "later\n");
        push(
            &mut events,
            "session",
            AgentEventKind::HookStarted {
                tool_call_id: "pending".into(),
                hook_id: "post".into(),
                phase: "post_tool_use".into(),
            },
        );
        fixture.persist("session", &events);
        fixture.mark().unwrap();
        assert_eq!(fixture.cut("session"), Some(3));
        push(
            &mut events,
            "session",
            AgentEventKind::HookFinished {
                tool_call_id: "pending".into(),
                hook_id: "post".into(),
                phase: "post_tool_use".into(),
                decision: "continue".into(),
            },
        );
        finish(&mut events, "session", "pending");
        fixture.persist("session", &events);
        let old =
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "old").unwrap();
        assert!(!old.limited);
        let pending =
            load_stored_tool_diff(&fixture.store, "session", &fixture.workspace, "pending")
                .unwrap();
        assert!(pending.limited);
        assert!(pending.files.iter().any(|file| file.unavailable.is_some()));
    }

    #[test]
    fn editor_history_invalid_or_incomplete_journal_and_foreign_bad_baseline_fail_closed() {
        let fixture = Fixture::new();
        fixture.baseline("session");
        fs::write(fixture.store.join("session.jsonl"), b"{\"unfinished\":").unwrap();
        assert!(fixture.mark().is_err());
        assert_eq!(fixture.cut("session"), None);
        fixture.baseline("session");
        fs::write(
            fixture.store.join("session-changes/foreign.json"),
            b"invalid",
        )
        .unwrap();
        assert!(fixture.mark().is_err());
        assert_eq!(fixture.cut("session"), None);
        fs::remove_file(fixture.store.join("session-changes/foreign.json")).unwrap();
        fixture.mark().unwrap();
        assert_eq!(fixture.cut("session"), Some(1));
    }

    #[test]
    fn rewind_moves_a_boundary_inside_the_removed_part_and_keeps_an_earlier_one() {
        let temp = tempfile::tempdir().unwrap();
        let (storage, workspace) = (temp.path().join("store"), temp.path().join("work"));
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        mark_session(&storage, "session", &workspace, 12).unwrap();
        rewind_marker(&storage, "session", 20).unwrap();
        assert_eq!(editor_save_boundary(&storage, "session", &workspace).unwrap(), Some(12));
        rewind_marker(&storage, "session", 9).unwrap();
        assert_eq!(editor_save_boundary(&storage, "session", &workspace).unwrap(), Some(8));
        // The journal regrows from 9: a save after it is accepted again.
        mark_session(&storage, "session", &workspace, 10).unwrap();
        rewind_marker(&storage, "missing", 3).unwrap();
    }

    #[test]
    fn editor_history_lock_is_independent_per_workspace_and_times_out_without_waiting_ui() {
        let fixture = Fixture::new();
        let guard = lock_workspace(&fixture.store, &fixture.workspace).unwrap();
        let error = lock_workspace_with_budget(
            &fixture.store,
            &fixture.workspace,
            Duration::from_millis(15),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let other = fixture._temp.path().join("other");
        fs::create_dir(&other).unwrap();
        let _independent = lock_workspace(&fixture.store, &other).unwrap();
        drop(guard);
        assert!(lock_workspace(&fixture.store, &fixture.workspace).is_ok());
    }

    #[test]
    fn editor_history_baseline_capture_waits_until_manual_effect_and_new_baseline_starts_after_it()
    {
        let fixture = Fixture::new();
        fixture.baseline("before");
        let guard = lock_workspace(&fixture.store, &fixture.workspace).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let workspace = fixture.workspace.clone();
        let store = fixture.store.clone();
        let worker = std::thread::spawn(move || {
            let events = vec![AgentEvent::new(
                "after",
                1,
                AgentEventKind::SessionStarted {
                    workspace: workspace.clone(),
                },
            )];
            started_tx.send(()).unwrap();
            done_tx
                .send(session_changes::capture_baseline(
                    &store, "after", &workspace, &events,
                ))
                .unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        mark_workspace_save(&guard).unwrap();
        fs::write(fixture.workspace.join("file.rs"), "manual\n").unwrap();
        drop(guard);
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .is_ok()
        );
        worker.join().unwrap();
        assert_eq!(fixture.cut("before"), Some(1));
        assert_eq!(fixture.cut("after"), None);
        let mut events = vec![AgentEvent::new(
            "after",
            1,
            AgentEventKind::SessionStarted {
                workspace: fixture.workspace.clone(),
            },
        )];
        start_write(&mut events, "after", "edit", "AI\n");
        finish(&mut events, "after", "edit");
        fixture.persist("after", &events);
        let diff =
            load_stored_tool_diff(&fixture.store, "after", &fixture.workspace, "edit").unwrap();
        assert!(!diff.limited);
        assert!(
            diff.files[0]
                .file
                .lines
                .iter()
                .any(|row| row.text == "-manual")
        );
    }

    #[test]
    fn editor_history_session_limit_deadline_and_checksum_are_strict_and_retryable() {
        let fixture = Fixture::new();
        fixture.baseline("session");
        assert!(
            session_changes::editor_save_boundaries(
                &fixture.store,
                &fixture.workspace,
                Instant::now()
            )
            .is_err()
        );
        for index in 0..2048 {
            fs::write(
                fixture
                    .store
                    .join(format!("session-changes/extra{index}.json")),
                b"{}",
            )
            .unwrap();
        }
        assert!(fixture.mark().is_err());
        assert_eq!(fixture.cut("session"), None);
        for index in 0..2048 {
            fs::remove_file(
                fixture
                    .store
                    .join(format!("session-changes/extra{index}.json")),
            )
            .unwrap();
        }
        fixture.mark().unwrap();
        assert_eq!(fixture.cut("session"), Some(1));
        session_changes::remove_baseline(&fixture.store, "session").unwrap();
        assert_eq!(fixture.cut("session"), None);
    }

    #[cfg(unix)]
    #[test]
    fn editor_history_guard_detects_directory_identity_replacement_before_publication() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        fixture.baseline("session");
        let guard = lock_workspace(&fixture.store, &fixture.workspace).unwrap();
        let path = fixture.store.join(DIRECTORY);
        fs::rename(&path, fixture.store.join("old-directory")).unwrap();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(mark_workspace_save(&guard).is_err());
        assert!(!path.join("session.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn editor_history_rejects_symlink_markers_directories_unsafe_permissions_and_lock_substitution()
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new();
        fixture.baseline("session");
        let marker = fixture.store.join(DIRECTORY).join("session.json");
        symlink(fixture.workspace.join("file.rs"), &marker).unwrap();
        assert!(fixture.mark().is_err());
        assert!(editor_save_boundary(&fixture.store, "session", &fixture.workspace).is_err());
        fs::remove_file(&marker).unwrap();
        fixture.mark().unwrap();
        assert_eq!(
            fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(fixture.mark().is_err());
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        let workspace_text = canonical_workspace(&fixture.workspace).unwrap();
        let lock = fixture.store.join(DIRECTORY).join(format!(
            ".workspace-{:x}.lock",
            Sha256::digest(workspace_text.as_bytes())
        ));
        fs::remove_file(&lock).unwrap();
        symlink(fixture.workspace.join("file.rs"), &lock).unwrap();
        assert!(lock_workspace(&fixture.store, &fixture.workspace).is_err());
        fs::remove_file(&lock).unwrap();
        let directory = fixture.store.join(DIRECTORY);
        let moved = fixture.store.join("moved");
        fs::rename(&directory, &moved).unwrap();
        symlink(&moved, &directory).unwrap();
        assert!(lock_workspace(&fixture.store, &fixture.workspace).is_err());
    }
}
