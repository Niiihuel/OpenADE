//! Bounded discovery of local `SKILL.md` documents from caller-authorized roots.
//!
//! Discovery never activates a skill. A caller can explicitly select catalog
//! entries and then activate them against a fresh, bounded discovery. This
//! catches renamed, changed, or unsafe documents before prompt construction.
//! Referenced files inside a skill are not read by this module.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_SKILL_DOCUMENT_BYTES: usize = 128 * 1024;
pub const MAX_SKILL_ROOTS: usize = 16;
pub const MAX_SKILL_ENTRIES_PER_ROOT: usize = 2048;
pub const MAX_DISCOVERED_SKILLS: usize = 256;
pub const MAX_ACTIVE_SKILLS: usize = 16;
pub const MAX_ACTIVE_SKILL_INSTRUCTIONS_BYTES: usize = 256 * 1024;
const MAX_SELECTION_PATH_BYTES: usize = 64 * 1024;
const MAX_FRONTMATTER_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    Project,
    User,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub origin: SkillOrigin,
}

impl SkillRoot {
    pub fn project(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            origin: SkillOrigin::Project,
        }
    }

    pub fn user(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            origin: SkillOrigin::User,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillProvenance {
    pub origin: SkillOrigin,
    pub root: PathBuf,
    pub skill_directory: PathBuf,
    pub document: PathBuf,
    pub sha256_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub instructions: String,
    pub provenance: SkillProvenance,
}

impl DiscoveredSkill {
    /// Persist this exact name, location, and document hash when the user
    /// chooses a skill. Activation will validate it against fresh discovery.
    pub fn selection(&self) -> SkillSelection {
        SkillSelection {
            name: self.name.clone(),
            provenance: self.provenance.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSelection {
    pub name: String,
    pub provenance: SkillProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivatedSkills {
    /// Model-visible instructions, in fresh catalog order.
    pub instructions: String,
    /// Hash of the rendered instructions and exact canonical provenance.
    pub sha256_hex: String,
    /// The selections actually included, in the same order as instructions.
    pub selected: Vec<SkillSelection>,
}

#[derive(Debug, Error)]
pub enum SkillActivationError {
    #[error(transparent)]
    Discovery(#[from] SkillDiscoveryError),
    #[error("too many selected skills (maximum {MAX_ACTIVE_SKILLS})")]
    TooManySelected,
    #[error("invalid stored skill selection")]
    InvalidSelection,
    #[error("skill {0:?} was selected more than once")]
    DuplicateSelection(String),
    #[error("selected skill {0:?} is no longer available")]
    SelectionUnavailable(String),
    #[error("selected skill {0:?} changed name, location, or document content")]
    SelectionChanged(String),
    #[error("activated skill instructions exceed {MAX_ACTIVE_SKILL_INSTRUCTIONS_BYTES} bytes")]
    InstructionsTooLarge,
}

#[derive(Debug, Error)]
pub enum SkillDiscoveryError {
    #[error("too many skill roots (maximum {MAX_SKILL_ROOTS})")]
    TooManyRoots,
    #[error("too many entries in skill root {root} (maximum {MAX_SKILL_ENTRIES_PER_ROOT})", root = .0.display())]
    TooManyEntries(PathBuf),
    #[error("too many discovered skills (maximum {MAX_DISCOVERED_SKILLS})")]
    TooManySkills,
    #[error("skill root is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error("skill path uses a symlink or reparse point: {0}")]
    LinkedPath(PathBuf),
    #[error("skill path {path} escapes authorized root {root}", path = .path.display(), root = .root.display())]
    EscapesRoot { root: PathBuf, path: PathBuf },
    #[error("skill document {document} escapes its directory {directory}", document = .document.display(), directory = .directory.display())]
    EscapesSkillDirectory {
        directory: PathBuf,
        document: PathBuf,
    },
    #[error("skill document exceeds {MAX_SKILL_DOCUMENT_BYTES} bytes: {path}", path = .0.display())]
    DocumentTooLarge(PathBuf),
    #[error("invalid skill document {path}: {reason}", path = .path.display())]
    InvalidDocument { path: PathBuf, reason: String },
    #[error("duplicate skill name {name:?} in {first} and {second}", first = .first.display(), second = .second.display())]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("skill I/O error at {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    metadata: FrontmatterMetadata,
}

#[derive(Default, Deserialize)]
struct FrontmatterMetadata {
    #[serde(rename = "short-description")]
    short_description: Option<String>,
}

/// Discover only direct child directories of the explicitly supplied roots.
/// Any invalid document or unsafe path fails the whole discovery; results are
/// sorted by origin, canonical root, and case-folded skill name.
pub fn discover_skills(roots: &[SkillRoot]) -> Result<Vec<DiscoveredSkill>, SkillDiscoveryError> {
    if roots.len() > MAX_SKILL_ROOTS {
        return Err(SkillDiscoveryError::TooManyRoots);
    }
    let mut canonical_roots = roots
        .iter()
        .map(|root| {
            let metadata =
                fs::symlink_metadata(&root.path).map_err(|source| io_error(&root.path, source))?;
            if is_link_or_reparse(&metadata) {
                return Err(SkillDiscoveryError::LinkedPath(root.path.clone()));
            }
            let canonical = canonicalize(&root.path)?;
            if !canonical.is_dir() {
                return Err(SkillDiscoveryError::RootNotDirectory(canonical));
            }
            Ok((root.origin, canonical))
        })
        .collect::<Result<Vec<_>, _>>()?;
    canonical_roots.sort();

    let mut discovered = Vec::new();
    let mut names = BTreeMap::<String, PathBuf>::new();
    for (origin, root) in canonical_roots {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&root).map_err(|source| io_error(&root, source))? {
            if entries.len() == MAX_SKILL_ENTRIES_PER_ROOT {
                return Err(SkillDiscoveryError::TooManyEntries(root));
            }
            entries.push(entry.map_err(|source| io_error(&root, source))?.path());
        }
        entries.sort();
        for entry in entries {
            let entry_metadata =
                fs::symlink_metadata(&entry).map_err(|source| io_error(&entry, source))?;
            if is_link_or_reparse(&entry_metadata) {
                return Err(SkillDiscoveryError::LinkedPath(entry));
            }
            let skill_directory = canonicalize(&entry)?;
            ensure_within(&root, &skill_directory)?;
            if !entry_metadata.is_dir() {
                continue;
            }
            let document_path = entry.join("SKILL.md");
            match fs::symlink_metadata(&document_path) {
                Ok(metadata) if is_link_or_reparse(&metadata) => {
                    return Err(SkillDiscoveryError::LinkedPath(document_path));
                }
                Ok(metadata) if !metadata.is_file() => {
                    return Err(SkillDiscoveryError::InvalidDocument {
                        path: document_path,
                        reason: "SKILL.md must be a regular file".to_owned(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&document_path, source)),
            }
            let document = canonicalize(&document_path)?;
            ensure_within(&root, &document)?;
            if !document.starts_with(&skill_directory) {
                return Err(SkillDiscoveryError::EscapesSkillDirectory {
                    directory: skill_directory,
                    document,
                });
            }
            let instructions = read_bounded(&document_path)?;
            let fallback_name = entry
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SkillDiscoveryError::InvalidDocument {
                    path: document.clone(),
                    reason: "skill directory name is not UTF-8".to_owned(),
                })?;
            let (name, description, short_description) =
                parse_frontmatter(&document, &instructions, fallback_name)?;
            let normalized_name = name.to_lowercase();
            if let Some(first) = names.insert(normalized_name, document.clone()) {
                return Err(SkillDiscoveryError::DuplicateName {
                    name,
                    first,
                    second: document,
                });
            }
            if discovered.len() >= MAX_DISCOVERED_SKILLS {
                return Err(SkillDiscoveryError::TooManySkills);
            }
            let sha256_hex = format!("{:x}", Sha256::digest(instructions.as_bytes()));
            discovered.push(DiscoveredSkill {
                name,
                description,
                short_description,
                instructions,
                provenance: SkillProvenance {
                    origin,
                    root: root.clone(),
                    skill_directory,
                    document,
                    sha256_hex,
                },
            });
        }
    }
    discovered.sort_by(|left, right| {
        left.provenance
            .origin
            .cmp(&right.provenance.origin)
            .then_with(|| left.provenance.root.cmp(&right.provenance.root))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(discovered)
}

/// Activate only exact selections supplied by the caller. Discovery is
/// repeated so a prior UI catalog cannot silently authorize changed files.
/// Any invalid catalog entry fails the entire activation, even when it was
/// not selected, because duplicate names make selection ambiguous.
pub fn activate_skills(
    roots: &[SkillRoot],
    selections: &[SkillSelection],
) -> Result<ActivatedSkills, SkillActivationError> {
    if selections.len() > MAX_ACTIVE_SKILLS {
        return Err(SkillActivationError::TooManySelected);
    }
    let mut selected_names = BTreeSet::new();
    for selection in selections {
        if selection.name.is_empty()
            || selection.name.len() > MAX_NAME_CHARS * 4
            || selection.name.chars().count() > MAX_NAME_CHARS
            || selection.provenance.sha256_hex.len() != 64
            || !selection
                .provenance
                .sha256_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || [
                &selection.provenance.root,
                &selection.provenance.skill_directory,
                &selection.provenance.document,
            ]
            .iter()
            .any(|path| path.as_os_str().as_encoded_bytes().len() > MAX_SELECTION_PATH_BYTES)
        {
            return Err(SkillActivationError::InvalidSelection);
        }
        if !selected_names.insert(selection.name.to_lowercase()) {
            return Err(SkillActivationError::DuplicateSelection(
                selection.name.clone(),
            ));
        }
    }

    let catalog = discover_skills(roots)?;
    for selection in selections {
        let current = catalog
            .iter()
            .find(|skill| skill.name.to_lowercase() == selection.name.to_lowercase())
            .ok_or_else(|| SkillActivationError::SelectionUnavailable(selection.name.clone()))?;
        if current.selection() != *selection {
            return Err(SkillActivationError::SelectionChanged(
                selection.name.clone(),
            ));
        }
    }

    let mut instructions = String::new();
    let mut selected = Vec::with_capacity(selections.len());
    for skill in catalog
        .iter()
        .filter(|skill| selected_names.contains(&skill.name.to_lowercase()))
    {
        let origin = match skill.provenance.origin {
            SkillOrigin::Project => "project",
            SkillOrigin::User => "user",
        };
        let block = format!(
            "<openide_skill name=\"{}\" origin=\"{origin}\" sha256=\"{}\">\n{}\n</openide_skill>\n",
            escape_xml_attribute(&skill.name),
            skill.provenance.sha256_hex,
            skill.instructions
        );
        let next_len = instructions
            .len()
            .checked_add(block.len())
            .ok_or(SkillActivationError::InstructionsTooLarge)?;
        if next_len > MAX_ACTIVE_SKILL_INSTRUCTIONS_BYTES {
            return Err(SkillActivationError::InstructionsTooLarge);
        }
        instructions.push_str(&block);
        selected.push(skill.selection());
    }

    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"openide-activated-skills-v1");
    hash_field(&mut hasher, instructions.as_bytes());
    for selection in &selected {
        hash_field(&mut hasher, selection.name.as_bytes());
        let origin = match selection.provenance.origin {
            SkillOrigin::Project => b"project".as_slice(),
            SkillOrigin::User => b"user".as_slice(),
        };
        hash_field(&mut hasher, origin);
        hash_field(
            &mut hasher,
            selection.provenance.root.as_os_str().as_encoded_bytes(),
        );
        hash_field(
            &mut hasher,
            selection
                .provenance
                .skill_directory
                .as_os_str()
                .as_encoded_bytes(),
        );
        hash_field(
            &mut hasher,
            selection.provenance.document.as_os_str().as_encoded_bytes(),
        );
        hash_field(&mut hasher, selection.provenance.sha256_hex.as_bytes());
    }
    Ok(ActivatedSkills {
        instructions,
        sha256_hex: format!("{:x}", hasher.finalize()),
        selected,
    })
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn escape_xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}

fn canonicalize(path: &Path) -> Result<PathBuf, SkillDiscoveryError> {
    fs::canonicalize(path).map_err(|source| io_error(path, source))
}

fn ensure_within(root: &Path, path: &Path) -> Result<(), SkillDiscoveryError> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(SkillDiscoveryError::EscapesRoot {
            root: root.to_path_buf(),
            path: path.to_path_buf(),
        })
    }
}

fn io_error(path: &Path, source: io::Error) -> SkillDiscoveryError {
    SkillDiscoveryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn read_bounded(path: &Path) -> Result<String, SkillDiscoveryError> {
    let file = open_no_follow(path).map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if is_link_or_reparse(&metadata) {
        return Err(SkillDiscoveryError::LinkedPath(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(invalid(path, "SKILL.md must be a regular file"));
    }
    if metadata.len() > MAX_SKILL_DOCUMENT_BYTES as u64 {
        return Err(SkillDiscoveryError::DocumentTooLarge(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.take((MAX_SKILL_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() > MAX_SKILL_DOCUMENT_BYTES {
        return Err(SkillDiscoveryError::DocumentTooLarge(path.to_path_buf()));
    }
    String::from_utf8(bytes).map_err(|_| SkillDiscoveryError::InvalidDocument {
        path: path.to_path_buf(),
        reason: "SKILL.md must be UTF-8".to_owned(),
    })
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn is_link_or_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn parse_frontmatter(
    path: &Path,
    instructions: &str,
    fallback_name: &str,
) -> Result<(String, String, Option<String>), SkillDiscoveryError> {
    let mut lines = instructions.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(invalid(path, "missing YAML frontmatter"));
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if yaml.len() + line.len() + 1 > MAX_FRONTMATTER_BYTES {
            return Err(invalid(path, "frontmatter is too large"));
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    if !closed {
        return Err(invalid(path, "frontmatter is not closed"));
    }
    let parsed: Frontmatter = serde_yaml::from_str(&yaml)
        .map_err(|error| invalid(path, format!("invalid YAML: {error}")))?;
    let name = clean_single_line(parsed.name.as_deref().unwrap_or(fallback_name));
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(invalid(path, "name must contain 1–64 characters"));
    }
    let description = clean_single_line(parsed.description.as_deref().unwrap_or_default());
    if description.is_empty()
        || description.chars().count() > MAX_DESCRIPTION_CHARS
        || description.chars().any(char::is_control)
    {
        return Err(invalid(path, "description must contain 1–4096 characters"));
    }
    let short_description = parsed
        .metadata
        .short_description
        .as_deref()
        .map(clean_single_line)
        .filter(|value| !value.is_empty());
    Ok((name, description, short_description))
}

fn clean_single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn invalid(path: &Path, reason: impl Into<String>) -> SkillDiscoveryError {
    SkillDiscoveryError::InvalidDocument {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(root: &Path, directory: &str, frontmatter: &str) -> PathBuf {
        let skill_dir = root.join(directory);
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        fs::write(
            &path,
            format!("---\n{frontmatter}\n---\n\n# Instructions\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn discovers_explicit_roots_with_stable_order_and_provenance() {
        let project = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        write_skill(project.path(), "zeta", "description: Last skill");
        write_skill(
            project.path(),
            "alpha",
            "name: Alpha\ndescription: >-\n  First line\n  second line\nmetadata:\n  short-description: Short",
        );
        write_skill(user.path(), "beta", "description: User skill");
        let roots = [
            SkillRoot::user(user.path()),
            SkillRoot::project(project.path()),
        ];
        let skills = discover_skills(&roots).unwrap();
        assert_eq!(
            skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "zeta", "beta"]
        );
        assert_eq!(skills[0].description, "First line second line");
        assert_eq!(skills[0].short_description.as_deref(), Some("Short"));
        assert_eq!(skills[0].provenance.origin, SkillOrigin::Project);
        assert_eq!(
            skills[0].provenance.root,
            fs::canonicalize(project.path()).unwrap()
        );
        assert_eq!(skills[0].provenance.sha256_hex.len(), 64);
        assert_eq!(discover_skills(&roots).unwrap(), skills);
    }

    #[test]
    fn duplicate_names_across_roots_fail_even_when_case_differs() {
        let project = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        write_skill(project.path(), "one", "name: Review\ndescription: One");
        write_skill(user.path(), "two", "name: review\ndescription: Two");
        assert!(matches!(
            discover_skills(&[
                SkillRoot::user(user.path()),
                SkillRoot::project(project.path())
            ]),
            Err(SkillDiscoveryError::DuplicateName { .. })
        ));
    }

    #[test]
    fn activation_is_explicit_deterministic_and_binds_provenance() {
        let project = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        write_skill(project.path(), "alpha", "name: Alpha\ndescription: First");
        write_skill(
            project.path(),
            "other",
            "name: Other\ndescription: Not selected",
        );
        write_skill(user.path(), "beta", "name: Beta\ndescription: Second");
        let roots = [
            SkillRoot::user(user.path()),
            SkillRoot::project(project.path()),
        ];
        let catalog = discover_skills(&roots).unwrap();
        let alpha = catalog[0].selection();
        let beta = catalog[2].selection();
        let active = activate_skills(&roots, &[beta.clone(), alpha.clone()]).unwrap();
        assert_eq!(active.selected, [alpha.clone(), beta.clone()]);
        assert!(active.instructions.contains("name=\"Alpha\""));
        assert!(active.instructions.contains("name=\"Beta\""));
        assert!(!active.instructions.contains("Not selected"));
        assert_eq!(active.sha256_hex.len(), 64);
        assert_eq!(
            active,
            activate_skills(&roots, &[alpha.clone(), beta.clone()]).unwrap()
        );
        let only_alpha = activate_skills(&roots, &[alpha]).unwrap();
        assert_ne!(active.sha256_hex, only_alpha.sha256_hex);
        let empty = activate_skills(&roots, &[]).unwrap();
        assert!(empty.instructions.is_empty());
        assert!(empty.selected.is_empty());

        let serialized = serde_json::to_string(&beta).unwrap();
        assert_eq!(
            serde_json::from_str::<SkillSelection>(&serialized).unwrap(),
            beta
        );
    }

    #[test]
    fn activation_rejects_changed_content_name_and_location() {
        let root = TempDir::new().unwrap();
        let path = write_skill(
            root.path(),
            "review",
            "name: Review\ndescription: Review code",
        );
        let roots = [SkillRoot::project(root.path())];
        let selected = discover_skills(&roots).unwrap()[0].selection();

        fs::write(
            &path,
            "---\nname: Review\ndescription: Review code\n---\n\nDifferent instructions\n",
        )
        .unwrap();
        assert!(matches!(
            activate_skills(&roots, std::slice::from_ref(&selected)),
            Err(SkillActivationError::SelectionChanged(_))
        ));

        fs::write(
            &path,
            "---\nname: Renamed\ndescription: Review code\n---\n\n# Instructions\n",
        )
        .unwrap();
        assert!(matches!(
            activate_skills(&roots, std::slice::from_ref(&selected)),
            Err(SkillActivationError::SelectionUnavailable(_))
        ));

        fs::rename(root.path().join("review"), root.path().join("moved")).unwrap();
        fs::write(
            root.path().join("moved/SKILL.md"),
            "---\nname: Review\ndescription: Review code\n---\n\n# Instructions\n",
        )
        .unwrap();
        assert!(matches!(
            activate_skills(&roots, &[selected]),
            Err(SkillActivationError::SelectionChanged(_))
        ));
    }

    #[test]
    fn activation_rejects_duplicate_unknown_and_oversized_selections() {
        let root = TempDir::new().unwrap();
        write_skill(root.path(), "one", "name: One\ndescription: One");
        let roots = [SkillRoot::project(root.path())];
        let selected = discover_skills(&roots).unwrap()[0].selection();
        assert!(matches!(
            activate_skills(&roots, &[selected.clone(), selected.clone()]),
            Err(SkillActivationError::DuplicateSelection(_))
        ));
        let mut unknown = selected.clone();
        unknown.name = "Absent".into();
        assert!(matches!(
            activate_skills(&roots, &[unknown]),
            Err(SkillActivationError::SelectionUnavailable(_))
        ));
        let mut invalid = selected.clone();
        invalid.name = "x".repeat(MAX_NAME_CHARS + 1);
        assert!(matches!(
            activate_skills(&roots, &[invalid]),
            Err(SkillActivationError::InvalidSelection)
        ));
        let mut invalid = selected.clone();
        invalid.provenance.sha256_hex = "not-a-digest".into();
        assert!(matches!(
            activate_skills(&roots, &[invalid]),
            Err(SkillActivationError::InvalidSelection)
        ));
        assert!(matches!(
            activate_skills(&roots, &vec![selected; MAX_ACTIVE_SKILLS + 1]),
            Err(SkillActivationError::TooManySelected)
        ));
    }

    #[test]
    fn activation_rejects_total_instruction_budget_without_partial_output() {
        let root = TempDir::new().unwrap();
        for index in 0..3 {
            let path = write_skill(
                root.path(),
                &format!("skill-{index}"),
                &format!("name: Skill{index}\ndescription: Test"),
            );
            let mut body = fs::read_to_string(&path).unwrap();
            body.push_str(&"x".repeat(100 * 1024));
            fs::write(path, body).unwrap();
        }
        let roots = [SkillRoot::project(root.path())];
        let selections = discover_skills(&roots)
            .unwrap()
            .iter()
            .map(DiscoveredSkill::selection)
            .collect::<Vec<_>>();
        assert!(matches!(
            activate_skills(&roots, &selections),
            Err(SkillActivationError::InstructionsTooLarge)
        ));
    }

    #[test]
    fn activation_escapes_metadata_in_prompt_wrapper() {
        let root = TempDir::new().unwrap();
        write_skill(
            root.path(),
            "odd",
            "name: 'Review \"<&>'\ndescription: Escaping test",
        );
        let roots = [SkillRoot::project(root.path())];
        let selected = discover_skills(&roots).unwrap()[0].selection();
        let active = activate_skills(&roots, &[selected]).unwrap();
        assert!(active
            .instructions
            .contains("name=\"Review &quot;&lt;&amp;&gt;\""));
    }

    #[test]
    fn activation_digest_changes_with_provenance_even_when_prompt_text_matches() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        for root in [&first, &second] {
            write_skill(
                root.path(),
                "same",
                "name: Same\ndescription: Same document",
            );
        }
        let first_roots = [SkillRoot::project(first.path())];
        let second_roots = [SkillRoot::project(second.path())];
        let first_selected = discover_skills(&first_roots).unwrap()[0].selection();
        let second_selected = discover_skills(&second_roots).unwrap()[0].selection();
        let first_active = activate_skills(&first_roots, &[first_selected]).unwrap();
        let second_active = activate_skills(&second_roots, &[second_selected]).unwrap();
        assert_eq!(first_active.instructions, second_active.instructions);
        assert_ne!(first_active.sha256_hex, second_active.sha256_hex);
    }

    #[test]
    fn rejects_oversized_and_invalid_documents_without_partial_catalog() {
        let root = TempDir::new().unwrap();
        write_skill(root.path(), "valid", "description: Valid");
        let huge = write_skill(root.path(), "huge", "description: Huge");
        fs::write(&huge, vec![b'x'; MAX_SKILL_DOCUMENT_BYTES + 1]).unwrap();
        assert!(matches!(
            discover_skills(&[SkillRoot::project(root.path())]),
            Err(SkillDiscoveryError::DocumentTooLarge(_))
        ));
        fs::write(&huge, "---\nname: Bad\n---\n").unwrap();
        assert!(matches!(
            discover_skills(&[SkillRoot::project(root.path())]),
            Err(SkillDiscoveryError::InvalidDocument { .. })
        ));
    }

    #[test]
    fn scans_direct_children_only() {
        let root = TempDir::new().unwrap();
        write_skill(root.path(), "outer/nested", "description: Not direct");
        assert!(discover_skills(&[SkillRoot::project(root.path())])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn containment_checks_path_components_instead_of_string_prefixes() {
        let root = TempDir::new().unwrap();
        let sibling = root.path().with_extension("other");
        assert!(matches!(
            ensure_within(root.path(), &sibling),
            Err(SkillDiscoveryError::EscapesRoot { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_skill_directory_outside_root() {
        use std::os::unix::fs::symlink;
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write_skill(outside.path(), "external", "description: Outside");
        symlink(outside.path().join("external"), root.path().join("escape")).unwrap();
        assert!(matches!(
            discover_skills(&[SkillRoot::project(root.path())]),
            Err(SkillDiscoveryError::LinkedPath(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_symlinked_skill_file_outside_root_when_supported() {
        use std::os::windows::fs::symlink_file;
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external = write_skill(outside.path(), "external", "description: Outside");
        fs::create_dir(root.path().join("escape")).unwrap();
        let result = symlink_file(external, root.path().join("escape/SKILL.md"));
        if result.as_ref().is_err_and(|error| {
            error.kind() == io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
        }) {
            return;
        }
        result.unwrap();
        assert!(matches!(
            discover_skills(&[SkillRoot::project(root.path())]),
            Err(SkillDiscoveryError::LinkedPath(_))
        ));
    }
}
