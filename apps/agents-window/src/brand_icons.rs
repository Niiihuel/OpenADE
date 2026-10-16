//! Complete, offline SVGL artwork and conservative brand suggestions.
//!
//! Metadata is evidence for presentation, never for permissions or execution.
//! No endpoint, command, package name or search query is sent to a service.
use base64::Engine;
use openide_ui::{Color, Element, ICON_SIZE, Icon, RasterImage, RasterImageFormat};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, OnceLock},
};

#[path = "brand_icons_catalog.rs"]
mod catalog_data;

#[derive(Clone, Copy)]
enum BrandArtwork {
    Svg(&'static str),
    Png(&'static [u8]),
}

pub(crate) struct BrandEntry {
    /// Derived from the upstream artwork routes, independent of array order/title.
    pub id: &'static str,
    pub title: &'static str,
    pub website: &'static str,
    pub aliases: &'static [&'static str],
    pub categories: &'static [&'static str],
    light: BrandArtwork,
    dark: BrandArtwork,
}

impl BrandEntry {
    fn artwork(&self, dark: bool) -> BrandArtwork {
        if dark { self.dark } else { self.light }
    }

    /// A native vector icon, when the upstream artwork is entirely vector.
    /// Prefer `element`: ordinary button icon slots tint their artwork.
    pub fn icon(&self, dark: bool) -> Option<Icon> {
        match self.artwork(dark) {
            BrandArtwork::Svg(svg) => Some(Icon::Custom(svg)),
            BrandArtwork::Png(_) => None,
        }
    }

    /// Preserve the original colors, with Contain in the normal 16px icon box.
    /// The five image/text SVGs unsupported by the vector-only native renderer
    /// were flattened at vendoring time; this never reads or rasterizes a file.
    pub fn element(&self, dark: bool) -> Element {
        match self.artwork(dark) {
            BrandArtwork::Svg(svg) => {
                Element::icon(Icon::Custom(svg)).foreground(Color::rgb(255, 255, 255))
            }
            BrandArtwork::Png(bytes) => {
                // A root is placed in the viewport regardless of its size;
                // keep the raster child fixed, just as native icons paint.
                Element::raster_image(png(bytes, self.title))
                    .size(Some(ICON_SIZE), Some(ICON_SIZE))
                    .centered()
                    .size(Some(ICON_SIZE), Some(ICON_SIZE))
            }
        }
    }
}

fn png(bytes: &'static [u8], title: &str) -> RasterImage {
    // At most ten tiny static variants, not an unbounded runtime SVG interner.
    static CACHE: OnceLock<Mutex<HashMap<&'static [u8], RasterImage>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache
        .entry(bytes)
        .or_insert_with(|| {
            RasterImage::new(
                format!("{:x}", Sha256::digest(bytes)),
                RasterImageFormat::Png,
                bytes.len() as u32,
                Arc::from(base64::engine::general_purpose::STANDARD.encode(bytes)),
                title,
            )
            .expect("vendored PNGs have validated size and metadata")
        })
        .clone()
}

pub(crate) fn catalog() -> &'static [BrandEntry] {
    catalog_data::ENTRIES
}

/// Only the catalog's immutable artwork uses white as a neutral color
/// multiplier. Ordinary native/custom icons still follow the theme's ink.
pub(crate) fn is_brand_icon(icon: &Icon) -> bool {
    let Icon::Custom(svg) = icon else {
        return false;
    };
    static SOURCES: OnceLock<BTreeSet<(usize, usize)>> = OnceLock::new();
    SOURCES
        .get_or_init(|| {
            catalog()
                .iter()
                .flat_map(|entry| [entry.light, entry.dark])
                .filter_map(|artwork| match artwork {
                    BrandArtwork::Svg(source) => Some((source.as_ptr() as usize, source.len())),
                    BrandArtwork::Png(_) => None,
                })
                .collect()
        })
        .contains(&(svg.as_ptr() as usize, svg.len()))
}

pub(crate) fn get(id: &str) -> Option<&'static BrandEntry> {
    catalog().iter().find(|entry| entry.id == id)
}

/// Search the local names, aliases, categories and official website. The
/// Names rank before category/site matches; ties keep source order.
pub(crate) fn search(query: &str, limit: usize) -> Vec<&'static BrandEntry> {
    let query = query
        .trim()
        .chars()
        .take(128)
        .collect::<String>()
        .to_lowercase();
    let mut found = catalog()
        .iter()
        .filter_map(|entry| {
            let rank = entry
                .aliases
                .iter()
                .copied()
                .chain(std::iter::once(entry.title))
                .filter_map(|name| {
                    let name = name.to_lowercase();
                    if name == query {
                        Some(0)
                    } else if name.starts_with(&query) {
                        Some(1)
                    } else if name.contains(&query) {
                        Some(2)
                    } else {
                        None
                    }
                })
                .min()
                .or_else(|| {
                    (entry
                        .categories
                        .iter()
                        .any(|value| value.to_lowercase().contains(&query))
                        || entry.website.to_lowercase().contains(&query))
                    .then_some(3)
                })?;
            Some((rank, entry))
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|(rank, _)| *rank);
    found
        .into_iter()
        .take(limit.min(128))
        .map(|(_, entry)| entry)
        .collect()
}

#[derive(Default)]
pub(crate) struct BrandHints<'a> {
    pub display_name: &'a str,
    pub package_names: &'a [&'a str],
    pub executable: Option<&'a str>,
    pub website_urls: &'a [&'a str],
    pub endpoint_url: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrandEvidence {
    EndpointHost,
    Website,
    PackageName,
    Executable,
    DisplayName,
}

impl BrandEvidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::EndpointHost => "Coincide el host del servidor",
            Self::Website => "Coincide el sitio de los metadatos",
            Self::PackageName => "Coincide el nombre del paquete",
            Self::Executable => "Coincide el nombre del ejecutable",
            Self::DisplayName => "Coincide el nombre mostrado",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BrandCandidate {
    pub id: &'static str,
    pub reason: BrandEvidence,
}

fn website(value: &str) -> Option<Url> {
    if value.len() > 2048 {
        return None;
    }
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

#[cfg(test)]
fn same_website(left: &str, right: &str) -> bool {
    let (Some(left), Some(right)) = (website(left), website(right)) else {
        return false;
    };
    same_url(&left, &right)
}

fn same_url(left: &Url, right: &Url) -> bool {
    left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
        && left.path().trim_end_matches('/').trim_end_matches(".git")
            == right.path().trim_end_matches('/').trim_end_matches(".git")
}

fn sites() -> &'static HashMap<&'static str, Url> {
    static SITES: OnceLock<HashMap<&'static str, Url>> = OnceLock::new();
    SITES.get_or_init(|| {
        catalog()
            .iter()
            .filter_map(|entry| website(entry.website).map(|url| (entry.id, url)))
            .collect()
    })
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn name_matches(entry: &BrandEntry, text: &str) -> bool {
    if text.len() > 512 {
        return false;
    }
    let words = text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    entry.aliases.iter().any(|alias| {
        let alias = normalized(alias);
        !alias.is_empty()
            && (alias == normalized(text) || words.iter().any(|word| normalized(word) == alias))
    })
}

fn executable(value: &str) -> Option<&str> {
    let value = value.rsplit(['/', '\\']).next()?.trim_end_matches(".exe");
    // Runtime/launcher names describe how a process starts, not which MCP
    // integration it provides. This is command grammar, not a company map.
    let lower = value.to_ascii_lowercase();
    (!matches!(
        lower.as_str(),
        "node"
            | "nodejs"
            | "python"
            | "python3"
            | "uv"
            | "uvx"
            | "npx"
            | "npm"
            | "pnpm"
            | "bun"
            | "deno"
            | "docker"
            | "podman"
            | "sh"
            | "bash"
            | "zsh"
            | "cmd"
            | "powershell"
            | "pwsh"
    ))
    .then_some(value)
}

/// Suggestions keep their evidence visible. Name/package/executable matches
/// never become an automatic claim; URLs use exact hosts, never substring or
/// suffix matches. Metadata websites also require their exact repository path.
pub(crate) fn candidates(hints: &BrandHints<'_>) -> Vec<BrandCandidate> {
    let endpoint = hints.endpoint_url.and_then(website);
    let executable = hints.executable.and_then(executable);
    let metadata = hints
        .website_urls
        .iter()
        .take(16)
        .filter_map(|url| website(url))
        .collect::<Vec<_>>();
    let mut found = catalog()
        .iter()
        .filter_map(|entry| {
            let reason = if endpoint.as_ref().is_some_and(|endpoint| {
                sites()
                    .get(entry.id)
                    .is_some_and(|known| endpoint.host_str() == known.host_str())
            }) {
                BrandEvidence::EndpointHost
            } else if sites()
                .get(entry.id)
                .is_some_and(|known| metadata.iter().any(|url| same_url(known, url)))
            {
                BrandEvidence::Website
            } else if hints
                .package_names
                .iter()
                .take(16)
                .any(|name| name_matches(entry, name))
            {
                BrandEvidence::PackageName
            } else if executable.is_some_and(|name| name_matches(entry, name)) {
                BrandEvidence::Executable
            } else if name_matches(entry, hints.display_name) {
                BrandEvidence::DisplayName
            } else {
                return None;
            };
            Some(BrandCandidate {
                id: entry.id,
                reason,
            })
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|candidate| candidate.reason as u8);
    found
}

/// Automatic artwork is a presentation hint, not verified brand identity.
/// A unique exact endpoint host is sufficient. Local metadata instead needs
/// one exact official website and an independent package/executable match.
/// Shared websites, ambiguous names or conflicting evidence stay generic.
pub(crate) fn detect(hints: &BrandHints<'_>) -> Option<&'static BrandEntry> {
    let metadata = hints
        .website_urls
        .iter()
        .take(16)
        .filter_map(|url| website(url))
        .collect::<Vec<_>>();
    let exact_sites = catalog()
        .iter()
        .filter(|entry| {
            sites()
                .get(entry.id)
                .is_some_and(|known| metadata.iter().any(|url| same_url(known, url)))
        })
        .collect::<Vec<_>>();
    if let Some(endpoint) = hints.endpoint_url {
        let endpoint = website(endpoint)?;
        // Ordinary rows call this while painting: these catalog URLs are cached.
        let mut hosts = catalog().iter().filter(|entry| {
            sites()
                .get(entry.id)
                .is_some_and(|known| endpoint.host_str() == known.host_str())
        });
        let found = hosts.next()?;
        if hosts.next().is_some() || exact_sites.iter().any(|entry| entry.id != found.id) {
            return None;
        }
        corroborated_name(found, hints)?;
        return Some(found);
    }
    let [found] = exact_sites.as_slice() else {
        return None;
    };
    corroborated_name(found, hints)?.then_some(*found)
}

/// `None` means conflicting/ambiguous evidence; `Some(false)` means no
/// independent name evidence. Display names never provide corroboration.
fn corroborated_name(found: &BrandEntry, hints: &BrandHints<'_>) -> Option<bool> {
    let mut corroborated = false;
    for name in hints
        .package_names
        .iter()
        .copied()
        .take(16)
        .chain(hints.executable.and_then(executable))
    {
        let mut matching = catalog().iter().filter(|entry| name_matches(entry, name));
        if let Some(named) = matching.next() {
            if named.id != found.id || matching.next().is_some() {
                return None;
            }
            corroborated = true;
        }
    }
    // A manually chosen server ID/display label cannot corroborate a package.
    // It can still reveal contradictory/ambiguous evidence worth suppressing.
    if catalog()
        .iter()
        .any(|entry| entry.id != found.id && name_matches(entry, hints.display_name))
    {
        return None;
    }
    Some(corroborated)
}

#[cfg(test)]
#[path = "brand_icons_tests.rs"]
mod tests;
