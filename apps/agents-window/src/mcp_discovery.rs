//! Local MCP installation hints. Reading this module never starts a process,
//! contacts a registry, changes a registration or proves protocol support.
use crate::mcp_settings::{McpServerSettings, McpSettings, McpSettingsScope};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_READ_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIRECTORIES: usize = 512;
const MAX_DIRECTORY_ENTRIES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum McpLauncher {
    Direct,
    Npx { package: Option<String> },
    Uvx { package: Option<String> },
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalPackageMetadata {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    /// Evidence for these hints, not a file to execute.
    pub manifest: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpLocalDiscovery {
    pub executable: Option<PathBuf>,
    pub launcher: McpLauncher,
    pub package: Option<LocalPackageMetadata>,
    /// Fixed messages only: no command arguments or environment values.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct McpIdentityHints {
    pub server_id: String,
    pub package_name: Option<String>,
    pub executable_name: Option<String>,
    pub homepage: Option<String>,
    pub url_host: Option<String>,
}

impl McpLocalDiscovery {
    pub fn identity_hints(&self, server: &McpServerSettings) -> McpIdentityHints {
        McpIdentityHints {
            server_id: server.id.clone(),
            package_name: self
                .package
                .as_ref()
                .map(|package| package.name.clone())
                .or_else(|| match &self.launcher {
                    McpLauncher::Npx { package } => package
                        .as_deref()
                        .and_then(npm_request)
                        .map(|request| request.name),
                    McpLauncher::Uvx { package } => package
                        .as_deref()
                        .and_then(python_request)
                        .map(|request| request.name),
                    _ => None,
                }),
            executable_name: self
                .executable
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_owned),
            homepage: self
                .package
                .as_ref()
                .and_then(|package| package.homepage.clone()),
            url_host: server
                .url
                .as_ref()
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_owned)),
        }
    }
}

/// Explicit paths make detection deterministic in tests. They are metadata
/// locations, never directories whose binaries should be tried as MCP servers.
#[derive(Clone, Debug, Default)]
pub(crate) struct DiscoveryEnvironment {
    pub search_path: OsString,
    pub npm_cache: Option<PathBuf>,
    pub npm_roots: Vec<PathBuf>,
    pub uv_roots: Vec<PathBuf>,
}

impl DiscoveryEnvironment {
    pub fn from_system() -> Self {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        let absolute_env = |name| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let cache = absolute_env("XDG_CACHE_HOME")
            .or_else(|| home.as_ref().map(|home| home.join(".cache")));
        let data = absolute_env("XDG_DATA_HOME")
            .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
        let mut npm_roots = Vec::new();
        if let Some(prefix) = absolute_env("npm_config_prefix") {
            npm_roots.push(prefix.join("lib/node_modules"));
            npm_roots.push(prefix.join("node_modules"));
        }
        if let Some(home) = &home {
            npm_roots.push(home.join(".local/lib/node_modules"));
        }
        if let Some(appdata) = absolute_env("APPDATA") {
            npm_roots.push(appdata.join("npm/node_modules"));
        }
        Self {
            search_path: std::env::var_os("PATH").unwrap_or_default(),
            npm_cache: absolute_env("npm_config_cache")
                .or_else(|| home.map(|home| home.join(".npm"))),
            npm_roots,
            uv_roots: [
                absolute_env("UV_TOOL_DIR").or_else(|| data.map(|data| data.join("uv/tools"))),
                absolute_env("UV_CACHE_DIR").or_else(|| cache.map(|cache| cache.join("uv"))),
            ]
            .into_iter()
            .flatten()
            .collect(),
        }
    }
}

/// Inspect registrations, including disabled ones. Finding a launcher or a
/// cached manifest is deliberately distinct from a successful MCP connection.
pub(crate) fn inspect_batch(
    servers: &[McpServerSettings],
    workspace: &Path,
    environment: &DiscoveryEnvironment,
) -> BTreeMap<String, McpLocalDiscovery> {
    servers
        .iter()
        .map(|server| (server.id.clone(), inspect(server, workspace, environment)))
        .collect()
}

pub(crate) fn inspect(
    server: &McpServerSettings,
    workspace: &Path,
    environment: &DiscoveryEnvironment,
) -> McpLocalDiscovery {
    let mut found = McpLocalDiscovery {
        executable: None,
        launcher: if server.url.is_some() {
            McpLauncher::Remote
        } else {
            McpLauncher::Direct
        },
        package: None,
        warnings: Vec::new(),
    };
    if (McpSettings {
        servers: vec![server.clone()],
    })
    .validate(workspace)
    .is_err()
    {
        found
            .warnings
            .push("La configuración o el directorio MCP no son válidos para este proyecto.".into());
        return found;
    }
    if server.url.is_some() {
        return found;
    }
    let Ok(workspace) = workspace.canonicalize() else {
        return found;
    };
    let cwd = server
        .cwd
        .as_ref()
        .map_or_else(|| workspace.clone(), |cwd| workspace.join(cwd));
    let search_path = server
        .env
        .get("PATH")
        .map(OsString::from)
        .unwrap_or_else(|| environment.search_path.clone());
    // Resolve relative PATH entries as the child would, after setting cwd.
    let paths = std::env::split_paths(&search_path).take(128).map(|path| {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    });
    let search_path = std::env::join_paths(paths).unwrap_or_default();
    found.executable = openide_cli::detect_executable_in(&server.program, Some(&search_path))
        .and_then(|path| path.canonicalize().ok());
    if found.executable.is_none() {
        found
            .warnings
            .push("El programa configurado no se encontró en PATH ni en su ruta absoluta.".into());
    }
    let name = Path::new(&server.program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let mut budget = ReadBudget::default();
    match name {
        "npx" => {
            let request =
                package_argument(&server.args, false).filter(|spec| npm_request(spec).is_some());
            found.launcher = McpLauncher::Npx {
                package: request.clone(),
            };
            found.package = request
                .as_deref()
                .and_then(npm_request)
                .and_then(|request| {
                    npm_metadata(
                        &request,
                        &cwd,
                        &workspace,
                        environment,
                        found.executable.as_deref(),
                        &mut budget,
                    )
                });
            found.warnings.push("La presencia de npx no verifica el servidor. Al conectar, npx puede descargar o seleccionar otra versión del paquete.".into());
        }
        "uvx" => {
            let request =
                package_argument(&server.args, true).filter(|spec| python_request(spec).is_some());
            found.launcher = McpLauncher::Uvx {
                package: request.clone(),
            };
            found.package = request
                .as_deref()
                .and_then(python_request)
                .and_then(|request| python_metadata(&request, &environment.uv_roots, &mut budget));
            found.warnings.push("La presencia de uvx no verifica el servidor. Al conectar, uvx puede descargar o seleccionar otra versión del paquete.".into());
        }
        _ => {
            // npm's executable symlink normally resolves into its package.
            if let Some(executable) = &found.executable {
                found.package = executable
                    .ancestors()
                    .skip(1)
                    .take(6)
                    .find_map(|directory| {
                        npm_manifest(
                            &directory.join("package.json"),
                            None,
                            Some(executable),
                            &mut budget,
                        )
                    });
            }
        }
    }
    if matches!(
        found.launcher,
        McpLauncher::Npx { .. } | McpLauncher::Uvx { .. }
    ) && found.package.is_none()
    {
        found.warnings.push("No se localizó metadata compatible del paquete en las ubicaciones locales revisadas; esto no demuestra que no esté instalado.".into());
    }
    if budget.truncated {
        found.warnings.push("La búsqueda local alcanzó su límite de archivos o tamaño; el resultado puede ser incompleto.".into());
    }
    found
}

#[derive(Default)]
struct ReadBudget {
    bytes: usize,
    directories: usize,
    truncated: bool,
}

impl ReadBudget {
    fn read(&mut self, path: &Path) -> Option<String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        if metadata.len() > MAX_MANIFEST_BYTES || self.bytes >= MAX_READ_BYTES {
            self.truncated = true;
            return None;
        }
        let limit = MAX_MANIFEST_BYTES.min((MAX_READ_BYTES - self.bytes) as u64);
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes).ok()?;
        self.bytes = self.bytes.saturating_add(bytes.len());
        if bytes.len() as u64 > limit {
            self.truncated = true;
            return None;
        }
        String::from_utf8(bytes).ok()
    }

    fn directories(&mut self, path: &Path) -> Vec<PathBuf> {
        if self.directories >= MAX_DIRECTORIES {
            self.truncated = true;
            return Vec::new();
        }
        self.directories += 1;
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let entries: Vec<_> = entries.take(MAX_DIRECTORY_ENTRIES + 1).collect();
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            self.truncated = true;
        }
        let mut directories: Vec<_> = entries
            .into_iter()
            .take(MAX_DIRECTORY_ENTRIES)
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        directories.sort();
        let remaining = MAX_DIRECTORIES.saturating_sub(self.directories);
        if directories.len() > remaining {
            self.truncated = true;
            directories.truncate(remaining);
        }
        self.directories += directories.len();
        directories
    }
}

struct PackageRequest {
    name: String,
    version: Option<String>,
}

/// Recognize launch syntax, never shell syntax. Unknown option layouts leave
/// the package unresolved rather than guessing that an option value is one.
fn package_argument(args: &[String], python: bool) -> Option<String> {
    let mut index = 0;
    let mut explicit = None;
    while let Some(argument) = args.get(index) {
        index += 1;
        if argument == "--" {
            return explicit.or_else(|| args.get(index).cloned());
        }
        let package_option = if python { "--from" } else { "--package" };
        if let Some(package) = argument.strip_prefix(&format!("{package_option}=")) {
            if explicit.is_some() {
                return None;
            }
            explicit = Some(package.to_owned());
        } else if argument == package_option || (!python && argument == "-p") {
            if explicit.is_some() {
                return None;
            }
            explicit = Some(args.get(index)?.clone());
            index += 1;
        } else if matches!(
            argument.as_str(),
            "-y" | "--yes"
                | "-n"
                | "--no"
                | "--offline"
                | "--prefer-offline"
                | "--ignore-existing"
                | "-q"
                | "--quiet"
                | "-v"
                | "--verbose"
                | "--isolated"
                | "--no-cache"
                | "--no-config"
        ) {
            continue;
        } else if matches!(
            argument.as_str(),
            "--cache"
                | "--registry"
                | "--userconfig"
                | "--prefix"
                | "--python"
                | "--directory"
                | "--cache-dir"
                | "--index-url"
                | "--with"
        ) {
            args.get(index)?;
            index += 1;
        } else if argument.starts_with('-') {
            return None;
        } else {
            return explicit.or_else(|| Some(argument.clone()));
        }
    }
    explicit
}

fn npm_request(spec: &str) -> Option<PackageRequest> {
    let split = spec.rfind('@').filter(|index| *index > 0);
    let (name, version) = split.map_or((spec, None), |index| {
        (&spec[..index], Some(&spec[index + 1..]))
    });
    let segments: Vec<_> = name.strip_prefix('@').unwrap_or(name).split('/').collect();
    if spec.len() > 256
        || segments.len() != if name.starts_with('@') { 2 } else { 1 }
        || segments.iter().any(|segment| {
            segment.is_empty()
                || *segment == "."
                || *segment == ".."
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
                })
        })
    {
        return None;
    }
    if version.is_some_and(|version| {
        version.is_empty()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+^~*<>=|".contains(&byte))
    }) {
        return None;
    }
    let version = version
        .filter(|version| {
            version.as_bytes()[0].is_ascii_digit()
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
        })
        .map(str::to_owned);
    Some(PackageRequest {
        name: name.into(),
        version,
    })
}

fn python_name(name: &str) -> Option<String> {
    if name.is_empty()
        || name.len() > 128
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return None;
    }
    let mut result = String::new();
    for byte in name.bytes() {
        if b"-_.".contains(&byte) {
            if !result.ends_with('-') {
                result.push('-');
            }
        } else {
            result.push(char::from(byte.to_ascii_lowercase()));
        }
    }
    Some(result)
}

fn python_request(spec: &str) -> Option<PackageRequest> {
    let (name, version) = spec
        .split_once("==")
        .map_or((spec, None), |(name, version)| (name, Some(version)));
    let name = if let Some((name, extras)) = name.split_once('[') {
        let extras = extras.strip_suffix(']')?;
        if extras.is_empty() || !extras.split(',').all(|extra| python_name(extra).is_some()) {
            return None;
        }
        name
    } else {
        name
    };
    let version = match version {
        Some(version)
            if version.is_empty()
                || version.len() > 64
                || !version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".-+_".contains(&byte)) =>
        {
            return None
        }
        Some(version) => Some(version.into()),
        None => None,
    };
    Some(PackageRequest {
        name: python_name(name)?,
        version,
    })
}

fn text(value: Option<&str>, limit: usize) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn safe_url(value: Option<&str>) -> Option<String> {
    let value = text(value, 2048)?;
    let mut url = reqwest::Url::parse(value.strip_prefix("git+").unwrap_or(&value)).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

fn npm_manifest(
    path: &Path,
    request: Option<&PackageRequest>,
    executable: Option<&Path>,
    budget: &mut ReadBudget,
) -> Option<LocalPackageMetadata> {
    let document: Value = serde_json::from_str(&budget.read(path)?).ok()?;
    let name = text(document.get("name").and_then(Value::as_str), 256)?;
    if npm_request(&name)?.name != name {
        return None;
    }
    if let Some(executable) = executable {
        let root = path.parent()?;
        let bin = document.get("bin")?;
        let targets: Vec<_> = bin
            .as_str()
            .into_iter()
            .chain(
                bin.as_object()
                    .into_iter()
                    .flat_map(|bins| bins.values().filter_map(Value::as_str)),
            )
            .collect();
        if !targets.into_iter().any(|target| {
            root.join(target)
                .canonicalize()
                .is_ok_and(|target| target == executable)
        }) {
            return None;
        }
    }
    let version = text(document.get("version").and_then(Value::as_str), 64);
    if request.is_some_and(|request| {
        request.name != name
            || request
                .version
                .as_ref()
                .is_some_and(|wanted| Some(wanted) != version.as_ref())
    }) {
        return None;
    }
    let repository = document.get("repository").and_then(|repository| {
        repository
            .as_str()
            .or_else(|| repository.get("url").and_then(Value::as_str))
    });
    Some(LocalPackageMetadata {
        name,
        version,
        description: text(document.get("description").and_then(Value::as_str), 512),
        homepage: safe_url(document.get("homepage").and_then(Value::as_str)),
        repository: safe_url(repository),
        manifest: path.to_path_buf(),
    })
}

fn npm_metadata(
    request: &PackageRequest,
    cwd: &Path,
    workspace: &Path,
    environment: &DiscoveryEnvironment,
    executable: Option<&Path>,
    budget: &mut ReadBudget,
) -> Option<LocalPackageMetadata> {
    let mut roots: Vec<_> = cwd
        .ancestors()
        .take_while(|path| path.starts_with(workspace))
        .map(|path| path.join("node_modules"))
        .collect();
    roots.extend(environment.npm_roots.iter().take(16).cloned());
    if let Some(prefix) = executable.and_then(Path::parent).and_then(Path::parent) {
        roots.push(prefix.join("lib/node_modules"));
    }
    if let Some(cache) = &environment.npm_cache {
        roots.extend(
            budget
                .directories(&cache.join("_npx"))
                .into_iter()
                .map(|path| path.join("node_modules")),
        );
    }
    roots.into_iter().find_map(|root| {
        npm_manifest(
            &root.join(&request.name).join("package.json"),
            Some(request),
            None,
            budget,
        )
    })
}

fn python_metadata(
    request: &PackageRequest,
    roots: &[PathBuf],
    budget: &mut ReadBudget,
) -> Option<LocalPackageMetadata> {
    let mut queue: VecDeque<_> = roots.iter().take(8).map(|root| (root.clone(), 0)).collect();
    while let Some((directory, depth)) = queue.pop_front() {
        if directory
            .extension()
            .is_some_and(|extension| extension == "dist-info")
        {
            let Some(contents) = budget.read(&directory.join("METADATA")) else {
                continue;
            };
            let fields: Vec<_> = contents
                .lines()
                .take_while(|line| !line.is_empty())
                .filter_map(|line| line.split_once(": "))
                .collect();
            let field = |key| {
                fields
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| *value)
            };
            if field("Name").and_then(python_name).as_deref() != Some(request.name.as_str()) {
                continue;
            }
            let version = text(field("Version"), 64);
            if request
                .version
                .as_ref()
                .is_some_and(|wanted| Some(wanted) != version.as_ref())
            {
                continue;
            }
            let project = |names: &[&str]| {
                fields
                    .iter()
                    .filter(|(key, _)| *key == "Project-URL")
                    .filter_map(|(_, value)| value.split_once(','))
                    .find(|(name, _)| {
                        names
                            .iter()
                            .any(|wanted| name.trim().eq_ignore_ascii_case(wanted))
                    })
                    .and_then(|(_, value)| safe_url(Some(value.trim())))
            };
            return Some(LocalPackageMetadata {
                name: request.name.clone(),
                version,
                description: text(field("Summary"), 512),
                homepage: safe_url(field("Home-page")).or_else(|| project(&["Homepage", "Home"])),
                repository: project(&["Source", "Repository", "Source Code"]),
                manifest: directory.join("METADATA"),
            });
        }
        if depth < 6 {
            queue.extend(
                budget
                    .directories(&directory)
                    .into_iter()
                    .map(|path| (path, depth + 1)),
            );
        }
    }
    None
}

/// A result is accepted only for the same saved registrations and workspace.
pub(crate) struct McpDiscoveryController {
    workspace: PathBuf,
    scope: McpSettingsScope,
    servers: Vec<McpServerSettings>,
    results: Receiver<BTreeMap<String, McpLocalDiscovery>>,
    terminal_seen: bool,
    invalidated: bool,
}

impl McpDiscoveryController {
    pub fn start(
        workspace: &Path,
        scope: McpSettingsScope,
        servers: Vec<McpServerSettings>,
    ) -> Result<Self, String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|_| "El proyecto MCP no está disponible".to_owned())?;
        let worker_workspace = workspace.clone();
        let worker_servers = servers.clone();
        let environment = DiscoveryEnvironment::from_system();
        let (sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("mcp-discovery".into())
            .spawn(move || {
                let _ = sender.send(inspect_batch(
                    &worker_servers,
                    &worker_workspace,
                    &environment,
                ));
            })
            .map_err(|_| "No se pudo iniciar la detección MCP".to_owned())?;
        Ok(Self {
            workspace,
            scope,
            servers,
            results,
            terminal_seen: false,
            invalidated: false,
        })
    }

    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    pub fn matches_saved(
        &self,
        workspace: &Path,
        scope: McpSettingsScope,
        servers: &[McpServerSettings],
    ) -> bool {
        !self.invalidated
            && self.scope == scope
            && self.servers == servers
            && workspace
                .canonicalize()
                .is_ok_and(|workspace| workspace == self.workspace)
    }

    pub fn try_recv(&mut self) -> Option<Result<BTreeMap<String, McpLocalDiscovery>, String>> {
        if self.terminal_seen {
            return None;
        }
        let result = match self.results.try_recv() {
            Ok(found) => Some(Ok(found)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("La detección MCP terminó sin resultado".into()))
            }
        };
        self.terminal_seen = result.is_some();
        result
    }
}

#[cfg(test)]
#[path = "mcp_discovery_tests.rs"]
mod tests;
