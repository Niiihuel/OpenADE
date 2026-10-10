//! Public Skills.sh discovery and explicit, commit-pinned GitHub installations.
//! All methods doing I/O belong on the host worker. Discovery only caches metadata.
use super::*;
use reqwest::{blocking::Client, redirect::Policy, Url};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    io::Write,
    time::{Duration, Instant},
};

const SEARCH_ORIGIN: &str = "https://skills.sh";
const GITHUB_ORIGIN: &str = "https://api.github.com";
const RAW_ORIGIN: &str = "https://raw.githubusercontent.com";
const SEARCH_LIMIT: usize = 100;
const SEARCH_BYTES: u64 = 512 * 1024;
const CACHE_BYTES: u64 = 4 * 1024 * 1024;
const CACHE_QUERIES: usize = 24;
const CACHE_TTL: u64 = 15 * 60;
const CACHE_MAX_AGE: u64 = 30 * 24 * 60 * 60;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);
const TREE_BYTES: u64 = 8 * 1024 * 1024;
const TREE_ENTRIES: usize = 32_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteSkill {
    pub id: String,
    pub name: String,
    /// Validated owner/repository, never an arbitrary URL.
    pub source: String,
    pub installs: u64,
}

impl RemoteSkill {
    pub fn item(&self) -> MarketplaceItem {
        MarketplaceItem {
            id: self.id.clone(), name: self.name.clone(),
            description: format!("Skill publicada por {} en Skills.sh. La licencia y las instrucciones se verifican al instalar.", self.source),
            license: None, source_name: self.source.clone(), source_kind: SourceKind::Github,
            installed_scope: None, enabled: false, source_available: true,
        }
    }

    fn validate(&self) -> io::Result<()> {
        validate_repository(&self.source)?;
        if !valid_skill_name(&self.name)
            || self.id != format!("skills-sh:{}:{}", self.source, self.name)
        {
            return Err(invalid("Identidad de skill remota inválida"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteSearchPage {
    pub query: String,
    pub skills: Vec<RemoteSkill>,
    pub from_cache: bool,
    pub stale: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillProvenance {
    pub repository: String,
    pub revision: String,
    pub directory: String,
    #[serde(default)]
    pub manifest_digest: Option<String>,
}

impl SkillProvenance {
    pub(super) fn validate(&self) -> io::Result<()> {
        validate_repository(&self.repository)?;
        if !valid_sha(&self.revision)
            || (!self.directory.is_empty() && !safe_relative(&self.directory))
            || self.manifest_digest.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            })
        {
            return Err(invalid("Procedencia de skill inválida"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillUpdate {
    pub item_id: String,
    pub installed_revision: Option<String>,
    pub available_revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedSearch {
    query: String,
    fetched_at: u64,
    skills: Vec<RemoteSkill>,
}
#[derive(Default, Serialize, Deserialize)]
struct SearchCache {
    entries: Vec<CachedSearch>,
}

#[derive(Deserialize)]
struct SearchResponse {
    skills: Vec<SearchHit>,
}
#[derive(Deserialize)]
struct SearchHit {
    id: String,
    name: String,
    source: String,
    #[serde(default)]
    installs: u64,
}

pub(super) fn remote_identity(id: &str) -> Option<RemoteSkill> {
    let (source, name) = id.strip_prefix("skills-sh:")?.rsplit_once(':')?;
    let skill = RemoteSkill {
        id: id.into(),
        name: name.into(),
        source: source.into(),
        installs: 0,
    };
    skill.validate().ok().map(|_| skill)
}

pub(super) fn starter_provenance(id: &str) -> Option<SkillProvenance> {
    let details = catalog::starter_details(id)?;
    Some(SkillProvenance {
        repository: details
            .repository_url
            .strip_prefix("https://github.com/")?
            .into(),
        revision: details.revision,
        directory: details.directory,
        manifest_digest: catalog::starter_manifest_digest(id),
    })
}

impl MarketplaceStore {
    /// Public, credential-free endpoint also used by the official skills CLI.
    /// A stale cached response is explicitly labelled; a cache miss never looks
    /// like a successful empty search after a network failure.
    pub fn search_remote(&self, query: &str, force_refresh: bool) -> io::Result<RemoteSearchPage> {
        self.search_remote_from(query, force_refresh, &configured_origin(SEARCH_ORIGIN))
    }

    fn search_remote_from(
        &self,
        query: &str,
        force_refresh: bool,
        origin: &str,
    ) -> io::Result<RemoteSearchPage> {
        let query = query.trim();
        if !(2..=120).contains(&query.chars().count()) || query.chars().any(char::is_control) {
            return Err(invalid("La búsqueda necesita entre 2 y 120 caracteres"));
        }
        let now = unix_now();
        let mut cache = self.read_search_cache()?;
        let cached = cache
            .entries
            .iter()
            .find(|entry| entry.query == query)
            .cloned()
            .filter(|entry| now.saturating_sub(entry.fetched_at) <= CACHE_MAX_AGE);
        if !force_refresh {
            if let Some(entry) = cached
                .as_ref()
                .filter(|entry| now.saturating_sub(entry.fetched_at) <= CACHE_TTL)
            {
                return Ok(RemoteSearchPage {
                    query: query.into(),
                    skills: entry.skills.clone(),
                    from_cache: true,
                    stale: false,
                    warning: None,
                });
            }
        }
        let result = (|| {
            let mut url = Url::parse(&format!("{}/api/search", origin.trim_end_matches('/')))
                .map_err(io::Error::other)?;
            url.query_pairs_mut()
                .append_pair("q", query)
                .append_pair("limit", &SEARCH_LIMIT.to_string());
            let http = Http::configured()?;
            let bytes = http.get(url.as_str(), SEARCH_BYTES)?;
            let response: SearchResponse = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("Respuesta de búsqueda inválida"))?;
            let mut seen = HashSet::new();
            let mut skills = Vec::new();
            for hit in response.skills.into_iter().take(SEARCH_LIMIT) {
                // The public index can contain unsupported hosts and malformed
                // metadata. Never turn these into fetch targets or local paths.
                let skill = RemoteSkill {
                    id: format!("skills-sh:{}:{}", hit.source, hit.name),
                    name: hit.name,
                    source: hit.source,
                    installs: hit.installs,
                };
                if skill.validate().is_ok()
                    && hit.id == format!("{}/{}", skill.source, skill.name)
                    && seen.insert(skill.id.clone())
                {
                    skills.push(skill);
                }
            }
            skills.sort_by(|a, b| b.installs.cmp(&a.installs).then_with(|| a.id.cmp(&b.id)));
            Ok(skills)
        })();
        match result {
            Ok(skills) => {
                cache.entries.retain(|entry| {
                    entry.query != query && now.saturating_sub(entry.fetched_at) <= CACHE_MAX_AGE
                });
                cache.entries.push(CachedSearch {
                    query: query.into(),
                    fetched_at: now,
                    skills: skills.clone(),
                });
                cache.entries.sort_by_key(|entry| entry.fetched_at);
                if cache.entries.len() > CACHE_QUERIES {
                    cache.entries.drain(..cache.entries.len() - CACHE_QUERIES);
                }
                let warning = self
                    .write_search_cache(&cache)
                    .err()
                    .map(|_| "Resultados recibidos; no se pudo guardar la caché local".into());
                Ok(RemoteSearchPage {
                    query: query.into(),
                    skills,
                    from_cache: false,
                    stale: false,
                    warning,
                })
            }
            Err(error) => cached
                .map(|entry| RemoteSearchPage {
                    query: query.into(),
                    skills: entry.skills,
                    from_cache: true,
                    stale: true,
                    warning: Some(format!(
                        "Skills.sh no está disponible; se muestran resultados guardados. {error}"
                    )),
                })
                .ok_or(error),
        }
    }

    fn cache_path(&self) -> PathBuf {
        self.store_root().join("remote-search.json")
    }
    fn read_search_cache(&self) -> io::Result<SearchCache> {
        let bytes = match bounded_file(&self.cache_path(), CACHE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(SearchCache::default()),
            // Metadata cache is disposable; corruption must not block a fresh query.
            Err(error) if error.kind() == ErrorKind::InvalidData => {
                return Ok(SearchCache::default())
            }
            Err(error) => return Err(error),
        };
        let Ok(mut cache) = serde_json::from_slice::<SearchCache>(&bytes) else {
            return Ok(SearchCache::default());
        };
        cache.entries.retain(|entry| {
            entry.query.chars().count() <= 120
                && !entry.query.chars().any(char::is_control)
                && entry.skills.len() <= SEARCH_LIMIT
                && entry.skills.iter().all(|skill| skill.validate().is_ok())
        });
        cache.entries.truncate(CACHE_QUERIES);
        Ok(cache)
    }
    fn write_search_cache(&self, cache: &SearchCache) -> io::Result<()> {
        self.ensure_install_parent(&self.store_root())?;
        let bytes = serde_json::to_vec(cache).map_err(io::Error::other)?;
        if bytes.len() as u64 > CACHE_BYTES {
            return Err(invalid("Caché de búsqueda demasiado grande"));
        }
        atomic_metadata(&self.cache_path(), &bytes)
    }
    pub fn cached_remote_skill(&self, id: &str) -> io::Result<Option<RemoteSkill>> {
        Ok(self
            .read_search_cache()?
            .entries
            .into_iter()
            .rev()
            .flat_map(|entry| entry.skills)
            .find(|skill| skill.id == id))
    }
    pub fn installed_provenance(&self, item_id: &str) -> io::Result<Option<SkillProvenance>> {
        Ok(self
            .load_registry()?
            .installed
            .into_iter()
            .find(|entry| entry.item_id == item_id && self.applies(entry))
            .and_then(|entry| entry.provenance))
    }

    pub fn install_remote(&self, skill: &RemoteSkill, scope: InstallScope) -> io::Result<()> {
        let http = Http::configured()?;
        self.install_remote_from(skill, scope, &http)
    }
    fn install_remote_from(
        &self,
        skill: &RemoteSkill,
        scope: InstallScope,
        http: &Http,
    ) -> io::Result<()> {
        skill.validate()?;
        let registry = self.load_registry()?;
        if registry.installed.iter().any(|entry| {
            entry.item_id == skill.id && self.applies(entry)
                || entry.name == skill.name
                    && entry.scope == scope
                    && (scope == InstallScope::User || self.applies(entry))
        }) {
            return Err(already_exists(
                "La skill o su nombre ya están instalados en este ámbito",
            ));
        }
        let destination = self.destination(&skill.name, scope);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(already_exists("La carpeta destino ya existe"));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Destino inválido"))?;
        self.ensure_install_parent(parent)?;
        let staging = parent.join(format!(".remote-{}-{}", skill.name, nonce()));
        let result = (|| {
            let prepared = http.prepare(skill, None, &staging)?;
            let entry = Installation {
                item_id: skill.id.clone(),
                name: skill.name.clone(),
                description: prepared.description,
                license: prepared.license,
                source_name: format!("{} · {}", skill.source, &prepared.provenance.revision[..7]),
                source_kind: SourceKind::Github,
                scope,
                workspace: (scope == InstallScope::Workspace).then(|| self.workspace_key()),
                enabled: true,
                provenance: Some(prepared.provenance),
            };
            self.publish_installation(registry, entry, &staging, false)
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Explicit refresh only; checking revisions never changes installed files.
    /// Errors include the source name instead of falsely marking it up to date.
    pub fn check_updates(&self) -> io::Result<Vec<SkillUpdate>> {
        self.check_updates_from(&Http::configured()?)
    }
    fn check_updates_from(&self, http: &Http) -> io::Result<Vec<SkillUpdate>> {
        let registry = self.load_registry()?;
        let mut updates = Vec::new();
        let mut heads = HashMap::<String, String>::new();
        for entry in registry
            .installed
            .iter()
            .filter(|entry| self.applies(entry))
        {
            if let Some(current) = starter_provenance(&entry.item_id) {
                if entry.provenance.as_ref() != Some(&current) {
                    updates.push(SkillUpdate {
                        item_id: entry.item_id.clone(),
                        installed_revision: entry.provenance.as_ref().map(|p| p.revision.clone()),
                        available_revision: current.revision,
                    });
                }
            } else if let Some(remote) = remote_identity(&entry.item_id) {
                let head = if let Some(head) = heads.get(&remote.source) {
                    head.clone()
                } else {
                    let head = http
                        .head(&remote.source)
                        .map_err(|error| {
                            io::Error::new(error.kind(), format!("{}: {error}", remote.source))
                        })?
                        .0;
                    heads.insert(remote.source.clone(), head.clone());
                    head
                };
                if entry.provenance.as_ref().map(|p| p.revision.as_str()) != Some(head.as_str()) {
                    updates.push(SkillUpdate {
                        item_id: entry.item_id.clone(),
                        installed_revision: entry.provenance.as_ref().map(|p| p.revision.clone()),
                        available_revision: head,
                    });
                }
            }
        }
        Ok(updates)
    }

    /// Replace a complete validated bundle, preserving scope and enabled state.
    /// The previous directory is restored when publishing its registry fails.
    pub fn update(&self, item_id: &str) -> io::Result<()> {
        self.update_from(item_id, &Http::configured()?, catalog::download)
    }
    fn update_from(
        &self,
        item_id: &str,
        http: &Http,
        download: impl FnOnce(&catalog::StarterEntry, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let registry = self.load_registry()?;
        let mut entry = registry
            .installed
            .iter()
            .find(|entry| entry.item_id == item_id && self.applies(entry))
            .cloned()
            .ok_or_else(|| not_found("La skill no está instalada"))?;
        let destination = self.destination(&entry.name, entry.scope);
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("Destino inválido"))?;
        self.ensure_install_parent(parent)?;
        let staging = parent.join(format!(".update-{}-{}", entry.name, nonce()));
        let result = (|| {
            if let Some(starter) = catalog::entry(item_id) {
                download(starter, &staging)?;
                let item = starter.item();
                entry.description = item.description;
                entry.license = item.license;
                entry.source_name = item.source_name;
                entry.provenance = starter_provenance(item_id);
            } else if let Some(skill) = remote_identity(item_id) {
                let prepared = http.prepare(
                    &skill,
                    entry.provenance.as_ref().map(|p| p.directory.as_str()),
                    &staging,
                )?;
                entry.description = prepared.description;
                entry.license = prepared.license;
                entry.source_name =
                    format!("{} · {}", skill.source, &prepared.provenance.revision[..7]);
                entry.provenance = Some(prepared.provenance);
            } else {
                let skill = registry
                    .sources
                    .iter()
                    .filter_map(|source| self.scan_source(source).ok())
                    .flatten()
                    .find(|skill| skill.item.id == item_id)
                    .ok_or_else(|| not_found("El origen de esta skill no está disponible"))?;
                copy_bundle(&skill.directory, &staging)?;
                entry.description = skill.item.description;
                entry.license = skill.item.license;
                entry.source_name = skill.item.source_name;
            }
            self.publish_installation(registry, entry, &staging, true)
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    fn publish_installation(
        &self,
        registry: Registry,
        entry: Installation,
        staging: &Path,
        replace: bool,
    ) -> io::Result<()> {
        self.publish_installation_with(registry, entry, staging, replace, |registry| {
            self.save_registry(registry)
        })
    }
    fn publish_installation_with(
        &self,
        mut registry: Registry,
        entry: Installation,
        staging: &Path,
        replace: bool,
        save: impl FnOnce(&Registry) -> io::Result<()>,
    ) -> io::Result<()> {
        // Locate the record before touching files, including failure paths.
        let replacing = if replace {
            Some(
                registry
                    .installed
                    .iter()
                    .position(|previous| {
                        previous.item_id == entry.item_id
                            && previous.scope == entry.scope
                            && previous.workspace == entry.workspace
                    })
                    .ok_or_else(|| not_found("La instalación cambió antes de actualizarse"))?,
            )
        } else {
            None
        };

        let destination = self.destination(&entry.name, entry.scope);
        let backup = destination.with_file_name(format!(".previous-{}-{}", entry.name, nonce()));
        match fs::symlink_metadata(&destination) {
            Ok(meta)
                if replace
                    && meta.is_dir()
                    && !meta.file_type().is_symlink()
                    && destination
                        .canonicalize()?
                        .starts_with(self.store_root().canonicalize()?) =>
            {
                fs::rename(&destination, &backup)?
            }
            Ok(_) => return Err(already_exists("El destino cambió o contiene un enlace")),
            Err(error) if error.kind() == ErrorKind::NotFound && !replace => {}
            Err(error) => return Err(error),
        }
        if let Err(error) = fs::rename(staging, &destination) {
            if replace {
                let _ = fs::rename(&backup, &destination);
            }
            return Err(error);
        }
        if let Some(index) = replacing {
            registry.installed[index] = entry;
        } else {
            registry.installed.push(entry);
        }
        if let Err(error) = save(&registry) {
            let _ = fs::remove_dir_all(&destination);
            if replace {
                let _ = fs::rename(&backup, &destination);
            }
            return Err(error);
        }
        if replace {
            let _ = fs::remove_dir_all(&backup);
        }
        Ok(())
    }
}

struct Http {
    client: Client,
    github: String,
    raw: String,
    deadline: Instant,
}
#[derive(Deserialize)]
struct CommitResponse {
    sha: String,
    commit: CommitData,
}
#[derive(Deserialize)]
struct CommitData {
    tree: TreeIdentity,
}
#[derive(Deserialize)]
struct TreeIdentity {
    sha: String,
}
#[derive(Clone, Deserialize)]
struct GitFile {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    size: Option<u64>,
}
#[derive(Deserialize)]
struct GitTree {
    #[serde(default)]
    truncated: bool,
    tree: Vec<GitFile>,
}
struct Prepared {
    description: String,
    license: Option<String>,
    provenance: SkillProvenance,
}

impl Http {
    fn configured() -> io::Result<Self> {
        Self::new(
            &configured_origin(GITHUB_ORIGIN),
            &configured_origin(RAW_ORIGIN),
        )
    }
    fn new(github: &str, raw: &str) -> io::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(REQUEST_TIMEOUT)
                .user_agent("OpenIDE-skill-marketplace")
                .build()
                .map_err(io::Error::other)?,
            github: github.into(),
            raw: raw.into(),
            deadline: Instant::now() + OPERATION_TIMEOUT,
        })
    }
    fn get(&self, url: &str, limit: u64) -> io::Result<Vec<u8>> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(ErrorKind::TimedOut, "La operación superó el tiempo límite")
            })?;
        let response = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .timeout(remaining.min(REQUEST_TIMEOUT))
            .send()
            .map_err(|e| {
                io::Error::new(
                    if e.is_timeout() {
                        ErrorKind::TimedOut
                    } else {
                        ErrorKind::Other
                    },
                    "No se pudo conectar con el catálogo remoto",
                )
            })?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "El catálogo remoto respondió HTTP {}",
                response.status().as_u16()
            )));
        }
        if response.content_length().is_some_and(|size| size > limit) {
            return Err(invalid("La respuesta remota supera el límite permitido"));
        }
        let mut bytes = Vec::new();
        response.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(invalid("La respuesta remota supera el límite permitido"));
        }
        Ok(bytes)
    }
    fn head(&self, source: &str) -> io::Result<(String, String)> {
        validate_repository(source)?;
        let data: CommitResponse = serde_json::from_slice(&self.get(
            &format!("{}/repos/{source}/commits/HEAD", self.github),
            SEARCH_BYTES,
        )?)
        .map_err(|_| invalid("Commit de GitHub inválido"))?;
        if !valid_sha(&data.sha) || !valid_sha(&data.commit.tree.sha) {
            return Err(invalid("Revisión de GitHub inválida"));
        }
        Ok((data.sha, data.commit.tree.sha))
    }
    fn raw_file(
        &self,
        source: &str,
        revision: &str,
        file: &GitFile,
        limit: u64,
    ) -> io::Result<Vec<u8>> {
        regular_file(file)?;
        let size = file
            .size
            .ok_or_else(|| invalid("GitHub no informó el tamaño del archivo"))?;
        if size > limit {
            return Err(invalid("El archivo de la skill supera el límite"));
        }
        let mut url = Url::parse(&self.raw).map_err(io::Error::other)?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| invalid("Origen de descarga inválido"))?;
            path.pop_if_empty();
            for part in source
                .split('/')
                .chain(std::iter::once(revision))
                .chain(file.path.split('/'))
            {
                path.push(part);
            }
        }
        let bytes = self.get(url.as_str(), limit)?;
        if bytes.len() as u64 != size || git_blob_hash(&bytes) != file.sha.to_ascii_lowercase() {
            return Err(invalid(format!(
                "El archivo {} no coincide con su objeto Git publicado",
                file.path
            )));
        }
        Ok(bytes)
    }
    fn prepare(
        &self,
        skill: &RemoteSkill,
        preferred: Option<&str>,
        destination: &Path,
    ) -> io::Result<Prepared> {
        skill.validate()?;
        let (revision, tree_id) = self.head(&skill.source)?;
        let tree: GitTree = serde_json::from_slice(&self.get(
            &format!(
                "{}/repos/{}/git/trees/{tree_id}?recursive=1",
                self.github, skill.source
            ),
            TREE_BYTES,
        )?)
        .map_err(|_| invalid("Árbol de GitHub inválido"))?;
        if tree.truncated || tree.tree.len() > TREE_ENTRIES {
            return Err(invalid(
                "El repositorio es demasiado grande para resolver la skill de forma segura",
            ));
        }
        let mut candidates: Vec<&GitFile> = tree
            .tree
            .iter()
            .filter(|file| file.path == "SKILL.md" || file.path.ends_with("/SKILL.md"))
            .collect();
        candidates.retain(|file| safe_relative(&file.path));
        let expected = |file: &&GitFile| {
            parent_path(&file.path).rsplit('/').next() == Some(skill.name.as_str())
        };
        let exact: Vec<_> = candidates.iter().copied().filter(expected).collect();
        if !exact.is_empty() {
            candidates = exact;
        }
        if let Some(preferred) = preferred {
            candidates.sort_by_key(|file| parent_path(&file.path) != preferred);
        }
        if candidates.len() > 16 {
            return Err(invalid("Hay demasiadas skills candidatas; no se puede elegir esta identidad sin ambigüedad"));
        }
        let mut matched = None;
        for file in candidates {
            let bytes = self.raw_file(&skill.source, &revision, file, MAX_SKILL_FILE_BYTES)?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| invalid("SKILL.md no contiene UTF-8 válido"))?;
            if let Some((name, description, license)) = parse_frontmatter(text) {
                if name == skill.name {
                    if license.as_ref().is_some_and(|license| {
                        license.len() > 2048
                            || license
                                .chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                    }) {
                        return Err(invalid("La licencia declarada no es válida"));
                    }
                    if matched.is_some() {
                        return Err(invalid(
                            "El repositorio contiene varias skills con la misma identidad",
                        ));
                    }
                    matched = Some((
                        parent_path(&file.path).to_owned(),
                        description,
                        license,
                        bytes,
                    ));
                }
            }
        }
        let (directory, description, mut license, skill_bytes) = matched.ok_or_else(|| {
            not_found("No se encontró un SKILL.md válido con ese nombre en GitHub")
        })?;
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let mut files = Vec::<(&GitFile, String)>::new();
        let mut total = 0u64;
        let mut paths = HashSet::new();
        let mut directories = HashSet::new();
        for file in &tree.tree {
            let Some(relative) = file.path.strip_prefix(&prefix) else {
                continue;
            };
            if file.kind == "tree" {
                continue;
            }
            regular_file(file)?;
            if !safe_relative(relative) || !paths.insert(relative.to_owned()) {
                return Err(invalid("Ruta repetida o insegura en el bundle"));
            }
            let mut dir = parent_path(relative);
            while !dir.is_empty() {
                directories.insert(dir.to_owned());
                dir = parent_path(dir);
            }
            total = total
                .checked_add(file.size.unwrap_or(u64::MAX))
                .ok_or_else(|| invalid("Bundle demasiado grande"))?;
            if total > MAX_BUNDLE_BYTES
                || files.len() >= MAX_BUNDLE_FILES
                || directories.len() > MAX_BUNDLE_DIRECTORIES
            {
                return Err(invalid("La skill supera los límites de archivos o tamaño"));
            }
            files.push((file, relative.to_owned()));
        }
        // Preserve a repository-wide license in a distinct namespace when it
        // lives above the skill. Never claim a license based only on its name.
        if !directory.is_empty() {
            for file in tree.tree.iter().filter(|file| {
                !file.path.contains('/')
                    && matches!(
                        file.path.to_ascii_lowercase().as_str(),
                        "license" | "license.md" | "license.txt" | "copying" | "notice"
                    )
            }) {
                regular_file(file)?;
                let path = format!("_upstream/{}", file.path);
                if paths.contains(&path) {
                    continue;
                }
                total = total
                    .checked_add(file.size.unwrap_or(u64::MAX))
                    .ok_or_else(|| invalid("Bundle demasiado grande"))?;
                if total > MAX_BUNDLE_BYTES || files.len() >= MAX_BUNDLE_FILES {
                    return Err(invalid("La licencia supera el límite del bundle"));
                }
                files.push((file, path));
                if license.is_none() {
                    license = Some("Consultar los archivos de licencia en _upstream/".into());
                }
            }
        }
        fs::create_dir(destination)?;
        let mut digest = Sha256::new();
        files.sort_by(|a, b| a.1.cmp(&b.1));
        for (file, relative) in files {
            let bytes = if relative == "SKILL.md" {
                skill_bytes.clone()
            } else {
                self.raw_file(&skill.source, &revision, file, MAX_BUNDLE_BYTES)?
            };
            let path = destination.join(&relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            output.write_all(&bytes)?;
            digest.update(relative.as_bytes());
            digest.update([0]);
            digest.update(file.sha.as_bytes());
            digest.update([0]);
        }
        Ok(Prepared {
            description,
            license,
            provenance: SkillProvenance {
                repository: skill.source.clone(),
                revision,
                directory,
                manifest_digest: Some(format!("{:x}", digest.finalize())),
            },
        })
    }
}

// Native integration fixtures can replace the three public origins in an
// explicitly built E2E binary. Production builds have no endpoint override.
fn configured_origin(default: &str) -> String {
    #[cfg(feature = "editor-e2e")]
    if let Ok(value) = std::env::var("OPENIDE_E2E_MARKETPLACE_ORIGIN") {
        if let Ok(url) = Url::parse(&value) {
            if url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/"
                && url.port().is_some()
            {
                return value.trim_end_matches('/').to_owned();
            }
        }
    }
    default.to_owned()
}

fn validate_repository(source: &str) -> io::Result<()> {
    let Some((owner, repo)) = source.split_once('/') else {
        return Err(invalid("Origen GitHub inválido"));
    };
    if owner.is_empty()
        || owner.len() > 39
        || owner.starts_with('-')
        || owner.ends_with('-')
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || repo.is_empty()
        || repo.len() > 100
        || matches!(repo, "." | "..")
        || repo.starts_with('.')
        || !repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid(
            "Sólo se aceptan repositorios públicos con formato owner/repository de GitHub",
        ));
    }
    Ok(())
}
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && part.len() <= 255
                && !part.chars().any(|c| {
                    c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
                })
                && !matches!(
                    stem.as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}
fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
fn regular_file(file: &GitFile) -> io::Result<()> {
    if file.kind != "blob"
        || !matches!(file.mode.as_str(), "100644" | "100755")
        || !valid_sha(&file.sha)
        || !safe_relative(&file.path)
    {
        return Err(invalid(
            "El bundle contiene enlaces, submódulos o rutas inválidas",
        ));
    }
    Ok(())
}
fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}
fn git_blob_hash(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", bytes.len()));
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn bounded_file(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(io::Error::new(ErrorKind::InvalidData, "Caché inválida"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "Caché demasiado grande",
        ));
    }
    Ok(bytes)
}
fn atomic_metadata(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staging = path.with_extension(format!("{}.tmp", nonce()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if cfg!(windows) && path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&staging, path)
    })();
    if staging.exists() {
        let _ = fs::remove_file(staging);
    }
    result
}

#[cfg(test)]
#[path = "marketplace_remote_tests.rs"]
mod tests;
