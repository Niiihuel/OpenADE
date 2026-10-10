//! Relative paths are validated again on the worker, including symlink scope.
use crate::MAX_FILE_BYTES;
use std::fs::File;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::MetadataExt,
    io::{AsRawFd, FromRawFd},
};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub canonical: PathBuf,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

pub(crate) struct ScopedFile {
    pub identity: FileIdentity,
    file: File,
}

pub(crate) fn validate_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("La ruta de vista previa debe estar dentro del proyecto.".into());
    }
    Ok(())
}

pub(crate) fn open(workspace: &Path, relative: &Path) -> Result<ScopedFile, String> {
    validate_relative(relative)?;
    if !workspace.is_absolute() {
        return Err("La carpeta del proyecto debe usar una ruta absoluta.".into());
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| "No se pudo abrir el proyecto.".to_owned())?;
    if root != workspace {
        return Err("La carpeta del proyecto cambió o contiene un enlace simbólico.".into());
    }
    let canonical = root.join(relative);
    let file = open_scoped(&root, relative)
        .map_err(|e| format!("No se pudo abrir el archivo sin seguir enlaces simbólicos: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("No se pudo leer el archivo: {e}"))?;
    if !metadata.is_file() {
        return Err("La vista previa requiere un archivo regular.".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("El archivo supera el límite de vista previa de 32 MiB.".into());
    }
    Ok(ScopedFile {
        identity: FileIdentity {
            canonical,
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        },
        file,
    })
}

#[cfg(unix)]
fn open_scoped(root: &Path, relative: &Path) -> std::io::Result<File> {
    // Pin every ancestor by descriptor. A rename or link substitution after
    // this step cannot redirect a later openat to another tree.
    let name = std::ffi::CString::new("/").unwrap();
    // SAFETY: name is NUL-terminated; a successful owned fd is immediately
    // transferred to exactly one File below.
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a newly owned valid descriptor from open.
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in root.components() {
        if let Component::Normal(name) = component {
            directory = open_child(&directory, name, true)?;
        }
    }
    let mut components = relative.components().peekable();
    while let Some(Component::Normal(name)) = components.next() {
        if components.peek().is_none() {
            return open_child(&directory, name, false);
        }
        directory = open_child(&directory, name, true)?;
    }
    Err(std::io::Error::other("empty relative path"))
}

#[cfg(unix)]
fn open_child(parent: &File, name: &std::ffi::OsStr, directory: bool) -> std::io::Result<File> {
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| std::io::Error::other("invalid path component"))?;
    // Nonblocking mode prevents a substituted FIFO from waiting for a writer
    // before the opened descriptor's metadata can reject non-regular files.
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent is a live directory descriptor and name is one validated,
    // NUL-terminated component. Flags request no creation and no link following.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn open_scoped(root: &Path, relative: &Path) -> std::io::Result<File> {
    // Best-effort fallback, matching the editor's non-Unix path checks. A
    // platform-native directory-handle walk is needed to close ancestor races.
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("symbolic link in preview path"));
        }
    }
    if !path.metadata()?.is_file() || path.canonicalize()? != path {
        return Err(std::io::Error::other("invalid regular file"));
    }
    let file = File::open(&path)?;
    // Recheck after open too; all type and byte-limit checks use this handle.
    if std::fs::symlink_metadata(&path)?.file_type().is_symlink() || path.canonicalize()? != path {
        return Err(std::io::Error::other("preview path changed"));
    }
    Ok(file)
}

impl ScopedFile {
    pub fn read(self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::with_capacity(self.identity.bytes.min(MAX_FILE_BYTES) as usize);
        self.file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("No se pudo leer el archivo: {e}"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("El archivo supera el límite de vista previa de 32 MiB.".into());
        }
        Ok(bytes)
    }
}

/// Resolve a Markdown asset relative to its document without permitting remote
/// URLs, absolute paths, or `..` escapes beyond the workspace root.
pub(crate) fn markdown_asset(document: &Path, destination: &str) -> Result<PathBuf, String> {
    let raw = destination.split('#').next().unwrap_or_default();
    let decoded = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| "La ruta de la imagen no es UTF-8.".to_owned())?;
    if decoded.is_empty()
        || decoded.contains(':')
        || decoded.contains('\0')
        || decoded.starts_with(['/', '\\'])
    {
        return Err("La vista previa carga imágenes locales del proyecto.".into());
    }
    let mut relative = document.parent().unwrap_or(Path::new("")).to_path_buf();
    for part in Path::new(decoded.as_ref()).components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            Component::ParentDir if relative.pop() => {}
            _ => return Err("La imagen apunta fuera del proyecto.".into()),
        }
    }
    validate_relative(&relative)?;
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_scope_rejects_parent_absolute_and_external_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("good.txt"), b"ok").unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
        assert!(open(root.path(), Path::new("../secret.txt")).is_err());
        assert!(open(root.path(), &outside.path().join("secret.txt")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                root.path().join("linked.txt"),
            )
            .unwrap();
            assert!(open(root.path(), Path::new("linked.txt")).is_err());
        }
        assert_eq!(
            open(root.path(), Path::new("good.txt"))
                .unwrap()
                .read()
                .unwrap(),
            b"ok"
        );
    }

    #[test]
    fn preview_file_limit_is_checked_before_reading() {
        let root = tempfile::tempdir().unwrap();
        File::create(root.path().join("large.pdf"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(open(root.path(), Path::new("large.pdf")).is_err());
    }

    #[test]
    fn preview_read_limit_survives_file_growth_after_opening() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("growing.pdf");
        let writer = File::create(&path).unwrap();
        let opened = open(root.path(), Path::new("growing.pdf")).unwrap();
        writer.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(opened.read().unwrap_err().contains("32 MiB"));
    }

    #[cfg(unix)]
    #[test]
    fn preview_rejects_internal_symlinks_and_fifo_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("real.png"), b"ok").unwrap();
        std::os::unix::fs::symlink("real.png", root.path().join("linked.png")).unwrap();
        assert!(open(root.path(), Path::new("linked.png")).is_err());
        let fifo = root.path().join("pipe.png");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name points to a valid path; mkfifo creates only a fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let workspace = root.path().to_path_buf();
        let thread = std::thread::spawn(move || {
            send.send(open(&workspace, Path::new("pipe.png")).is_err())
                .unwrap();
        });
        assert!(receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("FIFO opening blocked"));
        thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preview_descriptor_walk_never_follows_a_concurrently_substituted_parent() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let directory = root.path().join("images");
        let parked = root.path().join("parked");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("image.png"), b"inside").unwrap();
        std::fs::write(outside.path().join("image.png"), b"outside").unwrap();
        let target = outside.path().to_path_buf();
        let racing = std::thread::spawn(move || {
            for _ in 0..128 {
                std::fs::rename(&directory, &parked).unwrap();
                std::os::unix::fs::symlink(&target, &directory).unwrap();
                std::thread::yield_now();
                std::fs::remove_file(&directory).unwrap();
                std::fs::rename(&parked, &directory).unwrap();
            }
        });
        for _ in 0..256 {
            if let Ok(file) = open(root.path(), Path::new("images/image.png")) {
                assert_eq!(file.read().unwrap(), b"inside");
            }
        }
        racing.join().unwrap();
    }

    #[test]
    fn markdown_images_resolve_encoded_relative_paths_but_not_remote_or_escaping_paths() {
        let doc = Path::new("docs/readme.md");
        assert_eq!(
            markdown_asset(doc, "../images/my%20image.png").unwrap(),
            PathBuf::from("images/my image.png")
        );
        for value in [
            "../../outside.png",
            "/tmp/image.png",
            "https://example.com/x.png",
            "file:///tmp/x.png",
            "data:image/png,AAA",
        ] {
            assert!(markdown_asset(doc, value).is_err(), "{value}");
        }
    }
}
