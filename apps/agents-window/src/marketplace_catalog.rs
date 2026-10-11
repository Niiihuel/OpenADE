//! Curated, immutable skill metadata. Browsing never performs network I/O.
//! Only an explicit install fetches the reviewed file list, pinned to a commit
//! and SHA-256 for each file. No git clone, archive extraction or script runs.

use super::{invalid, MarketplaceItem, SourceKind, MAX_BUNDLE_BYTES, MAX_BUNDLE_FILES};
use reqwest::{blocking::Client, redirect::Policy};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::Path,
    sync::LazyLock,
    time::{Duration, Instant},
};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const FILE_TIMEOUT: Duration = Duration::from_secs(15);
const RAW_ORIGIN: &str = "https://raw.githubusercontent.com";
const PUBLISHERS: &[(&str, &str)] = &[
    ("starter-anthropic", "anthropics/skills"),
    ("starter-vercel", "vercel-labs/agent-skills"),
    ("starter-supabase", "supabase/agent-skills"),
    ("starter-sentry", "getsentry/skills"),
    ("starter-microsoft", "microsoft/skills"),
    ("starter-cloudflare", "cloudflare/skills"),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StarterDetails {
    pub repository_url: String,
    pub revision: String,
    pub directory: String,
    pub license: String,
    pub file_count: usize,
    pub download_bytes: u64,
    pub compatibility: String,
    pub category: String,
    /// Each file an install writes, with its size in bytes.
    pub files: Vec<(String, u64)>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct StarterEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    publisher: String,
    pub license: String,
    repository: String,
    revision: String,
    directory: String,
    compatibility: String,
    #[serde(default = "default_category")]
    category: String,
    files: Vec<StarterFile>,
}

fn default_category() -> String {
    "General".into()
}

#[derive(Clone, Debug, Deserialize)]
struct StarterFile {
    path: String,
    repository_path: String,
    bytes: u64,
    sha256: String,
}

static STARTERS: LazyLock<Vec<StarterEntry>> = LazyLock::new(|| {
    let entries: Vec<StarterEntry> =
        serde_json::from_str(include_str!("../assets/marketplace/starter-catalog.json"))
            .expect("bundled starter catalog JSON");
    let mut ids = HashSet::new();
    for entry in &entries {
        validate_entry(entry).expect("bundled starter catalog bounds and provenance");
        assert!(ids.insert(&entry.id), "unique starter ID");
    }
    entries
});

pub(super) fn entry(id: &str) -> Option<&'static StarterEntry> {
    STARTERS.iter().find(|entry| entry.id == id)
}

pub(super) fn items() -> impl Iterator<Item = MarketplaceItem> {
    STARTERS.iter().map(StarterEntry::item)
}

/// The complete, explicit publisher namespace. A similarly prefixed remote or
/// user source does not acquire curated provenance.
pub fn is_starter_source(source_id: &str) -> bool {
    PUBLISHERS.iter().any(|(source, _)| *source == source_id)
}

/// Includes bundle membership, not only the upstream revision: adding a
/// license/reference at the same commit also makes an installed copy outdated.
pub fn starter_manifest_digest(id: &str) -> Option<String> {
    Some(manifest_digest(entry(id)?))
}

fn manifest_digest(entry: &StarterEntry) -> String {
    let mut digest = Sha256::new();
    let mut field = |value: &str| {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    };
    field("openide-curated-manifest-v1");
    field(&entry.repository);
    field(&entry.revision);
    field(&entry.directory);
    let mut files: Vec<_> = entry.files.iter().collect();
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for file in files {
        field(&file.path);
        field(&file.repository_path);
        field(&file.bytes.to_string());
        field(&file.sha256);
    }
    format!("{:x}", digest.finalize())
}

pub fn starter_details(id: &str) -> Option<StarterDetails> {
    let entry = entry(id)?;
    Some(StarterDetails {
        repository_url: format!("https://github.com/{}", entry.repository),
        revision: entry.revision.clone(),
        directory: entry.directory.clone(),
        license: entry.license.clone(),
        file_count: entry.files.len(),
        download_bytes: entry.files.iter().map(|file| file.bytes).sum(),
        compatibility: entry.compatibility.clone(),
        category: entry.category.clone(),
        files: entry
            .files
            .iter()
            .map(|file| (file.path.clone(), file.bytes))
            .collect(),
    })
}

/// The category of a curated entry, without building its details.
pub fn starter_category(id: &str) -> Option<&'static str> {
    entry(id).map(|entry| entry.category.as_str())
}

impl StarterEntry {
    pub(super) fn item(&self) -> MarketplaceItem {
        MarketplaceItem {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            license: Some(self.license.clone()),
            source_name: format!("{} · {}", self.publisher, &self.revision[..7]),
            source_kind: SourceKind::Github,
            installed_scope: None,
            enabled: false,
            source_available: true,
        }
    }
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && path.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn validate_entry(entry: &StarterEntry) -> io::Result<()> {
    let Some((source, name)) = entry.id.split_once(':') else {
        return Err(invalid("ID de catálogo inicial inválido"));
    };
    if !PUBLISHERS
        .iter()
        .any(|(prefix, repository)| *prefix == source && *repository == entry.repository)
        || name != entry.name
        || !super::valid_skill_name(&entry.name)
        || entry.revision.len() != 40
        || !entry.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !safe_relative(&entry.directory)
        || entry.files.is_empty()
        || entry.files.len() > MAX_BUNDLE_FILES
        || entry.description.is_empty()
        || entry.description.len() > 1024
        || entry.category.is_empty()
        || entry.category.len() > 80
        || entry.compatibility.is_empty()
        || entry.compatibility.len() > 2048
        || !matches!(entry.license.as_str(), "Apache-2.0" | "MIT")
    {
        return Err(invalid("Metadatos del catálogo inicial inválidos"));
    }
    let mut paths = HashSet::new();
    let mut bytes = 0u64;
    for file in &entry.files {
        bytes = bytes
            .checked_add(file.bytes)
            .ok_or_else(|| invalid("La skill supera el límite de tamaño"))?;
        let expected = format!("{}/{}", entry.directory, file.path);
        let license_notice = matches!(
            file.repository_path.as_str(),
            "README.md"
                | "LICENSE"
                | "LICENSE.md"
                | "LICENSE.txt"
                | "NOTICE"
                | "NOTICE.md"
                | "THIRD_PARTY_NOTICES.md"
        ) && file.path == format!("_upstream/{}", file.repository_path);
        if !safe_relative(&file.path)
            || !safe_relative(&file.repository_path)
            || (!license_notice && file.repository_path != expected)
            || !paths.insert(file.path.as_str())
            || file.bytes > MAX_BUNDLE_BYTES
            || bytes > MAX_BUNDLE_BYTES
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("Archivo del catálogo inicial inválido"));
        }
    }
    if !paths.contains("SKILL.md") {
        return Err(invalid("La skill no contiene SKILL.md"));
    }
    Ok(())
}

pub(super) fn download(entry: &StarterEntry, destination: &Path) -> io::Result<()> {
    download_from(entry, destination, RAW_ORIGIN, INSTALL_TIMEOUT)
}

fn download_from(
    entry: &StarterEntry,
    destination: &Path,
    origin: &str,
    timeout: Duration,
) -> io::Result<()> {
    validate_entry(entry)?;
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(FILE_TIMEOUT)
        .user_agent("OpenIDE-skill-marketplace")
        .build()
        .map_err(io::Error::other)?;
    fs::create_dir(destination)?;
    let deadline = Instant::now() + timeout;
    for file in &entry.files {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "La descarga de la skill superó el tiempo límite",
                )
            })?;
        let url = format!(
            "{}/{}/{}/{}",
            origin.trim_end_matches('/'),
            entry.repository,
            entry.revision,
            file.repository_path
        );
        let response = client
            .get(&url)
            .timeout(remaining.min(FILE_TIMEOUT))
            .send()
            .map_err(|error| {
                io::Error::other(format!(
                    "No se pudo descargar {}: {}",
                    file.path,
                    if error.is_timeout() {
                        "tiempo agotado"
                    } else {
                        "error de conexión"
                    }
                ))
            })?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "No se pudo descargar {} (HTTP {})",
                file.path,
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length != file.bytes)
        {
            return Err(invalid(format!(
                "El tamaño de {} no coincide con la revisión publicada",
                file.path
            )));
        }
        let mut bytes = Vec::with_capacity(file.bytes as usize);
        response.take(file.bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != file.bytes
            || format!("{:x}", Sha256::digest(&bytes)) != file.sha256
        {
            return Err(invalid(format!(
                "La integridad de {} no coincide con la revisión publicada",
                file.path
            )));
        }
        let path = destination.join(&file.path);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| invalid("Archivo sin carpeta padre"))?,
        )?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
    }
    let skill = fs::read_to_string(destination.join("SKILL.md"))?;
    let (name, _, _) = super::parse_frontmatter(&skill)
        .ok_or_else(|| invalid("SKILL.md descargado no es válido"))?;
    if name != entry.name {
        return Err(invalid(
            "El nombre descargado no coincide con la skill seleccionada",
        ));
    }
    // Records the exact installed revision independently of future app updates.
    let provenance = serde_json::json!({"repository":entry.repository,"revision":entry.revision,
        "directory":entry.directory,"license":entry.license,"item_id":entry.id,
        "manifest_digest":manifest_digest(entry)});
    fs::write(
        destination.join(".openide-source.json"),
        serde_json::to_vec_pretty(&provenance).map_err(io::Error::other)?,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "marketplace_catalog_tests.rs"]
mod tests;
