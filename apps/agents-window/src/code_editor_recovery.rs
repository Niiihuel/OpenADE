//! Private, checksummed editor drafts. All operations run on the file worker.
use super::*;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};

const HEADER_LIMIT: usize = 16 * 1024;
const DRAFT_LIMIT: usize = 16;

#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    workspace: PathBuf,
    relative_path: PathBuf,
    base_digest: Option<EditorFileDigest>,
    anchor: usize,
    head: usize,
    bytes: usize,
    digest: EditorFileDigest,
}

#[derive(Debug)]
pub struct RecoveredDraft {
    pub workspace: PathBuf,
    pub relative_path: PathBuf,
    pub base_digest: Option<EditorFileDigest>,
    pub buffer: TextBuffer,
}

pub(super) struct RecoveryStore {
    root: PathBuf,
    directory: File,
    // A second running application must not replace another instance's drafts.
    _lease: File,
}

impl Drop for RecoveryStore {
    fn drop(&mut self) {
        // CLOEXEC closes the descriptor at exec, but a concurrently spawned
        // process can briefly inherit the same open file description at fork.
        // Unlock explicitly so ending this store does not depend on that child
        // reaching exec (or on any other duplicate descriptor being closed).
        let _ = self._lease.unlock();
    }
}

fn key(workspace: &Path, relative: &Path) -> Result<String, EditorFileError> {
    let bytes =
        serde_json::to_vec(&(workspace, relative)).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(format!("{:x}.draft", Sha256::digest(bytes)))
}

impl RecoveryStore {
    pub(super) fn open(root: PathBuf) -> Result<Self, EditorFileError> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let directory = open_directory(&root)?;
        #[cfg(unix)]
        if directory.metadata()?.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "La carpeta de recuperación debe ser privada (0700)",
            )
            .into());
        }
        let lock_name = std::ffi::OsStr::new(".lock");
        let lease = match create_sibling(&directory, lock_name, &root.join(lock_name)) {
            Ok(file) => file,
            Err(EditorFileError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            }) => open_regular_file(&directory, lock_name, &root.join(lock_name))?,
            Err(error) => return Err(error),
        };
        lease.try_lock().map_err(|e| {
            io::Error::other(format!(
                "Otra ventana está usando la recuperación del editor: {e}"
            ))
        })?;
        Ok(Self {
            root,
            directory,
            _lease: lease,
        })
    }

    fn target(&self, workspace: &Path, relative: &Path) -> Result<ResolvedFile, EditorFileError> {
        relative_components(relative)?;
        if !workspace.is_absolute() {
            return Err(EditorFileError::InvalidScope);
        }
        let name = std::ffi::OsString::from(key(workspace, relative)?);
        Ok(ResolvedFile {
            canonical_workspace: self.root.clone(),
            canonical_path: self.root.join(&name),
            parent: self.directory.try_clone()?,
            name,
        })
    }

    pub(super) fn store(
        &self,
        scope: &EditorFileScope,
        base_digest: Option<EditorFileDigest>,
        snapshot: &BufferSnapshot,
    ) -> Result<(), EditorFileError> {
        if snapshot.byte_count() > MAX_EDITABLE_FILE_BYTES {
            return Err(EditorFileError::TooLarge);
        }
        let target = self.target(&scope.workspace, &scope.relative_path)?;
        let mut temporary = target.create_temporary()?;
        // Content materialization happens here, never on the event/render thread.
        let text = snapshot
            .text_range(0..snapshot.char_count())
            .map_err(|e| EditorFileError::Buffer(e.to_string()))?;
        let selection = snapshot.selection();
        let header = Header {
            version: 1,
            workspace: scope.workspace.clone(),
            relative_path: scope.relative_path.clone(),
            base_digest,
            anchor: selection.anchor,
            head: selection.head,
            bytes: text.len(),
            digest: digest_bytes(text.as_bytes()),
        };
        let bytes = serde_json::to_vec(&header).map_err(|e| io::Error::other(e.to_string()))?;
        if bytes.len() >= HEADER_LIMIT {
            return Err(EditorFileError::InvalidScope);
        }
        temporary.file.write_all(&bytes)?;
        temporary.file.write_all(b"\n")?;
        temporary.file.write_all(text.as_bytes())?;
        temporary.file.sync_all()?;
        temporary.commit(&target)?;
        self.directory.sync_all()?;
        Ok(())
    }

    pub(super) fn remove(&self, scope: &EditorFileScope) -> Result<(), EditorFileError> {
        let target = self.target(&scope.workspace, &scope.relative_path)?;
        #[cfg(unix)]
        {
            let name = c_name(&target.name)?;
            // SAFETY: directory descriptor is owned and the filename is terminated.
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e.into());
                }
            }
        }
        #[cfg(not(unix))]
        match fs::remove_file(&target.canonical_path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.directory.sync_all()?;
        Ok(())
    }

    pub(super) fn load(&self) -> (Vec<RecoveredDraft>, Vec<String>) {
        let mut drafts = Vec::new();
        let mut errors = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) => return (drafts, vec![e.to_string()]),
        };
        for (index, entry) in entries.enumerate() {
            if index >= 128 {
                errors.push("Hay demasiadas entradas de recuperación".into());
                break;
            }
            let result: Result<Option<RecoveredDraft>, EditorFileError> = (|| {
                let entry = entry?;
                let name = entry.file_name();
                if !name.to_string_lossy().ends_with(".draft") {
                    return Ok(None);
                }
                if drafts.len() >= DRAFT_LIMIT {
                    return Err(io::Error::other(
                        "Se alcanzó el límite de borradores recuperables",
                    )
                    .into());
                }
                let file = open_regular_file(&self.directory, &name, &self.root.join(&name))?;
                #[cfg(unix)]
                if file.metadata()?.mode() & 0o077 != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "El borrador no es privado",
                    )
                    .into());
                }
                if file.metadata()?.len() > (HEADER_LIMIT + MAX_EDITABLE_FILE_BYTES) as u64 {
                    return Err(EditorFileError::TooLarge);
                }
                let mut reader = BufReader::new(file);
                let mut header = Vec::new();
                reader
                    .by_ref()
                    .take(HEADER_LIMIT as u64)
                    .read_until(b'\n', &mut header)?;
                if header.last() != Some(&b'\n') {
                    return Err(io::Error::other("Cabecera de borrador incompleta").into());
                }
                let header: Header =
                    serde_json::from_slice(&header).map_err(|e| io::Error::other(e.to_string()))?;
                if header.version != 1
                    || header.bytes > MAX_EDITABLE_FILE_BYTES
                    || name != std::ffi::OsStr::new(&key(&header.workspace, &header.relative_path)?)
                {
                    return Err(EditorFileError::InvalidScope);
                }
                relative_components(&header.relative_path)?;
                if !header.workspace.is_absolute() {
                    return Err(EditorFileError::InvalidScope);
                }
                let mut text = String::new();
                reader
                    .take((MAX_EDITABLE_FILE_BYTES + 1) as u64)
                    .read_to_string(&mut text)?;
                if text.len() != header.bytes || digest_bytes(text.as_bytes()) != header.digest {
                    return Err(io::Error::other(
                        "El borrador está incompleto o su checksum no coincide",
                    )
                    .into());
                }
                let buffer = TextBuffer::restore_unsaved(
                    &text,
                    openide_editor::Selection {
                        anchor: header.anchor,
                        head: header.head,
                    },
                    EditorLimits {
                        max_document_bytes: MAX_EDITABLE_FILE_BYTES,
                        ..EditorLimits::default()
                    },
                )
                .map_err(|e| EditorFileError::Buffer(e.to_string()))?;
                Ok(Some(RecoveredDraft {
                    workspace: header.workspace,
                    relative_path: header.relative_path,
                    base_digest: header.base_digest,
                    buffer,
                }))
            })();
            match result {
                Ok(Some(draft)) => drafts.push(draft),
                Ok(None) => {}
                Err(error) => errors.push(format!("Borrador conservado sin cargar: {error}")),
            }
        }
        (drafts, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::Selection;

    fn scope(root: &Path) -> EditorFileScope {
        EditorFileScope {
            tab_id: 1,
            generation: 1,
            workspace: root.to_path_buf(),
            relative_path: "src/main.rs".into(),
        }
    }
    #[test]
    fn recovery_is_atomic_private_bounded_and_lease_protected() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("drafts");
        let store = RecoveryStore::open(directory.clone()).unwrap();
        assert!(
            RecoveryStore::open(directory.clone()).is_err(),
            "live instances must not share mutable recovery"
        );
        let buffer = TextBuffer::restore_unsaved(
            "café 🌎\r\n",
            Selection { anchor: 1, head: 5 },
            EditorLimits::default(),
        )
        .unwrap();
        store
            .store(&scope(temp.path()), Some([42; 32]), &buffer.snapshot())
            .unwrap();
        let (recovered, errors) = store.load();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].base_digest, Some([42; 32]));
        assert_eq!(recovered[0].buffer.selection(), buffer.selection());
        assert!(recovered[0].buffer.is_dirty());
        #[cfg(unix)]
        {
            assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
            let target = store.target(temp.path(), Path::new("src/main.rs")).unwrap();
            assert_eq!(
                fs::metadata(&target.canonical_path).unwrap().mode() & 0o777,
                0o600
            );
        }
        store.remove(&scope(temp.path())).unwrap();
        assert!(store.load().0.is_empty());
        // A duplicate models the shared open file description inherited by
        // fork, without forking a multithreaded test process.
        let inherited_lease = store._lease.try_clone().unwrap();
        drop(store);
        let reopened = RecoveryStore::open(directory.clone())
            .unwrap_or_else(|error| panic!("releasing the store must release its lease: {error}"));
        drop(inherited_lease);
        assert!(RecoveryStore::open(directory.clone()).is_err());
        drop(reopened);
        assert!(RecoveryStore::open(directory).is_ok());
    }

    #[test]
    fn corrupt_or_partial_recovery_is_reported_and_never_deleted_or_replayed() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::open(temp.path().join("drafts")).unwrap();
        let scope = scope(temp.path());
        let snapshot = TextBuffer::new("original").unwrap().snapshot();
        store.store(&scope, Some([1; 32]), &snapshot).unwrap();
        let path = store
            .target(&scope.workspace, &scope.relative_path)
            .unwrap()
            .canonical_path;
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&path, &bytes).unwrap();
        let (drafts, errors) = store.load();
        assert!(drafts.is_empty());
        assert!(!errors.is_empty());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(&path, b"{partial").unwrap();
        assert!(store.load().0.is_empty());
        assert_eq!(fs::read(path).unwrap(), b"{partial");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlink_directory_and_symlink_record_without_following() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("alias")).unwrap();
        assert!(RecoveryStore::open(temp.path().join("alias")).is_err());
        let store = RecoveryStore::open(temp.path().join("drafts")).unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, "untouched").unwrap();
        let target = store.target(temp.path(), Path::new("src/main.rs")).unwrap();
        std::os::unix::fs::symlink(&victim, &target.canonical_path).unwrap();
        let (drafts, errors) = store.load();
        assert!(drafts.is_empty() && !errors.is_empty());
        assert_eq!(fs::read_to_string(victim).unwrap(), "untouched");
    }
}
