//! Local marketplace inventory for portable `SKILL.md` capabilities.
//!
//! OpenCode discovers skills in `.agents/skills` and `.opencode/skills` and
//! keeps npm/JavaScript plugins separate. OpenIDE only installs skill folders;
//! it never evaluates plugin code or starts MCP servers while browsing them.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::child_process::command_no_window;

const MAX_SKILL_FILE_BYTES: u64 = 512 * 1024;
const MAX_BUNDLE_FILES: usize = 512;
const MAX_BUNDLE_DIRECTORIES: usize = 512;
const MAX_BUNDLE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ACTIVE_SKILLS: usize = 32;
const MAX_ACTIVE_INSTRUCTIONS_BYTES: usize = 96 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallScope {
    User,
    Workspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Local,
    Github,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketplaceSource {
    pub id: String,
    pub name: String,
    pub location: String,
    pub kind: SourceKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketplaceItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub source_name: String,
    pub source_kind: SourceKind,
    pub installed_scope: Option<InstallScope>,
    pub enabled: bool,
    pub source_available: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarketplaceSnapshot {
    pub sources: Vec<MarketplaceSource>,
    pub items: Vec<MarketplaceItem>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SourceRecord {
    id: String,
    name: String,
    location: String,
    kind: SourceKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Installation {
    item_id: String,
    name: String,
    description: String,
    license: Option<String>,
    source_name: String,
    source_kind: SourceKind,
    scope: InstallScope,
    workspace: Option<String>,
    enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    sources: Vec<SourceRecord>,
    #[serde(default)]
    installed: Vec<Installation>,
}

#[derive(Clone, Debug)]
struct DiscoveredSkill {
    item: MarketplaceItem,
    directory: PathBuf,
}

#[derive(Clone, Debug)]
pub struct MarketplaceStore {
    data_root: PathBuf,
    workspace: PathBuf,
}

impl MarketplaceStore {
    pub fn new(data_root: PathBuf, workspace: PathBuf) -> Self {
        Self {
            data_root,
            workspace,
        }
    }

    fn store_root(&self) -> PathBuf {
        self.data_root.join("marketplace")
    }

    fn registry_path(&self) -> PathBuf {
        self.store_root().join("registry.json")
    }

    fn workspace_key(&self) -> String {
        workspace_key(&self.workspace)
    }

    fn destination(&self, name: &str, scope: InstallScope) -> PathBuf {
        match scope {
            InstallScope::User => self.store_root().join("installed").join("user").join(name),
            InstallScope::Workspace => self
                .store_root()
                .join("installed")
                .join("workspaces")
                .join(self.workspace_key())
                .join(name),
        }
    }

    pub fn snapshot(&self) -> io::Result<MarketplaceSnapshot> {
        let registry = self.load_registry()?;
        let mut snapshot = MarketplaceSnapshot::default();
        let mut seen = HashSet::new();
        for source in &registry.sources {
            snapshot.sources.push(MarketplaceSource {
                id: source.id.clone(),
                name: source.name.clone(),
                location: source.location.clone(),
                kind: source.kind,
            });
            match self.scan_source(source) {
                Ok(skills) => {
                    for skill in skills {
                        seen.insert(skill.item.id.clone());
                        let mut item = skill.item;
                        if let Some(installed) = registry
                            .installed
                            .iter()
                            .find(|entry| entry.item_id == item.id && self.applies(entry))
                        {
                            item.installed_scope = Some(installed.scope);
                            item.enabled = installed.enabled;
                        }
                        snapshot.items.push(item);
                    }
                }
                Err(error) => snapshot
                    .diagnostics
                    .push(format!("{}: {error}", source.name)),
            }
        }
        for installed in registry
            .installed
            .iter()
            .filter(|entry| self.applies(entry) && !seen.contains(&entry.item_id))
        {
            snapshot.items.push(MarketplaceItem {
                id: installed.item_id.clone(),
                name: installed.name.clone(),
                description: installed.description.clone(),
                license: installed.license.clone(),
                source_name: installed.source_name.clone(),
                source_kind: installed.source_kind,
                installed_scope: Some(installed.scope),
                enabled: installed.enabled,
                source_available: false,
            });
        }
        snapshot.items.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(snapshot)
    }

    pub fn add_local_source(&self, path: &Path) -> io::Result<String> {
        let location = path.canonicalize()?;
        if !location.is_dir() {
            return Err(invalid("El origen debe ser una carpeta"));
        }
        let mut registry = self.load_registry()?;
        let location_string = location.to_string_lossy().into_owned();
        if registry
            .sources
            .iter()
            .any(|source| source.kind == SourceKind::Local && source.location == location_string)
        {
            return Err(already_exists("Este origen local ya está agregado"));
        }
        let id = next_source_id(&registry);
        let record = SourceRecord {
            id: id.clone(),
            name: location
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("Carpeta local")
                .to_owned(),
            location: location_string,
            kind: SourceKind::Local,
        };
        if self.scan_source(&record)?.is_empty() {
            return Err(invalid("No hay SKILL.md válidos en este origen"));
        }
        registry.sources.push(record);
        self.save_registry(&registry)?;
        Ok(id)
    }

    /// Clone one explicit GitHub repository into OpenIDE's private cache. Git
    /// receives a reconstructed URL as a single argument, never user flags.
    pub fn add_github_source(&self, url: &str) -> io::Result<String> {
        let (owner, repository) = parse_github_repository(url)?;
        let canonical_url = format!("https://github.com/{owner}/{repository}.git");
        let mut registry = self.load_registry()?;
        if registry
            .sources
            .iter()
            .any(|source| source.kind == SourceKind::Github && source.location == canonical_url)
        {
            return Err(already_exists("Este repositorio ya está agregado"));
        }
        let id = next_source_id(&registry);
        let source_root = self.store_root().join("sources");
        fs::create_dir_all(&source_root)?;
        let destination = source_root.join(&id);
        let staging = source_root.join(format!(".staging-{id}-{}", nonce()));
        let result = (|| {
            let output = command_no_window("git")
                .arg("clone")
                .arg("--depth=1")
                .arg("--no-recurse-submodules")
                .arg(&canonical_url)
                .arg(&staging)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "No se pudo clonar GitHub: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            let source = SourceRecord {
                id: id.clone(),
                name: format!("{owner}/{repository}"),
                location: canonical_url,
                kind: SourceKind::Github,
            };
            if scan_skill_folders(&staging, &source)?.is_empty() {
                return Err(invalid("El repositorio no contiene SKILL.md válidos"));
            }
            fs::rename(&staging, &destination)?;
            registry.sources.push(source);
            if let Err(error) = self.save_registry(&registry) {
                let _ = fs::remove_dir_all(&destination);
                return Err(error);
            }
            Ok(id)
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Re-clone configured GitHub catalogs, validating their skills before
    /// replacing the cached checkout. Call this from a worker: Git/network
    /// operations can take time and must not block a UI frame.
    pub fn refresh_github_sources(&self) -> io::Result<Vec<String>> {
        let registry = self.load_registry()?;
        let mut diagnostics = Vec::new();
        for source in registry
            .sources
            .iter()
            .filter(|source| source.kind == SourceKind::Github)
        {
            if let Err(error) = self.refresh_github_source(source) {
                diagnostics.push(format!("{}: {error}", source.name));
            }
        }
        Ok(diagnostics)
    }

    fn refresh_github_source(&self, source: &SourceRecord) -> io::Result<()> {
        let source_root = self.store_root().join("sources");
        fs::create_dir_all(&source_root)?;
        let destination = source_root.join(&source.id);
        let staging = source_root.join(format!(".refresh-{}-{}", source.id, nonce()));
        let backup = source_root.join(format!(".previous-{}-{}", source.id, nonce()));
        let result = (|| {
            let output = command_no_window("git")
                .arg("clone")
                .arg("--depth=1")
                .arg("--no-recurse-submodules")
                .arg(&source.location)
                .arg(&staging)
                .output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "No se pudo actualizar GitHub: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            if scan_skill_folders(&staging, source)?.is_empty() {
                return Err(invalid(
                    "El origen actualizado no contiene SKILL.md válidos",
                ));
            }
            let had_cache = destination.exists();
            if had_cache {
                if fs::symlink_metadata(&destination)?.file_type().is_symlink()
                    || !destination
                        .canonicalize()?
                        .starts_with(source_root.canonicalize()?)
                {
                    return Err(invalid("El caché del origen apunta fuera del marketplace"));
                }
                fs::rename(&destination, &backup)?;
            }
            if let Err(error) = fs::rename(&staging, &destination) {
                if had_cache {
                    let _ = fs::rename(&backup, &destination);
                }
                return Err(error);
            }
            Ok(())
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        if result.is_ok() && backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        result
    }

    pub fn remove_source(&self, source_id: &str) -> io::Result<()> {
        let mut registry = self.load_registry()?;
        let Some(index) = registry
            .sources
            .iter()
            .position(|source| source.id == source_id)
        else {
            return Err(not_found("No se encontró el origen"));
        };
        let source = registry.sources.remove(index);
        if source.kind == SourceKind::Github {
            let cache = self.store_root().join("sources").join(&source.id);
            if cache.exists()
                && (fs::symlink_metadata(&cache)?.file_type().is_symlink()
                    || !cache
                        .canonicalize()?
                        .starts_with(self.store_root().canonicalize()?))
            {
                return Err(invalid("El caché del origen apunta fuera del marketplace"));
            }
        }
        self.save_registry(&registry)?;
        if source.kind == SourceKind::Github {
            let cache = self.store_root().join("sources").join(&source.id);
            if cache.exists() {
                fs::remove_dir_all(cache)?;
            }
        }
        // Installed copies are independent of their source and stay available.
        Ok(())
    }

    pub fn install(&self, item_id: &str, scope: InstallScope) -> io::Result<()> {
        let mut registry = self.load_registry()?;
        if registry
            .installed
            .iter()
            .any(|entry| entry.item_id == item_id && self.applies(entry))
        {
            return Err(already_exists("La skill ya está instalada"));
        }
        let skill = registry
            .sources
            .iter()
            .filter_map(|source| self.scan_source(source).ok())
            .flatten()
            .find(|skill| skill.item.id == item_id)
            .ok_or_else(|| not_found("La skill no está disponible en los orígenes"))?;
        if registry.installed.iter().any(|entry| {
            entry.name == skill.item.name
                && entry.scope == scope
                && (scope == InstallScope::User
                    || entry.workspace.as_deref() == Some(self.workspace_key().as_str()))
        }) {
            return Err(already_exists(
                "Ya hay una skill con este nombre en el ámbito",
            ));
        }
        let destination = self.destination(&skill.item.name, scope);
        if destination.exists() {
            return Err(already_exists("La carpeta destino ya existe"));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Destino sin carpeta padre"))?;
        fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".staging-{}-{}", skill.item.name, nonce()));
        let result = (|| {
            copy_bundle(&skill.directory, &staging)?;
            fs::rename(&staging, &destination)?;
            registry.installed.push(Installation {
                item_id: skill.item.id,
                name: skill.item.name,
                description: skill.item.description,
                license: skill.item.license,
                source_name: skill.item.source_name,
                source_kind: skill.item.source_kind,
                scope,
                workspace: (scope == InstallScope::Workspace).then(|| self.workspace_key()),
                enabled: true,
            });
            if let Err(error) = self.save_registry(&registry) {
                let _ = fs::remove_dir_all(&destination);
                return Err(error);
            }
            Ok(())
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(staging);
        }
        result
    }

    pub fn set_enabled(&self, item_id: &str, enabled: bool) -> io::Result<()> {
        let mut registry = self.load_registry()?;
        let Some(entry) = registry
            .installed
            .iter_mut()
            .find(|entry| entry.item_id == item_id && applies_to(&self.workspace_key(), entry))
        else {
            return Err(not_found("La skill no está instalada"));
        };
        entry.enabled = enabled;
        self.save_registry(&registry)
    }

    pub fn uninstall(&self, item_id: &str) -> io::Result<()> {
        let mut registry = self.load_registry()?;
        let Some(index) = registry
            .installed
            .iter()
            .position(|entry| entry.item_id == item_id && self.applies(entry))
        else {
            return Err(not_found("La skill no está instalada"));
        };
        let entry = registry.installed.remove(index);
        let destination = self.destination(&entry.name, entry.scope);
        if destination.exists() {
            // The path is derived from our validated name and configured root,
            // not from editable registry JSON. Never follow a symlink on delete.
            if fs::symlink_metadata(&destination)?.file_type().is_symlink()
                || !destination
                    .canonicalize()?
                    .starts_with(self.store_root().canonicalize()?)
            {
                return Err(invalid("La instalación contiene un enlace simbólico"));
            }
            fs::remove_dir_all(&destination)?;
        }
        self.save_registry(&registry)
    }

    fn applies(&self, entry: &Installation) -> bool {
        applies_to(&self.workspace_key(), entry)
    }

    fn scan_source(&self, source: &SourceRecord) -> io::Result<Vec<DiscoveredSkill>> {
        let path = match source.kind {
            SourceKind::Local => PathBuf::from(&source.location),
            SourceKind::Github => self.store_root().join("sources").join(&source.id),
        };
        scan_skill_folders(&path, source)
    }

    fn load_registry(&self) -> io::Result<Registry> {
        let path = self.registry_path();
        // On Windows `rename` cannot overwrite a destination. The save path
        // uses a recoverable backup during its short swap window; if the app
        // was interrupted there, recover the last complete registry.
        let backup = self.store_root().join("registry.json.bak");
        if !path.exists() && backup.exists() {
            fs::rename(&backup, &path)?;
        }
        match fs::read(path) {
            Ok(bytes) => {
                let registry: Registry = serde_json::from_slice(&bytes).map_err(|error| {
                    invalid(format!("Registro del marketplace inválido: {error}"))
                })?;
                validate_registry(&registry)?;
                Ok(registry)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Registry::default()),
            Err(error) => Err(error),
        }
    }

    fn save_registry(&self, registry: &Registry) -> io::Result<()> {
        fs::create_dir_all(self.store_root())?;
        let path = self.registry_path();
        let backup = self.store_root().join("registry.json.bak");
        let staging = self.store_root().join(format!(".registry-{}.tmp", nonce()));
        fs::write(
            &staging,
            serde_json::to_vec_pretty(registry).map_err(io::Error::other)?,
        )?;
        // Unix can replace an existing file in one rename. Windows cannot;
        // use a backup swap there and restore it if installing the staged
        // registry fails. The backup is also recovered by `load_registry`.
        if fs::rename(&staging, &path).is_ok() {
            if backup.exists() {
                let _ = fs::remove_file(&backup);
            }
            return Ok(());
        }
        if !path.exists() {
            let _ = fs::remove_file(&staging);
            return Err(io::Error::other(
                "No se pudo guardar el registro del marketplace",
            ));
        }
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(&path, &backup)?;
        if let Err(error) = fs::rename(&staging, &path) {
            let _ = fs::rename(&backup, &path);
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
        let _ = fs::remove_file(&backup);
        Ok(())
    }
}

/// Bounded, explicit prompt material for the native harness. External CLIs
/// retain their own skill discovery rules and are not affected by this text.
pub fn active_skill_instructions(data_root: &Path, workspace: &Path) -> io::Result<String> {
    let store = MarketplaceStore::new(data_root.to_path_buf(), workspace.to_path_buf());
    let registry = store.load_registry()?;
    let mut selected = HashMap::<String, &Installation>::new();
    for entry in registry
        .installed
        .iter()
        .filter(|entry| entry.enabled && store.applies(entry))
    {
        // Workspace definitions override global definitions of the same name.
        if entry.scope == InstallScope::Workspace || !selected.contains_key(&entry.name) {
            selected.insert(entry.name.clone(), entry);
        }
    }
    let mut entries: Vec<_> = selected.into_iter().collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut output = String::new();
    for (_, entry) in entries.into_iter().take(MAX_ACTIVE_SKILLS) {
        let installed_root = store.destination(&entry.name, entry.scope);
        let skill_path = installed_root.join("SKILL.md");
        if !skill_path.is_file()
            || fs::symlink_metadata(&installed_root)?
                .file_type()
                .is_symlink()
            || !installed_root
                .canonicalize()?
                .starts_with(store.store_root().canonicalize()?)
            || fs::symlink_metadata(&skill_path)?.file_type().is_symlink()
            || !skill_path
                .canonicalize()?
                .starts_with(installed_root.canonicalize()?)
            || fs::metadata(&skill_path)?.len() > MAX_SKILL_FILE_BYTES
        {
            continue;
        }
        let body = fs::read_to_string(skill_path)?;
        let next = format!(
            "\n\n<openide_skill name=\"{}\">\n{}\n</openide_skill>",
            entry.name, body
        );
        if output.len() + next.len() > MAX_ACTIVE_INSTRUCTIONS_BYTES {
            break;
        }
        output.push_str(&next);
    }
    Ok(output)
}

fn applies_to(workspace_key: &str, entry: &Installation) -> bool {
    entry.scope == InstallScope::User || entry.workspace.as_deref() == Some(workspace_key)
}

fn scan_skill_folders(root: &Path, source: &SourceRecord) -> io::Result<Vec<DiscoveredSkill>> {
    let root = root.canonicalize()?;
    let mut candidates = Vec::new();
    for relative in [
        ".agents/skills",
        ".opencode/skills",
        ".claude/skills",
        "skills",
        "",
    ] {
        let folder = root.join(relative);
        if !folder.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&folder) {
            for entry in entries.flatten().take(256) {
                let path = entry.path();
                if path.is_dir() && path.join("SKILL.md").is_file() {
                    candidates.push(path);
                }
            }
        }
    }
    if root.join("SKILL.md").is_file() {
        candidates.push(root.clone());
    }
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for candidate in candidates {
        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(&root) {
            continue;
        }
        let name = canonical
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !valid_skill_name(name) || !seen.insert(name.to_owned()) {
            continue;
        }
        let skill_path = canonical.join("SKILL.md");
        let Ok(metadata) = fs::metadata(&skill_path) else {
            continue;
        };
        if metadata.len() > MAX_SKILL_FILE_BYTES {
            continue;
        }
        let Ok(content) = fs::read_to_string(&skill_path) else {
            continue;
        };
        let Some((manifest_name, description, license)) = parse_frontmatter(&content) else {
            continue;
        };
        if manifest_name != name {
            continue;
        }
        result.push(DiscoveredSkill {
            item: MarketplaceItem {
                id: format!("{}:{name}", source.id),
                name: name.to_owned(),
                description,
                license,
                source_name: source.name.clone(),
                source_kind: source.kind,
                installed_scope: None,
                enabled: false,
                source_available: true,
            },
            directory: canonical,
        });
    }
    result.sort_by(|left, right| left.item.name.cmp(&right.item.name));
    Ok(result)
}

fn parse_frontmatter(content: &str) -> Option<(String, String, Option<String>)> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut name = None;
    let mut description = None;
    let mut license = None;
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            "license" if !value.is_empty() => license = Some(value),
            _ => {}
        }
    }
    let name = name?;
    let description = description?;
    (closed && valid_skill_name(&name) && !description.is_empty() && description.len() <= 1024)
        .then_some((name, description, license))
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_source_id(id: &str) -> bool {
    id.strip_prefix("source-")
        .is_some_and(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn validate_registry(registry: &Registry) -> io::Result<()> {
    let mut source_ids = HashSet::new();
    for source in &registry.sources {
        if !valid_source_id(&source.id) || !source_ids.insert(&source.id) {
            return Err(invalid("ID de origen inválido en el registro"));
        }
        if source.kind == SourceKind::Github {
            let (owner, repository) = parse_github_repository(&source.location)?;
            if source.location != format!("https://github.com/{owner}/{repository}.git") {
                return Err(invalid("URL de GitHub inválida en el registro"));
            }
        }
    }
    for installation in &registry.installed {
        let Some((source_id, name)) = installation.item_id.split_once(':') else {
            return Err(invalid("ID de skill inválido en el registro"));
        };
        if !valid_source_id(source_id)
            || !valid_skill_name(&installation.name)
            || name != installation.name
        {
            return Err(invalid("Nombre de skill inválido en el registro"));
        }
    }
    Ok(())
}

fn parse_github_repository(url: &str) -> io::Result<(&str, &str)> {
    let path = url
        .trim()
        .strip_prefix("https://github.com/")
        .ok_or_else(|| invalid("Sólo se aceptan URLs HTTPS de github.com"))?;
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let (owner, repository) = path
        .split_once('/')
        .ok_or_else(|| invalid("URL de repositorio GitHub inválida"))?;
    if owner.is_empty()
        || repository.is_empty()
        || repository.contains('/')
        || ![owner, repository].iter().all(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        || owner.starts_with('.')
        || repository.starts_with('.')
    {
        return Err(invalid("URL de repositorio GitHub inválida"));
    }
    Ok((owner, repository))
}

fn copy_bundle(source: &Path, destination: &Path) -> io::Result<()> {
    let root = source.canonicalize()?;
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf())];
    let mut visited = HashSet::from([root.clone()]);
    let mut files = 0;
    let mut bytes = 0;
    while let Some((current_source, current_destination)) = stack.pop() {
        fs::create_dir_all(&current_destination)?;
        for entry in fs::read_dir(current_source)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            let canonical = entry.path().canonicalize()?;
            if metadata.file_type().is_symlink() || !canonical.starts_with(&root) {
                return Err(invalid("No se permiten enlaces simbólicos en una skill"));
            }
            let target = current_destination.join(entry.file_name());
            if metadata.is_dir() {
                if !visited.insert(canonical) || visited.len() > MAX_BUNDLE_DIRECTORIES {
                    return Err(invalid("La skill contiene demasiadas carpetas o un ciclo"));
                }
                stack.push((entry.path(), target));
            } else if metadata.is_file() {
                files += 1;
                bytes += metadata.len();
                if files > MAX_BUNDLE_FILES || bytes > MAX_BUNDLE_BYTES {
                    return Err(invalid("La skill supera el límite de archivos o tamaño"));
                }
                fs::copy(entry.path(), target)?;
            } else {
                return Err(invalid("Tipo de archivo no admitido en una skill"));
            }
        }
    }
    if files == 0 {
        return Err(invalid("La skill está vacía"));
    }
    Ok(())
}

fn workspace_key(workspace: &Path) -> String {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in canonical.to_string_lossy().to_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn next_source_id(registry: &Registry) -> String {
    let mut id = 1;
    loop {
        let candidate = format!("source-{id}");
        if registry.sources.iter().all(|source| source.id != candidate) {
            return candidate;
        }
        id += 1;
    }
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message.into())
}

fn not_found(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::NotFound, message.into())
}

fn already_exists(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::AlreadyExists, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_source(root: &Path) -> PathBuf {
        let source = root.join("catalog");
        let skill = source.join(".agents").join("skills").join("git-review");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: git-review\ndescription: Review changes in a Git repository\nlicense: MIT\n---\n\n## Steps\nRead the diff.\n",
        )
        .unwrap();
        source
    }

    #[test]
    fn local_catalog_install_toggle_and_remove() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), workspace.clone());
        let source_id = store.add_local_source(&sample_source(temp.path())).unwrap();
        let item_id = format!("{source_id}:git-review");
        let item = &store.snapshot().unwrap().items[0];
        assert_eq!(item.name, "git-review");
        assert_eq!(item.license.as_deref(), Some("MIT"));
        store.install(&item_id, InstallScope::Workspace).unwrap();
        assert!(active_skill_instructions(&store.data_root, &workspace)
            .unwrap()
            .contains("Read the diff"));
        store.set_enabled(&item_id, false).unwrap();
        assert!(active_skill_instructions(&store.data_root, &workspace)
            .unwrap()
            .is_empty());
        store.uninstall(&item_id).unwrap();
        assert_eq!(store.snapshot().unwrap().items[0].installed_scope, None);
    }

    #[test]
    fn rejects_traversal_and_invalid_github_urls() {
        for url in [
            "http://github.com/o/r",
            "https://github.com/o/r/../../x",
            "https://github.com/o/r?x=y",
            "https://github.com.evil/o/r",
        ] {
            assert!(parse_github_repository(url).is_err(), "{url}");
        }
        assert!(parse_github_repository("https://github.com/o/r.git").is_ok());
        assert!(!valid_skill_name("../outside"));
    }

    #[test]
    fn rejects_mismatched_manifest_name() {
        let temp = tempfile::tempdir().unwrap();
        let source = sample_source(temp.path());
        fs::write(
            source.join(".agents/skills/git-review/SKILL.md"),
            "---\nname: other\ndescription: Mismatch\n---\n",
        )
        .unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), temp.path().to_path_buf());
        assert!(store.add_local_source(&source).is_err());
    }

    #[test]
    fn requires_closed_frontmatter_and_recovers_interrupted_registry_swap() {
        assert!(parse_frontmatter("---\nname: demo\ndescription: A demo\n").is_none());
        let temp = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), temp.path().to_path_buf());
        let source_id = store.add_local_source(&sample_source(temp.path())).unwrap();
        let path = store.registry_path();
        let backup = store.store_root().join("registry.json.bak");
        fs::rename(&path, &backup).unwrap();
        assert_eq!(store.snapshot().unwrap().sources[0].id, source_id);
        assert!(path.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn rejects_tampered_registry_paths_before_filesystem_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), temp.path().to_path_buf());
        let source_id = store.add_local_source(&sample_source(temp.path())).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), "keep").unwrap();
        let mut registry = store.load_registry().unwrap();
        registry.installed.push(Installation {
            item_id: format!("{source_id}:git-review"),
            name: "../outside".into(),
            description: "bad".into(),
            license: None,
            source_name: "bad".into(),
            source_kind: SourceKind::Local,
            scope: InstallScope::User,
            workspace: None,
            enabled: true,
        });
        fs::write(
            store.registry_path(),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
        assert!(store.uninstall(&format!("{source_id}:git-review")).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn malformed_skill_does_not_hide_valid_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let source = sample_source(temp.path());
        let invalid = source.join(".agents/skills/broken");
        fs::create_dir_all(&invalid).unwrap();
        fs::write(invalid.join("SKILL.md"), [0xff, 0xfe]).unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), temp.path().to_path_buf());
        store.add_local_source(&source).unwrap();
        assert_eq!(store.snapshot().unwrap().items.len(), 1);
    }

    #[test]
    fn installed_copy_survives_source_removal() {
        let temp = tempfile::tempdir().unwrap();
        let store = MarketplaceStore::new(temp.path().join("data"), temp.path().to_path_buf());
        let source_id = store.add_local_source(&sample_source(temp.path())).unwrap();
        let item_id = format!("{source_id}:git-review");
        store.install(&item_id, InstallScope::User).unwrap();
        store.remove_source(&source_id).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert!(!snapshot.items[0].source_available);
        assert!(snapshot.items[0].enabled);
    }
}
