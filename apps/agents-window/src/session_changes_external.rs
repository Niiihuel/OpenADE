//! Durable evidence that an external MCP service may have touched the chat's
//! workspace outside ToolStarted/ToolFinished. Presence limits file replay;
//! it does not assert that a filesystem effect actually happened.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const VERSION: u8 = 1;
const MAX_MARKER_BYTES: usize = 8 * 1024;
const MAX_WORKSPACE_BYTES: usize = 4096;
const DIRECTORY: &str = "session-changes-external";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u8,
    session_id: String,
    workspace: String,
    /// Journal sequence before the first external service preparation. Older
    /// confirmed tools remain reconstructable when MCP is enabled later.
    after_sequence: u64,
}

/// Call after baseline capture, before starting any external MCP service.
/// Failure must prevent startup: otherwise its effects would have no durable
/// coverage marker. Existing valid markers are retained without rewriting.
pub(crate) fn mark_external_runtime(
    storage_root: &Path,
    session_id: &str,
    workspace: &Path,
    after_sequence: u64,
) -> io::Result<()> {
    let filename = marker_filename(session_id)?;
    let workspace = canonical_workspace(workspace)?;
    let directory = Directory::open(storage_root, true)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "No existe el registro de MCP externo",
        )
    })?;
    if let Some(marker) = read_marker(&directory, &filename, session_id, Some(&workspace))? {
        require_earliest_boundary(&marker, after_sequence)?;
        return directory.sync();
    }
    let bytes = serde_json::to_vec(&Marker {
        version: VERSION,
        session_id: session_id.to_owned(),
        workspace: workspace.clone(),
        after_sequence,
    })
    .map_err(invalid_data)?;
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(invalid_data("Marcador MCP externo demasiado extenso"));
    }
    let temporary = format!(".{session_id}.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = directory.create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        match directory.publish(&temporary, &filename) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let marker = read_marker(&directory, &filename, session_id, Some(&workspace))?
                    .ok_or_else(|| invalid_data("El marcador MCP externo desapareció"))?;
                require_earliest_boundary(&marker, after_sequence)?;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    })();
    let cleanup = directory.remove(&temporary);
    result?;
    cleanup?;
    directory.sync()
}

/// A strict, bounded, read-only lookup of the first preparation boundary.
/// Malformed or wrong-scope markers fail closed. Unix also requires owner-only
/// permissions. Absence returns None.
pub(crate) fn external_runtime_was_prepared(
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

fn require_earliest_boundary(marker: &Marker, after_sequence: u64) -> io::Result<()> {
    if after_sequence < marker.after_sequence {
        return Err(invalid_data(
            "La preparación MCP externa precede su límite registrado",
        ));
    }
    Ok(())
}

/// A rewind is about to remove the journal from `before_sequence` on. A
/// boundary inside that part would be later than every boundary a new turn
/// can offer, and refuse them all, so it moves to the last sequence that
/// stays. Call before the journal changes.
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
    mark_external_runtime(
        storage_root,
        session_id,
        Path::new(&marker.workspace),
        before_sequence.saturating_sub(1),
    )
}

/// Remove only after the corresponding session deletion has committed.
/// Does not need the workspace to still exist, and never follows a symlink.
pub(crate) fn remove_marker(storage_root: &Path, session_id: &str) -> io::Result<()> {
    let filename = marker_filename(session_id)?;
    let Some(directory) = Directory::open(storage_root, false)? else {
        return Ok(());
    };
    if read_marker(&directory, &filename, session_id, None)?.is_none() {
        return Ok(());
    }
    directory.remove(&filename)?;
    directory.sync()
}

fn marker_filename(session_id: &str) -> io::Result<String> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Identidad de sesión no válida para el marcador MCP externo",
        ));
    }
    Ok(format!("{session_id}.json"))
}

fn canonical_workspace(workspace: &Path) -> io::Result<String> {
    let path = workspace.canonicalize()?;
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "El workspace del marcador debe ser un directorio",
        ));
    }
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Workspace no Unicode"))?;
    if text.len() > MAX_WORKSPACE_BYTES || text.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Workspace no válido para el marcador MCP externo",
        ));
    }
    Ok(text.to_owned())
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
        return Err(invalid_data(
            "Marcador MCP externo no es un archivo acotado",
        ));
    }
    require_private_file(&metadata)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(invalid_data("Marcador MCP externo demasiado extenso"));
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
            "Marcador MCP externo pertenece a otra versión o alcance",
        ));
    }
    Ok(Some(marker))
}

fn invalid_data(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(unix)]
fn require_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
        return Err(invalid_data(
            "Marcador MCP externo no es un archivo privado",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_file(metadata: &std::fs::Metadata) -> io::Result<()> {
    reject_link(metadata)
}

#[cfg(unix)]
struct Directory {
    file: File,
}

#[cfg(unix)]
impl Directory {
    fn open(storage_root: &Path, create: bool) -> io::Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        use std::path::Component;
        let absolute: PathBuf = if storage_root.is_absolute() {
            storage_root.to_owned()
        } else {
            std::env::current_dir()?.join(storage_root)
        };
        let mut current = open_directory_root()?;
        for part in absolute.components() {
            match part {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => current = open_directory_at(&current, part)?,
                _ => return Err(invalid_data("Ruta del registro MCP externo no válida")),
            }
        }
        if create {
            let name = c_string(DIRECTORY.as_ref())?;
            if unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            } else {
                current.sync_all()?;
            }
        }
        let file = match open_directory_at(&current, DIRECTORY.as_ref()) {
            Ok(file) => file,
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(invalid_data(
                "Directorio de marcadores MCP externo no es privado",
            ));
        }
        Ok(Some(Self { file }))
    }

    fn read(&self, name: &str) -> io::Result<File> {
        open_file_at(&self.file, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
    }

    fn create(&self, name: &str) -> io::Result<File> {
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

    fn publish(&self, temporary: &str, destination: &str) -> io::Result<()> {
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

    fn remove(&self, name: &str) -> io::Result<()> {
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

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
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
struct Directory {
    path: PathBuf,
}

// The portable fallback verifies each path component and opens the final
// Windows file as a reparse point, rejecting it before reading. It inherits
// the session store's user-profile ACL. Unlike Unix openat handles, these
// pathname checks do not guarantee identity against parent-renaming races.
#[cfg(not(unix))]
impl Directory {
    fn open(storage_root: &Path, create: bool) -> io::Result<Option<Self>> {
        let storage_root = if storage_root.is_absolute() {
            storage_root.to_owned()
        } else {
            std::env::current_dir()?.join(storage_root)
        };
        validate_directory_path(&storage_root)?;
        let path = storage_root.join(DIRECTORY);
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

    fn read(&self, name: &str) -> io::Result<File> {
        portable_options().read(true).open(self.checked_path(name)?)
    }

    fn create(&self, name: &str) -> io::Result<File> {
        portable_options()
            .write(true)
            .create_new(true)
            .open(self.checked_path(name)?)
    }

    fn publish(&self, temporary: &str, destination: &str) -> io::Result<()> {
        // Hard links publish create-only on NTFS and other supporting local
        // filesystems. Never use rename with replacement on Windows.
        let temporary = self.checked_path(temporary)?;
        let destination = self.checked_path(destination)?;
        std::fs::hard_link(temporary, &destination)?;
        // Windows has no portable directory fsync. Flush the published file;
        // this preserves the strongest durability available through std here.
        portable_options().write(true).open(destination)?.sync_all()
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        match std::fs::remove_file(self.checked_path(name)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn sync(&self) -> io::Result<()> {
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
                        "Un padre del marcador MCP externo no es directorio",
                    ));
                }
            }
            Component::CurDir => {}
            _ => return Err(invalid_data("Ruta del registro MCP externo no válida")),
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
            "El marcador MCP externo no admite enlaces ni reparse points",
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
