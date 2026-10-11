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
    /// Copies `sources` into the folder `into`, or moves them when `cut`.
    Paste {
        sources: Vec<PathBuf>,
        cut: bool,
        into: PathBuf,
    },
}

/// A copy stops rather than follow a folder without end.
const MAX_COPY_DEPTH: usize = 64;
const MAX_COPY_ENTRIES: usize = 20_000;

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
        Op::Paste { sources, cut, into } => {
            let mut created = Vec::new();
            for source in sources {
                if let Err(message) = paste(root, source, *cut, into, trash, &mut created) {
                    return Err(Failure { message, created });
                }
            }
            created
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
    // Only this module's own limits carry a message; the system's do not.
    if let Some(limit) = error.get_ref() {
        return limit.to_string();
    }
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

/// Pastes one entry into `into`, adding to `created` whatever it starts to
/// create. A copy never replaces: a taken name gets " copia", " copia 2"…
/// A cut moves, and across filesystems copies and then trashes the original.
fn paste(
    root: &Path,
    source: &Path,
    cut: bool,
    into: &Path,
    trash: Trash,
    created: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let Some(name) = source.file_name().map(|name| name.to_string_lossy()) else {
        return Err("La carpeta del proyecto no se puede copiar ni mover".into());
    };
    let from = locate(root, source)?;
    let folder = locate(root, into)?;
    let metadata =
        fs::symlink_metadata(&from).map_err(|error| failed("No se pudo pegar", &error))?;
    if !fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("La carpeta de destino ya no existe".into());
    }
    if metadata.is_dir() && into.starts_with(source) {
        return Err("Una carpeta no se puede pegar dentro de sí misma".into());
    }
    if cut && source.parent() == Some(into) {
        // It is already there: moving it would only rename it.
        return Ok(());
    }
    let name = free_name(&folder, &name, metadata.is_dir())?;
    let (target, relative) = (folder.join(&name), into.join(&name));
    if cut {
        match move_without_replacing(&from, &target) {
            Ok(()) => {
                created.push(relative);
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(failed("No se pudo mover", &error)),
        }
    }
    // Named before it is complete, so a copy that stops is still reported.
    created.push(relative);
    copy_across(&from, &target, cut.then_some(trash))
}

/// Copies `from` to `target`; a move between filesystems then sends the
/// original to the trash, and never deletes it.
fn copy_across(from: &Path, target: &Path, discard: Option<Trash>) -> Result<(), String> {
    copy_entry(from, target, 0, &mut MAX_COPY_ENTRIES.clone())
        .map_err(|error| failed("No se pudo copiar", &error))?;
    let Some(trash) = discard else {
        return Ok(());
    };
    trash(from)
        .map_err(|error| format!("Se copió, pero el original no fue a la papelera: {error}"))
}

/// `name` when nothing in `folder` has it, else the first free of
/// "name copia", "name copia 2"…, kept before a file's extension.
fn free_name(folder: &Path, name: &str, is_dir: bool) -> Result<String, String> {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    (0..1000)
        .map(|attempt| match attempt {
            0 => name.to_owned(),
            1 => format!("{stem} copia{extension}"),
            _ => format!("{stem} copia {attempt}{extension}"),
        })
        .find(|candidate| fs::symlink_metadata(folder.join(candidate)).is_err())
        .ok_or_else(|| "Ya hay demasiadas copias con ese nombre".to_owned())
}

/// Copies a file, a folder with its contents, or a link as a link. Nothing
/// is followed and nothing existing is written over.
fn copy_entry(from: &Path, to: &Path, depth: usize, budget: &mut usize) -> io::Result<()> {
    let limit = |message: &'static str| io::Error::other(message);
    *budget = budget
        .checked_sub(1)
        .ok_or_else(|| limit("la carpeta tiene demasiados elementos"))?;
    let metadata = fs::symlink_metadata(from)?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(fs::read_link(from)?, to);
        #[cfg(not(unix))]
        return Err(limit("los enlaces no se pueden copiar en este sistema"));
    }
    if kind.is_dir() {
        if depth >= MAX_COPY_DEPTH {
            return Err(limit("la carpeta tiene demasiados niveles"));
        }
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_entry(&entry.path(), &to.join(entry.file_name()), depth + 1, budget)?;
        }
    } else if kind.is_file() {
        let mut source = fs::OpenOptions::new();
        source.read(true);
        // A file swapped for a link after the check above is not followed.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut source, libc::O_NOFOLLOW);
        let mut target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        io::copy(&mut source.open(from)?, &mut target)?;
    } else {
        return Err(limit("solo se copian archivos, carpetas y enlaces"));
    }
    // Last, so a read-only folder can still receive its contents.
    fs::set_permissions(to, metadata.permissions())
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

    fn pasted(sources: &[&str], cut: bool, into: &str) -> Op {
        Op::Paste {
            sources: sources.iter().map(PathBuf::from).collect(),
            cut,
            into: into.into(),
        }
    }
    fn read(root: &Path, path: &str) -> String {
        fs::read_to_string(root.join(path)).unwrap()
    }

    #[test]
    fn a_copy_never_replaces_and_takes_the_next_free_copia_name() {
        let (_temp, root) = project();
        let paste = |sources: &[&str], into: &str| {
            run(&root, &pasted(sources, false, into), fake_trash).unwrap()
        };
        assert_eq!(paste(&["README.md"], "src"), [PathBuf::from("src/README.md")]);
        assert_eq!(paste(&["README.md"], "src"), [PathBuf::from("src/README copia.md")]);
        assert_eq!(paste(&["README.md"], "src"), [PathBuf::from("src/README copia 2.md")]);
        // Into its own folder is a duplicate; a folder keeps its whole name.
        assert_eq!(paste(&["README.md"], ""), [PathBuf::from("README copia.md")]);
        fs::write(root.join(".env"), "secreto").unwrap();
        assert_eq!(paste(&[".env"], ""), [PathBuf::from(".env copia")]);
        fs::create_dir(root.join("v1.2")).unwrap();
        assert_eq!(paste(&["v1.2"], ""), [PathBuf::from("v1.2 copia")]);
        assert_eq!(
            paste(&["src"], ""),
            [PathBuf::from("src copia")],
            "a folder is copied with its contents"
        );
        for copy in ["src/README.md", "src/README copia 2.md", "src copia/README copia.md"] {
            assert_eq!(read(&root, copy), "readme");
        }
        assert_eq!(read(&root, "src copia/main.rs"), "main");
        // The originals are all as they were.
        assert_eq!(read(&root, "README.md"), "readme");
        assert_eq!(read(&root, "src/main.rs"), "main");
    }

    #[test]
    fn a_folder_is_never_pasted_into_itself_or_one_of_its_folders() {
        let (temp, root) = project();
        fs::create_dir_all(root.join("src/deep/deeper")).unwrap();
        for into in ["src", "src/deep", "src/deep/deeper"] {
            for cut in [false, true] {
                let message = refused(&temp, &root, pasted(&["src"], cut, into));
                assert!(message.contains("dentro de sí misma"), "{message}");
            }
        }
        assert_eq!(fs::read_dir(root.join("src")).unwrap().count(), 2);
        assert_eq!(fs::read_dir(root.join("src/deep/deeper")).unwrap().count(), 0);
        // A sibling whose name merely starts the same is another folder.
        fs::create_dir(root.join("src-old")).unwrap();
        run(&root, &pasted(&["src"], false, "src-old"), fake_trash).unwrap();
        assert_eq!(read(&root, "src-old/src/main.rs"), "main");
    }

    #[test]
    fn a_cut_moves_without_replacing_and_only_when_it_is_pasted() {
        let (temp, root) = project();
        fs::write(root.join("src/README.md"), "other").unwrap();
        assert_eq!(
            run(&root, &pasted(&["README.md"], true, "src"), fake_trash).unwrap(),
            [PathBuf::from("src/README copia.md")]
        );
        assert_eq!(read(&root, "src/README.md"), "other");
        assert_eq!(read(&root, "src/README copia.md"), "readme");
        assert!(!root.join("README.md").exists());
        // Back into the folder it is already in: nothing to move.
        assert!(run(&root, &pasted(&["src/main.rs"], true, "src"), fake_trash)
            .unwrap()
            .is_empty());
        assert_eq!(read(&root, "src/main.rs"), "main");
        // Outside, missing and root sources are refused and nothing moves.
        for (sources, into) in [
            (&["../outside.txt"][..], ""),
            (&["src/main.rs"], ".."),
            (&["gone.rs"], "src"),
            (&[""], "src"),
            (&["src/main.rs"], "src/main.rs"),
        ] {
            for cut in [false, true] {
                refused(&temp, &root, pasted(sources, cut, into));
            }
        }
        assert_eq!(read(&root, "src/main.rs"), "main");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_move_between_filesystems_copies_and_then_trashes_the_original() {
        let (_temp, root) = project();
        // The trash refusing leaves both: the copy is reported, nothing is deleted.
        let message = copy_across(&root.join("src"), &root.join("moved"), Some(broken_trash));
        assert_eq!(
            message.unwrap_err(),
            "Se copió, pero el original no fue a la papelera: la papelera del sistema no está disponible"
        );
        assert_eq!(read(&root, "src/main.rs"), "main");
        assert_eq!(read(&root, "moved/main.rs"), "main");
        copy_across(&root.join("src"), &root.join("again"), Some(fake_trash)).unwrap();
        assert_eq!(read(&root, "again/main.rs"), "main");
        assert!(!root.join("src").exists());
        assert_eq!(read(&root, "src.trashed/main.rs"), "main");
        // An existing target stops the copy before anything is trashed.
        let taken = copy_across(&root.join("again"), &root.join("moved"), Some(fake_trash));
        assert_eq!(
            taken.unwrap_err(),
            "No se pudo copiar: ya existe un elemento con ese nombre"
        );
        assert_eq!(read(&root, "again/main.rs"), "main");
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_keeps_links_as_links_and_never_reads_through_them() {
        let (temp, root) = project();
        std::os::unix::fs::symlink(temp.path().join("outside.txt"), root.join("src/link")).unwrap();
        std::os::unix::fs::symlink(temp.path(), root.join("src/up")).unwrap();
        run(&root, &pasted(&["src"], false, ""), fake_trash).unwrap();
        for link in ["src copia/link", "src copia/up"] {
            let copy = root.join(link);
            assert!(fs::symlink_metadata(&copy).unwrap().file_type().is_symlink());
            assert_eq!(
                fs::read_link(&copy).unwrap(),
                fs::read_link(root.join(link.replace(" copia", ""))).unwrap()
            );
        }
        // A link is the thing copied when it is the source itself.
        assert_eq!(
            run(&root, &pasted(&["src/link"], false, ""), fake_trash).unwrap(),
            [PathBuf::from("link")]
        );
        assert!(fs::symlink_metadata(root.join("link")).unwrap().file_type().is_symlink());
        // And a link is never a folder to paste into.
        refused(&temp, &root, pasted(&["README.md"], false, "src/up"));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_copy_that_stops_leaves_the_source_and_reports_what_it_created() {
        let (_temp, root) = project();
        let mut deep = root.join("deep");
        for _ in 0..MAX_COPY_DEPTH + 2 {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("leaf.txt"), "leaf").unwrap();
        for cut in [false, true] {
            let failure = run(&root, &pasted(&["deep"], cut, "src"), fake_trash);
            if cut {
                // On one filesystem a cut is a rename and has no depth to walk.
                assert_eq!(failure.unwrap(), [PathBuf::from("src/deep")]);
                assert!(!root.join("deep").exists());
                continue;
            }
            let failure = failure.unwrap_err();
            assert_eq!(
                failure.message,
                "No se pudo copiar: la carpeta tiene demasiados niveles"
            );
            assert_eq!(failure.created, [PathBuf::from("src/deep")]);
            assert!(root.join("src/deep/d").is_dir(), "the partial copy is left");
            assert_eq!(fs::read_to_string(deep.join("leaf.txt")).unwrap(), "leaf");
            fs::remove_dir_all(root.join("src/deep")).unwrap();
        }
        // The entry budget stops a wide folder the same way.
        let error = copy_entry(&root.join("src"), &root.join("wide"), 0, &mut 1).unwrap_err();
        assert_eq!(reason(&error), "la carpeta tiene demasiados elementos");
        assert!(root.join("wide").is_dir() && !root.join("wide/deep").exists());
    }
}
