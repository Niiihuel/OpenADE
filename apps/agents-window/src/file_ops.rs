//! What the file tree may do to the disk. Every target is a plain relative
//! path under the canonical workspace root, symlinks are never followed,
//! nothing is overwritten and deleting only moves to the system trash.
//! Messages are for the user: Spanish, and without absolute paths.
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Moves one absolute path to the trash. A plain function, so a test can
/// stand in for the system's.
pub type Trash = fn(&Path) -> Result<(), String>;

pub fn system_trash(path: &Path) -> Result<(), String> {
    // The crate's own message can name paths outside the project.
    trash::delete(path).map_err(|_| "la papelera del sistema no está disponible".to_owned())
}

/// One operation; every path is relative to the workspace root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    CreateFile { parent: PathBuf, name: String },
    CreateDir { parent: PathBuf, name: String },
    Rename { path: PathBuf, name: String },
    Trash { path: PathBuf },
}

/// Why an operation stopped, and what it had already created by then.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub created: Vec<PathBuf>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            created: Vec::new(),
        }
    }
}

/// Runs `op` and returns the relative paths it created.
pub fn run(root: &Path, op: &Op, trash: Trash) -> Result<Vec<PathBuf>, Failure> {
    Ok(match op {
        Op::CreateFile { parent, name } => vec![create(root, parent, name, false)?],
        Op::CreateDir { parent, name } => vec![create(root, parent, name, true)?],
        Op::Rename { path, name } => vec![rename(root, path, name)?],
        Op::Trash { path } => {
            trash_path(root, path, trash)?;
            Vec::new()
        }
    })
}

pub fn valid_name(name: &str) -> Result<(), String> {
    Err(if name.trim().is_empty() {
        "Escribe un nombre"
    } else if name == "." || name == ".." {
        "«.» y «..» no son nombres válidos"
    } else if name.contains(['/', '\\', '\0']) {
        "El nombre no puede contener separadores de ruta"
    } else {
        return Ok(());
    }
    .to_owned())
}

/// The cause of an I/O error without the path the system may attach to it.
fn reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "ya no existe".into(),
        io::ErrorKind::AlreadyExists => "ya existe un elemento con ese nombre".into(),
        io::ErrorKind::PermissionDenied => "permiso denegado".into(),
        io::ErrorKind::StorageFull => "no queda espacio en el disco".into(),
        io::ErrorKind::DirectoryNotEmpty => "la carpeta de destino no está vacía".into(),
        kind => format!("error del sistema ({kind:?})"),
    }
}

/// The reason alone, as a sentence: it is shown on the row being named.
fn sentence(error: &io::Error) -> String {
    let reason = reason(error);
    let mut letters = reason.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

fn failed(what: &str, error: &io::Error) -> String {
    format!("{what}: {}", reason(error))
}

/// The absolute path of `relative` inside the canonical root. Only plain
/// names are accepted, a symlinked ancestor is refused instead of followed,
/// and the last component is not resolved at all.
fn locate(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    const OUTSIDE: &str = "La ruta queda fuera del proyecto";
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(OUTSIDE.into());
    }
    let root = root
        .canonicalize()
        .map_err(|error| failed("No se pudo leer el proyecto", &error))?;
    if relative.as_os_str().is_empty() {
        return Ok(root);
    }
    let target = root.join(relative);
    let parent = target.parent().unwrap_or(&root);
    let resolved = parent
        .canonicalize()
        .map_err(|error| failed("No se pudo leer la carpeta", &error))?;
    if resolved != parent {
        return Err(OUTSIDE.into());
    }
    Ok(target)
}

fn create(root: &Path, parent: &Path, name: &str, directory: bool) -> Result<PathBuf, String> {
    valid_name(name)?;
    let folder = locate(root, parent)?;
    // A link to a folder is a link, not a place to create things in.
    if !fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("La carpeta de destino ya no existe".into());
    }
    let target = folder.join(name);
    // Both calls fail on anything already there, a dangling link included.
    if directory {
        fs::create_dir(&target)
    } else {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map(drop)
    }
    .map_err(|error| sentence(&error))?;
    Ok(parent.join(name))
}

fn rename(root: &Path, path: &Path, name: &str) -> Result<PathBuf, String> {
    valid_name(name)?;
    if path.file_name().is_none() {
        return Err("La carpeta del proyecto no se puede renombrar".into());
    }
    let source = locate(root, path)?;
    fs::symlink_metadata(&source).map_err(|error| sentence(&error))?;
    move_without_replacing(&source, &source.with_file_name(name))
        .map_err(|error| sentence(&error))?;
    Ok(path.with_file_name(name))
}

fn trash_path(root: &Path, path: &Path, trash: Trash) -> Result<(), String> {
    if path.file_name().is_none() {
        return Err("La carpeta del proyecto no se puede eliminar".into());
    }
    let target = locate(root, path)?;
    fs::symlink_metadata(&target).map_err(|error| failed("No se pudo eliminar", &error))?;
    trash(&target).map_err(|error| format!("No se pudo enviar a la papelera: {error}"))
}

/// A rename that fails when `target` exists instead of replacing it.
fn move_without_replacing(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_path = |path: &Path| {
            std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
        };
        let (from, to) = (c_path(source)?, c_path(target)?);
        // SAFETY: both pointers are NUL-terminated paths that outlive the call.
        let moved = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if moved == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        // A filesystem without the flag takes the checked rename below.
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(error);
        }
    }
    // shortcut: between this check and the rename another process can create
    // `target` and lose it; only Linux has the atomic form wired above.
    if fs::symlink_metadata(target).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    fs::rename(source, target)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Stands in for the system trash: the entry leaves its place for a
    /// sibling named `<name>.trashed`.
    pub(crate) fn fake_trash(path: &Path) -> Result<(), String> {
        let mut name = path.file_name().unwrap().to_owned();
        name.push(".trashed");
        fs::rename(path, path.with_file_name(name)).map_err(|error| error.to_string())
    }
    pub(crate) fn broken_trash(_: &Path) -> Result<(), String> {
        Err("la papelera del sistema no está disponible".into())
    }

    fn project() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "main").unwrap();
        fs::write(root.join("README.md"), "readme").unwrap();
        fs::write(temp.path().join("outside.txt"), "outside").unwrap();
        (temp, root)
    }

    fn new_file(parent: &str, name: &str) -> Op {
        Op::CreateFile {
            parent: parent.into(),
            name: name.into(),
        }
    }
    fn new_dir(parent: &str, name: &str) -> Op {
        Op::CreateDir {
            parent: parent.into(),
            name: name.into(),
        }
    }
    fn renamed(path: &str, name: &str) -> Op {
        Op::Rename {
            path: path.into(),
            name: name.into(),
        }
    }
    fn trashed(path: &str) -> Op {
        Op::Trash { path: path.into() }
    }

    /// The failure of `op`, which must not name the temporary directory.
    fn refused(temp: &tempfile::TempDir, root: &Path, op: Op) -> String {
        let failure = run(root, &op, fake_trash).expect_err(&format!("{op:?} must be refused"));
        assert!(failure.created.is_empty(), "{op:?}");
        let message = failure.message;
        let leaked = temp.path().to_string_lossy().into_owned();
        assert!(!message.contains(&leaked), "{message} leaks an absolute path");
        message
    }

    #[test]
    fn names_reject_empty_dots_separators_and_nul() {
        let (temp, root) = project();
        for name in ["", "  ", ".", "..", "a/b", "a\\b", "/abs", "nul\0byte"] {
            assert!(valid_name(name).is_err(), "{name:?}");
            for op in [
                new_file("", name),
                new_dir("src", name),
                renamed("README.md", name),
            ] {
                refused(&temp, &root, op);
            }
        }
        for name in ["a", ".env", "con espacio.txt", "ñandú…"] {
            assert!(valid_name(name).is_ok(), "{name:?}");
        }
        // Nothing was created or moved by any of the refusals.
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        assert_eq!(fs::read_dir(root.join("src")).unwrap().count(), 1);
    }

    #[test]
    fn every_operation_refuses_a_target_outside_the_workspace() {
        let (temp, root) = project();
        for path in ["..", "../outside.txt", "src/../..", "/etc", "./src"] {
            for op in [
                new_file(path, "x"),
                new_dir(path, "x"),
                renamed(path, "x"),
                trashed(path),
            ] {
                refused(&temp, &root, op);
            }
        }
        assert_eq!(
            fs::read_to_string(temp.path().join("outside.txt")).unwrap(),
            "outside"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
        assert!(!root.join("src/x").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_operated_on_as_links_and_never_followed() {
        let (temp, root) = project();
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("src"), root.join("inner")).unwrap();
        // Through a link, even one that stays inside the project, nothing happens.
        for path in ["link", "inner"] {
            refused(&temp, &root, new_file(path, "x"));
            refused(&temp, &root, new_dir(path, "x"));
        }
        for path in ["link/secret", "inner/main.rs"] {
            refused(&temp, &root, renamed(path, "x"));
            refused(&temp, &root, trashed(path));
        }
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
        assert_eq!(fs::read_dir(root.join("src")).unwrap().count(), 1);
        // The link itself is renamed and trashed; what it points at stays.
        assert_eq!(
            run(&root, &renamed("link", "alias"), fake_trash).unwrap(),
            [PathBuf::from("alias")]
        );
        assert!(fs::symlink_metadata(root.join("alias"))
            .unwrap()
            .file_type()
            .is_symlink());
        run(&root, &trashed("alias"), fake_trash).unwrap();
        assert!(fs::symlink_metadata(root.join("alias.trashed"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(elsewhere.join("secret")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn creating_never_overwrites_what_is_already_there() {
        let (temp, root) = project();
        assert_eq!(
            run(&root, &new_file("src", "lib.rs"), fake_trash).unwrap(),
            [PathBuf::from("src/lib.rs")]
        );
        assert_eq!(fs::read_to_string(root.join("src/lib.rs")).unwrap(), "");
        assert_eq!(
            run(&root, &new_dir("", "docs"), fake_trash).unwrap(),
            [PathBuf::from("docs")]
        );
        assert!(root.join("docs").is_dir());
        for op in [
            new_file("", "README.md"),
            new_dir("", "README.md"),
            new_file("", "src"),
            new_dir("", "src"),
        ] {
            assert!(refused(&temp, &root, op).contains("Ya existe"));
        }
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "readme");
        assert!(root.join("src/main.rs").is_file());
        // A parent that is a file or is gone is not created on the way.
        refused(&temp, &root, new_file("README.md", "x"));
        refused(&temp, &root, new_dir("gone/deeper", "x"));
        assert!(!root.join("gone").exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path().join("nowhere"), root.join("dangling")).unwrap();
            refused(&temp, &root, new_file("", "dangling"));
            assert!(!temp.path().join("nowhere").exists());
        }
    }

    #[test]
    fn renaming_stays_in_the_parent_and_never_replaces_an_existing_entry() {
        let (temp, root) = project();
        assert_eq!(
            run(&root, &renamed("src/main.rs", "app.rs"), fake_trash).unwrap(),
            [PathBuf::from("src/app.rs")]
        );
        assert_eq!(fs::read_to_string(root.join("src/app.rs")).unwrap(), "main");
        assert!(!root.join("src/main.rs").exists());
        fs::write(root.join("src/other.rs"), "other").unwrap();
        for (path, name) in [
            ("src/app.rs", "other.rs"),
            ("src/app.rs", "app.rs"),
            ("README.md", "src"),
            ("src", "README.md"),
        ] {
            assert!(refused(&temp, &root, renamed(path, name)).contains("Ya existe"));
        }
        assert_eq!(fs::read_to_string(root.join("src/app.rs")).unwrap(), "main");
        assert_eq!(
            fs::read_to_string(root.join("src/other.rs")).unwrap(),
            "other"
        );
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "readme");
        refused(&temp, &root, renamed("gone.rs", "x"));
        assert!(refused(&temp, &root, renamed("", "x")).contains("proyecto"));
        assert!(root.is_dir());
    }

    #[test]
    fn deleting_only_goes_through_the_trash_and_a_failure_leaves_everything() {
        let (temp, root) = project();
        assert!(refused(&temp, &root, trashed("")).contains("proyecto"));
        refused(&temp, &root, trashed("gone.rs"));
        let failure = run(&root, &trashed("src"), broken_trash).unwrap_err();
        assert_eq!(
            failure.message,
            "No se pudo enviar a la papelera: la papelera del sistema no está disponible"
        );
        // No fallback: the folder and its contents are still there.
        assert!(root.join("src/main.rs").is_file());
        assert!(run(&root, &trashed("src"), fake_trash).unwrap().is_empty());
        assert!(!root.join("src").exists());
        assert!(root.join("src.trashed/main.rs").is_file());
    }
}
