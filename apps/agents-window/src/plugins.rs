//! Managed executable plugins. Installation copies a reviewed bundle without
//! running it. Each workspace explicitly enables its own MCP stdio adapters.
//! This is an OpenIDE package format, not a VSIX or Zed extension host.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mcp_settings::{McpServerSettings, McpSettings};

const MANIFEST: &str = "openide-plugin.json";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 2048;
const MAX_DIRECTORIES: usize = 512;
const MAX_DEPTH: usize = 32;
const MAX_PLUGINS: usize = 64;
const MAX_WORKSPACES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub publisher: String,
    /// A PATH executable with an entrypoint, or an explicitly relative bundle
    /// executable, e.g. `./bin/server`. Never a shell command string.
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub credential_env_names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub enabled: bool,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPreview {
    pub manifest: PluginManifest,
    pub digest: String,
    pub source: PathBuf,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    manifest: PluginManifest,
    digest: String,
    enabled_workspaces: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: u32,
    installed: Vec<Installation>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            installed: Vec::new(),
        }
    }
}

struct BundleFile {
    relative: PathBuf,
    bytes: Vec<u8>,
    executable: bool,
}

struct Bundle {
    preview: PluginPreview,
    directories: Vec<PathBuf>,
    files: Vec<BundleFile>,
}

pub(crate) struct PluginStore;

impl PluginStore {
    pub fn inspect(source: &Path) -> Result<PluginPreview, String> {
        Ok(read_bundle(source)?.preview)
    }

    /// Lists persisted identities, even if a package was modified or removed
    /// externally, so the user can still disable or uninstall it. Execution
    /// always verifies the installed bytes separately.
    pub fn list(root: &Path, workspace: &Path) -> Result<Vec<InstalledPlugin>, String> {
        let workspace = workspace_key(workspace)?;
        let store = store_path(root, false)?;
        Ok(load_registry(&store)?
            .installed
            .into_iter()
            .map(|item| InstalledPlugin {
                path: store.join("installed").join(&item.manifest.id),
                enabled: item.enabled_workspaces.contains(&workspace),
                manifest: item.manifest,
                digest: item.digest,
            })
            .collect())
    }

    pub fn install(root: &Path, workspace: &Path, preview: &PluginPreview) -> Result<(), String> {
        workspace_key(workspace)?;
        // The preview is an exact content observation, not permission to read
        // arbitrary newer files from the original directory.
        let bundle = read_bundle(&preview.source)?;
        if bundle.preview != *preview {
            return Err("El plugin cambió desde la revisión. Vuelve a revisar su carpeta.".into());
        }
        let store = store_path(root, true)?;
        let _lock = lock_store(&store)?;
        let mut registry = load_registry(&store)?;
        if registry
            .installed
            .iter()
            .any(|item| item.manifest.id == preview.manifest.id)
        {
            return Err(
                "Ya hay un plugin instalado con ese ID. Quita la versión anterior primero.".into(),
            );
        }
        if registry.installed.len() >= MAX_PLUGINS {
            return Err("Se alcanzó el límite de plugins instalados.".into());
        }
        let parent = store.join("installed");
        ensure_private_dir(&parent)?;
        let destination = parent.join(&preview.manifest.id);
        if exists_no_follow(&destination)? {
            return Err("La carpeta de instalación ya existe; no se sobrescribió.".into());
        }
        let staging = parent.join(format!(".install-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            ensure_private_dir(&staging)?;
            for relative in &bundle.directories {
                ensure_private_dir(&staging.join(relative))?;
            }
            for file in &bundle.files {
                let path = staging.join(&file.relative);
                let mut output = private_new_file(&path)?;
                output.write_all(&file.bytes).map_err(io_message)?;
                set_executable(&output, file.executable)?;
                output.sync_all().map_err(io_message)?;
            }
            let copied = read_bundle(&staging)?;
            if copied.preview.digest != preview.digest
                || copied.preview.manifest != preview.manifest
            {
                return Err("La copia del plugin no coincide con el contenido revisado.".into());
            }
            fs::rename(&staging, &destination).map_err(io_message)?;
            registry.installed.push(Installation {
                manifest: preview.manifest.clone(),
                digest: preview.digest.clone(),
                enabled_workspaces: BTreeSet::new(),
            });
            if let Err(error) = save_registry(&store, &registry) {
                let _ = fs::remove_dir_all(&destination);
                return Err(error);
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&staging);
        result
    }

    pub fn set_enabled(
        root: &Path,
        workspace: &Path,
        id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        validate_id(id)?;
        let workspace = workspace_key(workspace)?;
        let store = store_path(root, true)?;
        let _lock = lock_store(&store)?;
        let mut registry = load_registry(&store)?;
        let item = registry
            .installed
            .iter_mut()
            .find(|item| item.manifest.id == id)
            .ok_or_else(|| "El plugin no está instalado.".to_owned())?;
        if enabled {
            verify_installation(&store, item)?;
            if !item.enabled_workspaces.contains(&workspace)
                && item.enabled_workspaces.len() >= MAX_WORKSPACES
            {
                return Err("El plugin alcanzó el límite de proyectos activados.".into());
            }
            item.enabled_workspaces.insert(workspace.clone());
        } else {
            item.enabled_workspaces.remove(&workspace);
        }
        if enabled {
            let workspace = Path::new(&workspace);
            let mut settings = McpSettings::load(root, workspace)?;
            settings
                .servers
                .extend(runtime_servers_from_registry(&store, &registry, workspace)?);
            settings.validate(workspace)?;
        }
        save_registry(&store, &registry)
    }

    /// Removes only an ID-derived managed copy, never the original source.
    /// The host must defer this while a turn may still be using the package.
    pub fn uninstall(root: &Path, id: &str) -> Result<(), String> {
        validate_id(id)?;
        let store = store_path(root, true)?;
        let _lock = lock_store(&store)?;
        let mut registry = load_registry(&store)?;
        let index = registry
            .installed
            .iter()
            .position(|item| item.manifest.id == id)
            .ok_or_else(|| "El plugin no está instalado.".to_owned())?;
        let parent = store.join("installed");
        check_managed_directory_if_present(&parent)?;
        let destination = parent.join(id);
        let trash = parent.join(format!(".removed-{}", uuid::Uuid::new_v4()));
        let present = exists_no_follow(&destination)?;
        if present {
            check_managed_directory_if_present(&destination)?;
            fs::rename(&destination, &trash).map_err(io_message)?;
        }
        registry.installed.remove(index);
        if let Err(error) = save_registry(&store, &registry) {
            if present {
                if let Err(rollback) = fs::rename(&trash, &destination) {
                    return Err(format!(
                        "{error}. No se pudo restaurar la copia: {rollback}"
                    ));
                }
            }
            return Err(error);
        }
        // A locked file on Windows may postpone physical cleanup; the package
        // is already unregistered and can no longer be enabled or launched.
        if present {
            let _ = fs::remove_dir_all(&trash);
        }
        Ok(())
    }

    /// Produces an immutable turn snapshot, without starting any process or
    /// reading the values of credential environment variables.
    pub fn runtime_servers(
        root: &Path,
        workspace: &Path,
    ) -> Result<Vec<McpServerSettings>, String> {
        let workspace_key = workspace_key(workspace)?;
        let workspace = Path::new(&workspace_key);
        let store = store_path(root, false)?;
        let registry = load_registry(&store)?;
        let servers = runtime_servers_from_registry(&store, &registry, workspace)?;
        McpSettings {
            servers: servers.clone(),
        }
        .validate(workspace)?;
        Ok(servers)
    }
}

fn runtime_servers_from_registry(
    store: &Path,
    registry: &Registry,
    workspace: &Path,
) -> Result<Vec<McpServerSettings>, String> {
    let workspace_key = path_string(workspace)?;
    let mut servers = Vec::new();
    for item in registry
        .installed
        .iter()
        .filter(|item| item.enabled_workspaces.contains(&workspace_key))
    {
        let directory = verify_installation(store, item)?;
        servers.push(runtime_server(&item.manifest, &directory, workspace)?);
    }
    Ok(servers)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 24
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    {
        return Err("El ID del plugin debe empezar con a-z y usar hasta 24 letras minúsculas, números, - o _.".into());
    }
    Ok(())
}

fn text_field(value: &str, limit: usize, field: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > limit
        || value.chars().any(char::is_control)
    {
        return Err(format!("Campo {field} del plugin vacío o no válido."));
    }
    Ok(())
}

fn relative_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty()
        || value.len() > 1024
        || value.contains(['\\', ':', '$'])
        || value.chars().any(char::is_control)
    {
        return Err("Ruta relativa del plugin no válida.".into());
    }
    let value = value.strip_prefix("./").unwrap_or(value);
    // Check the literal segments too: Path::components normalizes `.` away.
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Las rutas del plugin no admiten segmentos vacíos, . ni ...".into());
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Las rutas del plugin deben permanecer dentro de su carpeta.".into());
    }
    Ok(path.to_path_buf())
}

fn is_bare_program(program: &str) -> bool {
    !program.contains(['/', '\\', ':'])
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err(
            "Versión de formato de plugin no compatible; se requiere schema_version 1.".into(),
        );
    }
    validate_id(&manifest.id)?;
    for (value, limit, field) in [
        (&manifest.name, 100, "name"),
        (&manifest.version, 64, "version"),
        (&manifest.description, 1000, "description"),
        (&manifest.publisher, 100, "publisher"),
        (&manifest.program, 1024, "program"),
    ] {
        text_field(value, limit, field)?;
    }
    if is_bare_program(&manifest.program) {
        if manifest.program.starts_with('-')
            || !manifest
                .program
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err("program debe ser un nombre ejecutable, no una orden de shell.".into());
        }
        if manifest.entrypoint.is_none() {
            return Err(
                "Un plugin con intérprete debe incluir un entrypoint dentro del paquete.".into(),
            );
        }
    } else {
        relative_path(&manifest.program)?;
    }
    if let Some(entrypoint) = &manifest.entrypoint {
        relative_path(entrypoint)?;
    }
    if manifest.args.len() + usize::from(manifest.entrypoint.is_some()) > 32 {
        return Err("El plugin admite como máximo 32 argumentos, incluida su entrada.".into());
    }
    for argument in &manifest.args {
        expand_argument(argument, Path::new("/plugin"), Path::new("/workspace"))?;
    }
    // Reuse the same argument and credential-name contract as the actual
    // execution route, rather than allowing settings that fail only at launch.
    McpSettings {
        servers: vec![McpServerSettings {
            id: format!("plugin_{}", manifest.id),
            enabled: true,
            program: "plugin-runtime".into(),
            args: manifest.args.clone(),
            cwd: None,
            credential_env_names: manifest.credential_env_names.clone(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
        }],
    }
    .validate_structure()
}

fn expand_argument(argument: &str, plugin: &Path, workspace: &Path) -> Result<String, String> {
    for (token, base) in [("${plugin}", plugin), ("${workspace}", workspace)] {
        if argument == token {
            return path_string(base);
        }
        if let Some(suffix) = argument.strip_prefix(&format!("{token}/")) {
            return path_string(&base.join(relative_path(suffix)?));
        }
    }
    if argument.contains("${") {
        return Err("Los argumentos sólo admiten ${plugin} o ${workspace} completos, opcionalmente seguidos de una ruta relativa.".into());
    }
    Ok(argument.into())
}

fn runtime_server(
    manifest: &PluginManifest,
    plugin: &Path,
    workspace: &Path,
) -> Result<McpServerSettings, String> {
    let program = if is_bare_program(&manifest.program) {
        manifest.program.clone()
    } else {
        path_string(&plugin.join(relative_path(&manifest.program)?))?
    };
    let mut args = Vec::new();
    if let Some(entrypoint) = &manifest.entrypoint {
        args.push(path_string(&plugin.join(relative_path(entrypoint)?))?);
    }
    for argument in &manifest.args {
        args.push(expand_argument(argument, plugin, workspace)?);
    }
    Ok(McpServerSettings {
        id: format!("plugin_{}", manifest.id),
        enabled: true,
        program,
        args,
        cwd: None,
        credential_env_names: manifest.credential_env_names.clone(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
    })
}

fn verify_installation(store: &Path, item: &Installation) -> Result<PathBuf, String> {
    check_managed_directory_if_present(&store.join("installed"))?;
    let directory = store.join("installed").join(&item.manifest.id);
    check_managed_directory_if_present(&directory)?;
    let bundle = read_bundle(&directory)
        .map_err(|error| format!("Plugin {} no disponible: {error}", item.manifest.name))?;
    if bundle.preview.digest != item.digest || bundle.preview.manifest != item.manifest {
        return Err(format!(
            "El contenido de {} cambió después de instalarlo. Desactívalo y vuelve a instalar la versión revisada.",
            item.manifest.name
        ));
    }
    Ok(bundle.preview.source)
}

fn read_bundle(source: &Path) -> Result<Bundle, String> {
    check_directory_if_present(source)?;
    let source = source.canonicalize().map_err(io_message)?;
    if !source.is_dir() {
        return Err("El origen del plugin debe ser una carpeta.".into());
    }
    let manifest_bytes = read_regular(&source.join(MANIFEST), MAX_MANIFEST_BYTES)?;
    let manifest: PluginManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Manifiesto {MANIFEST} no válido: {error}"))?;
    validate_manifest(&manifest)?;
    let mut bundle = Bundle {
        preview: PluginPreview {
            manifest,
            digest: String::new(),
            source: source.clone(),
            file_count: 0,
            total_bytes: 0,
        },
        directories: Vec::new(),
        files: Vec::new(),
    };
    collect_bundle(&source, Path::new(""), 0, &mut bundle)?;
    let observed_manifest = bundle
        .files
        .iter()
        .find(|file| file.relative == Path::new(MANIFEST))
        .ok_or_else(|| "El manifiesto desapareció durante la revisión.".to_owned())?;
    if observed_manifest.bytes != manifest_bytes {
        return Err("El manifiesto cambió durante la revisión.".into());
    }
    let entry = bundle
        .preview
        .manifest
        .entrypoint
        .as_ref()
        .map(|value| relative_path(value))
        .transpose()?;
    if let Some(entry) = entry {
        if !bundle.files.iter().any(|file| file.relative == entry) {
            return Err("El entrypoint no existe como archivo dentro del paquete.".into());
        }
    }
    if !is_bare_program(&bundle.preview.manifest.program) {
        let program = relative_path(&bundle.preview.manifest.program)?;
        let file = bundle
            .files
            .iter()
            .find(|file| file.relative == program)
            .ok_or_else(|| "El ejecutable no existe dentro del paquete.".to_owned())?;
        #[cfg(unix)]
        if !file.executable {
            return Err("El programa empaquetado no tiene permiso de ejecución.".into());
        }
        #[cfg(not(unix))]
        let _ = file;
    }
    bundle.files.sort_by(|a, b| a.relative.cmp(&b.relative));
    bundle.directories.sort();
    let mut digest = Sha256::new();
    digest.update(b"openide-plugin-bundle-v1\0");
    for directory in &bundle.directories {
        digest.update(b"directory\0");
        hash_field(&mut digest, portable_relative(directory)?.as_bytes());
    }
    for file in &bundle.files {
        digest.update(b"file\0");
        hash_field(&mut digest, portable_relative(&file.relative)?.as_bytes());
        digest.update([u8::from(file.executable)]);
        hash_field(&mut digest, &file.bytes);
    }
    bundle.preview.digest = format!("{:x}", digest.finalize());
    bundle.preview.file_count = bundle.files.len();
    Ok(bundle)
}

fn collect_bundle(
    root: &Path,
    relative: &Path,
    depth: usize,
    bundle: &mut Bundle,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("El paquete tiene demasiados niveles de carpetas.".into());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(root.join(relative)).map_err(io_message)? {
        entries.push(entry.map_err(io_message)?);
        if entries.len() > MAX_FILES + MAX_DIRECTORIES {
            return Err("El paquete contiene demasiadas entradas.".into());
        }
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = relative.join(entry.file_name());
        portable_relative(&child)?;
        let path = root.join(&child);
        let metadata = fs::symlink_metadata(&path).map_err(io_message)?;
        if metadata.file_type().is_symlink()
            || !path.canonicalize().map_err(io_message)?.starts_with(root)
        {
            return Err(
                "El paquete no admite enlaces simbólicos ni rutas fuera de su carpeta.".into(),
            );
        }
        if metadata.is_dir() {
            bundle.directories.push(child.clone());
            if bundle.directories.len() > MAX_DIRECTORIES {
                return Err("El paquete contiene demasiadas carpetas.".into());
            }
            collect_bundle(root, &child, depth + 1, bundle)?;
        } else if metadata.is_file() {
            if bundle.files.len() >= MAX_FILES {
                return Err("El paquete contiene demasiados archivos.".into());
            }
            let bytes = read_regular(&path, MAX_FILE_BYTES)?;
            bundle.preview.total_bytes += bytes.len() as u64;
            if bundle.preview.total_bytes > MAX_BUNDLE_BYTES {
                return Err("El paquete excede el límite de 64 MiB.".into());
            }
            bundle.files.push(BundleFile {
                relative: child,
                bytes,
                executable: is_executable(&metadata),
            });
        } else {
            return Err("El paquete sólo puede contener archivos regulares y carpetas.".into());
        }
    }
    Ok(())
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn portable_relative(path: &Path) -> Result<String, String> {
    let value = path_string(path)?.replace(std::path::MAIN_SEPARATOR, "/");
    relative_path(&value)?;
    Ok(value)
}

fn workspace_key(workspace: &Path) -> Result<String, String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("Proyecto no disponible: {error}"))?;
    if !workspace.is_dir() {
        return Err("El proyecto debe ser una carpeta.".into());
    }
    path_string(&workspace)
}

fn path_string(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "La ruta del plugin debe ser UTF-8.".to_owned())?;
    if value.chars().any(char::is_control) {
        return Err("La ruta del plugin contiene caracteres de control.".into());
    }
    Ok(value.into())
}

fn store_path(root: &Path, create: bool) -> Result<PathBuf, String> {
    if create {
        fs::create_dir_all(root).map_err(io_message)?;
    }
    let root = if root.exists() {
        root.canonicalize().map_err(io_message)?
    } else {
        root.to_path_buf()
    };
    check_managed_directory_if_present(&root)?;
    let store = root.join("plugins");
    if create {
        ensure_private_dir(&store)?;
    } else {
        check_managed_directory_if_present(&store)?;
    }
    Ok(store)
}

fn check_directory_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("La carpeta de plugins contiene un enlace o un archivo inesperado.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_message(error)),
    }
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    if exists_no_follow(path)? {
        return check_managed_directory_if_present(path);
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(io_message)
}

/// The source package may be shared, but persisted executable configuration
/// and its containing directories must not be writable by another account.
fn check_managed_access(metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        validate_managed_unix_access(metadata.uid(), metadata.permissions().mode())?;
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

#[cfg(unix)]
fn validate_managed_unix_access(owner: u32, mode: u32) -> Result<(), String> {
    if owner != unsafe { libc::geteuid() } || mode & 0o022 != 0 {
        return Err("El almacenamiento de plugins debe pertenecer al usuario actual y no permitir escritura de grupo u otros usuarios.".into());
    }
    Ok(())
}

fn check_managed_directory_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            check_managed_access(&metadata)
        }
        Ok(_) => Err("La carpeta de plugins contiene un enlace o un archivo inesperado.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_message(error)),
    }
}

fn read_managed_regular(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    check_managed_access(&fs::symlink_metadata(path).map_err(io_message)?)?;
    read_regular(path, limit)
}

fn exists_no_follow(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_message(error)),
    }
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(io_message)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err("El archivo del plugin no es regular o supera el tamaño permitido.".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io_message)?;
    if !file.metadata().map_err(io_message)?.is_file() {
        return Err("El archivo del plugin cambió durante la lectura.".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_message)?;
    if bytes.len() as u64 > limit {
        return Err("El archivo del plugin supera el tamaño permitido.".into());
    }
    Ok(bytes)
}

fn private_new_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(io_message)
}

fn lock_store(store: &Path) -> Result<File, String> {
    let path = store.join("registry.lock");
    if exists_no_follow(&path)? {
        let metadata = fs::symlink_metadata(&path).map_err(io_message)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("El bloqueo de plugins no es un archivo regular.".into());
        }
        check_managed_access(&metadata)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io_message)?;
    check_managed_access(&file.metadata().map_err(io_message)?)?;
    fs2::FileExt::lock_exclusive(&file).map_err(io_message)?;
    Ok(file)
}

fn load_registry(store: &Path) -> Result<Registry, String> {
    let path = store.join("registry.json");
    if !exists_no_follow(&path)? {
        return Ok(Registry::default());
    }
    let registry: Registry =
        serde_json::from_slice(&read_managed_regular(&path, MAX_REGISTRY_BYTES)?)
            .map_err(|error| format!("Registro de plugins no válido: {error}"))?;
    validate_registry(&registry)?;
    Ok(registry)
}

fn validate_registry(registry: &Registry) -> Result<(), String> {
    if registry.schema_version != 1 || registry.installed.len() > MAX_PLUGINS {
        return Err("Versión o tamaño de registro de plugins no válido.".into());
    }
    let mut ids = BTreeSet::new();
    for item in &registry.installed {
        validate_manifest(&item.manifest)?;
        if !ids.insert(&item.manifest.id)
            || item.digest.len() != 64
            || !item
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || item.enabled_workspaces.len() > MAX_WORKSPACES
        {
            return Err("Identidad de plugin guardada no válida.".into());
        }
        for workspace in &item.enabled_workspaces {
            if !Path::new(workspace).is_absolute()
                || workspace.len() > 4096
                || workspace.chars().any(char::is_control)
            {
                return Err("Proyecto de activación guardado no válido.".into());
            }
        }
    }
    Ok(())
}

fn save_registry(store: &Path, registry: &Registry) -> Result<(), String> {
    validate_registry(registry)?;
    let bytes = serde_json::to_vec_pretty(registry).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err("El registro de plugins excede el límite de tamaño.".into());
    }
    let destination = store.join("registry.json");
    if exists_no_follow(&destination)? {
        read_managed_regular(&destination, MAX_REGISTRY_BYTES)?;
    }
    let temporary = store.join(format!(".registry-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = private_new_file(&temporary)?;
        file.write_all(&bytes).map_err(io_message)?;
        file.sync_all().map_err(io_message)?;
        drop(file);
        replace_atomically(&temporary, &destination).map_err(io_message)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_executable(file: &File, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(if executable {
        0o700
    } else {
        0o600
    }))
    .map_err(io_message)
}

#[cfg(not(unix))]
fn set_executable(_: &File, _: bool) -> Result<(), String> {
    Ok(())
}

fn io_message(error: io::Error) -> String {
    format!("No se pudo acceder al paquete de plugin: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PluginManifest {
        PluginManifest {
            schema_version: 1,
            id: "fixture".into(),
            name: "Fixture tools".into(),
            version: "1.0.0".into(),
            description: "Local fixture tools".into(),
            publisher: "OpenIDE tests".into(),
            program: "python3".into(),
            entrypoint: Some("server.py".into()),
            args: vec!["${workspace}".into()],
            credential_env_names: vec!["PLUGIN_TEST_TOKEN".into()],
        }
    }

    fn source(path: &Path, manifest: &PluginManifest) {
        fs::create_dir_all(path).unwrap();
        fs::write(
            path.join(MANIFEST),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            path.join("server.py"),
            b"raise RuntimeError('installation must not execute code')\n",
        )
        .unwrap();
    }

    #[test]
    fn install_reopen_activate_per_workspace_and_remove_only_private_copy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let workspace = temp.path().join("project");
        let other = temp.path().join("other-project");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&other).unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        let preview = PluginStore::inspect(&original).unwrap();
        assert_eq!(preview.file_count, 2);
        PluginStore::install(&root, &workspace, &preview).unwrap();
        let installed = PluginStore::list(&root, &workspace).unwrap();
        assert!(!installed[0].enabled);
        assert_ne!(installed[0].path, original);
        assert!(PluginStore::runtime_servers(&root, &workspace)
            .unwrap()
            .is_empty());
        PluginStore::set_enabled(&root, &workspace, "fixture", true).unwrap();
        let runtime = PluginStore::runtime_servers(&root, &workspace).unwrap();
        assert_eq!(runtime[0].id, "plugin_fixture");
        assert_eq!(
            runtime[0].args[0],
            installed[0].path.join("server.py").to_str().unwrap()
        );
        assert_eq!(runtime[0].args[1], workspace.to_str().unwrap());
        assert_eq!(runtime[0].credential_env_names, ["PLUGIN_TEST_TOKEN"]);
        assert!(PluginStore::runtime_servers(&root, &other)
            .unwrap()
            .is_empty());
        assert!(!PluginStore::list(&root, &other).unwrap()[0].enabled);
        PluginStore::set_enabled(&root, &workspace, "fixture", false).unwrap();
        assert!(PluginStore::runtime_servers(&root, &workspace)
            .unwrap()
            .is_empty());
        PluginStore::uninstall(&root, "fixture").unwrap();
        assert!(PluginStore::list(&root, &workspace).unwrap().is_empty());
        assert!(!installed[0].path.exists());
        assert!(original.join("server.py").is_file());
    }

    #[test]
    fn stale_review_is_rejected_and_duplicate_install_preserves_existing_activation() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        let preview = PluginStore::inspect(&original).unwrap();
        fs::write(original.join("server.py"), b"changed").unwrap();
        let root = temp.path().join("data");
        assert!(PluginStore::install(&root, temp.path(), &preview)
            .unwrap_err()
            .contains("cambió"));
        assert!(PluginStore::list(&root, temp.path()).unwrap().is_empty());
        let preview = PluginStore::inspect(&original).unwrap();
        PluginStore::install(&root, temp.path(), &preview).unwrap();
        PluginStore::set_enabled(&root, temp.path(), "fixture", true).unwrap();
        assert!(PluginStore::install(&root, temp.path(), &preview).is_err());
        assert!(PluginStore::list(&root, temp.path()).unwrap()[0].enabled);
        assert_eq!(
            PluginStore::runtime_servers(&root, temp.path())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn changed_dependency_blocks_activation_and_runtime_but_allows_disabling_and_removal() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        fs::create_dir(original.join("lib")).unwrap();
        fs::write(original.join("lib/helper.py"), b"value = 1\n").unwrap();
        let root = temp.path().join("data");
        PluginStore::install(
            &root,
            temp.path(),
            &PluginStore::inspect(&original).unwrap(),
        )
        .unwrap();
        PluginStore::set_enabled(&root, temp.path(), "fixture", true).unwrap();
        let plugin = PluginStore::list(&root, temp.path()).unwrap().remove(0);
        fs::write(plugin.path.join("lib/helper.py"), b"value = 2\n").unwrap();
        assert!(PluginStore::runtime_servers(&root, temp.path())
            .unwrap_err()
            .contains("cambió"));
        assert!(PluginStore::set_enabled(&root, temp.path(), "fixture", true).is_err());
        PluginStore::set_enabled(&root, temp.path(), "fixture", false).unwrap();
        assert!(PluginStore::runtime_servers(&root, temp.path())
            .unwrap()
            .is_empty());
        PluginStore::uninstall(&root, "fixture").unwrap();
    }

    #[test]
    fn traversal_unknown_fields_missing_entrypoint_and_shell_strings_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        for bad in [
            "../server.py",
            "lib/../../server.py",
            "/tmp/server.py",
            "C:\\server.py",
            "lib//server.py",
        ] {
            let mut candidate = manifest();
            candidate.entrypoint = Some(bad.into());
            source(temp.path(), &candidate);
            assert!(PluginStore::inspect(temp.path()).is_err(), "{bad}");
        }
        let mut candidate = manifest();
        candidate.program = "python3 -c malicious".into();
        source(temp.path(), &candidate);
        assert!(PluginStore::inspect(temp.path()).is_err());
        candidate = manifest();
        candidate.entrypoint = None;
        source(temp.path(), &candidate);
        assert!(PluginStore::inspect(temp.path()).is_err());
        candidate = manifest();
        candidate.entrypoint = Some("missing.py".into());
        source(temp.path(), &candidate);
        assert!(PluginStore::inspect(temp.path()).is_err());
        let mut value = serde_json::to_value(manifest()).unwrap();
        value["postinstall"] = "run me".into();
        fs::write(
            temp.path().join(MANIFEST),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(PluginStore::inspect(temp.path()).is_err());
    }

    #[test]
    fn placeholders_are_literal_path_substitutions_not_shell_or_traversal() {
        let plugin = Path::new("/managed/plugins");
        let workspace = Path::new("/a project");
        assert_eq!(
            expand_argument("${workspace}", plugin, workspace).unwrap(),
            "/a project"
        );
        assert_eq!(
            expand_argument("${plugin}/lib/data.json", plugin, workspace).unwrap(),
            "/managed/plugins/lib/data.json"
        );
        assert_eq!(
            expand_argument("$(touch never)", plugin, workspace).unwrap(),
            "$(touch never)"
        );
        for argument in [
            "${HOME}",
            "prefix${workspace}",
            "${plugin}/../outside",
            "${workspace}/${secret}",
        ] {
            assert!(expand_argument(argument, plugin, workspace).is_err());
        }
    }

    #[test]
    fn corrupt_registry_and_oversized_files_do_not_get_overwritten_or_installed() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        let preview = PluginStore::inspect(&original).unwrap();
        let root = temp.path().join("data");
        fs::create_dir_all(root.join("plugins")).unwrap();
        fs::write(root.join("plugins/registry.json"), b"invalid registry").unwrap();
        assert!(PluginStore::install(&root, temp.path(), &preview).is_err());
        assert_eq!(
            fs::read(root.join("plugins/registry.json")).unwrap(),
            b"invalid registry"
        );
        File::create(original.join("huge.bin"))
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(PluginStore::inspect(&original).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn package_and_managed_path_symlinks_cannot_escape_or_delete_the_target() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("preserve"), b"important").unwrap();
        symlink(&outside, original.join("linked")).unwrap();
        assert!(PluginStore::inspect(&original).is_err());
        fs::remove_file(original.join("linked")).unwrap();
        let root = temp.path().join("data");
        PluginStore::install(
            &root,
            temp.path(),
            &PluginStore::inspect(&original).unwrap(),
        )
        .unwrap();
        let destination = PluginStore::list(&root, temp.path())
            .unwrap()
            .remove(0)
            .path;
        fs::remove_dir_all(&destination).unwrap();
        symlink(&outside, &destination).unwrap();
        assert!(PluginStore::set_enabled(&root, temp.path(), "fixture", true).is_err());
        assert!(PluginStore::uninstall(&root, "fixture").is_err());
        assert_eq!(fs::read(outside.join("preserve")).unwrap(), b"important");
        assert_eq!(PluginStore::list(&root, temp.path()).unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn managed_storage_rejects_shared_write_permissions_without_restricting_source_folders() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        source(&original, &manifest());
        fs::set_permissions(&original, fs::Permissions::from_mode(0o777)).unwrap();
        let preview = PluginStore::inspect(&original).unwrap();
        let root = temp.path().join("data");
        PluginStore::install(&root, temp.path(), &preview).unwrap();
        let store = root.join("plugins");
        let registry = store.join("registry.json");
        let saved = fs::read(&registry).unwrap();

        for path in [
            &root,
            &store,
            &store.join("installed"),
            &store.join("installed/fixture"),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o770)).unwrap();
            assert!(
                PluginStore::set_enabled(&root, temp.path(), "fixture", true).is_err(),
                "accepted writable managed directory {}",
                path.display()
            );
            assert!(PluginStore::uninstall(&root, "fixture").is_err());
            assert_eq!(fs::read(&registry).unwrap(), saved);
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        fs::set_permissions(&registry, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(PluginStore::list(&root, temp.path()).is_err());
        assert!(PluginStore::runtime_servers(&root, temp.path()).is_err());
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o600)).unwrap();
        let lock = store.join("registry.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o662)).unwrap();
        assert!(PluginStore::set_enabled(&root, temp.path(), "fixture", true).is_err());
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        PluginStore::set_enabled(&root, temp.path(), "fixture", true).unwrap();
        assert_eq!(
            PluginStore::runtime_servers(&root, temp.path())
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_storage_requires_current_owner_even_when_mode_is_private() {
        let owner = unsafe { libc::geteuid() };
        assert!(validate_managed_unix_access(owner.wrapping_add(1), 0o700).is_err());
        assert!(validate_managed_unix_access(owner, 0o702).is_err());
        assert!(validate_managed_unix_access(owner, 0o720).is_err());
        assert!(validate_managed_unix_access(owner, 0o700).is_ok());
        assert!(validate_managed_unix_access(owner, 0o755).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn bundled_executable_preserves_execution_bit_and_activation_uses_canonical_workspace() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("source");
        let mut candidate = manifest();
        candidate.program = "./server.py".into();
        candidate.entrypoint = None;
        source(&original, &candidate);
        fs::set_permissions(
            original.join("server.py"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let root = temp.path().join("data");
        let alias = temp.path().join("alias");
        symlink(temp.path(), &alias).unwrap();
        PluginStore::install(&root, &alias, &PluginStore::inspect(&original).unwrap()).unwrap();
        PluginStore::set_enabled(&root, &alias, "fixture", true).unwrap();
        let runtime = PluginStore::runtime_servers(&root, temp.path()).unwrap();
        assert!(Path::new(&runtime[0].program).is_absolute());
        assert_eq!(
            fs::metadata(&runtime[0].program)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::set_permissions(&runtime[0].program, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(PluginStore::runtime_servers(&root, temp.path()).is_err());
    }
}
