//! Explicit, workspace-scoped selection of local SKILL.md instructions.
//!
//! The app data directory and the selected workspace determine the only two
//! discoverable roots. Discovery is read-only; only a saved exact selection can
//! be activated for a native turn.

use crate::agent_tool_settings::replace_atomically;
use openide_harness::{
    activate_skills, discover_skills, ActivatedSkills, DiscoveredSkill, SkillOrigin, SkillRoot,
    SkillSelection, MAX_ACTIVE_SKILLS,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const FILE_NAME: &str = "native-local-skills.json";
const VERSION: u32 = 1;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_WORKSPACES: usize = 128;
const MAX_PATH_BYTES: usize = 64 * 1024;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct NativeSkillsSettings {
    pub workspace: PathBuf,
    pub selections: Vec<SkillSelection>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeSkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub origin: SkillOrigin,
    pub document: PathBuf,
    pub selection: SkillSelection,
    /// Complete SKILL.md text from the same bounded read as its observed hash.
    /// It is retained for review in RAM and is never saved in the selection file.
    pub instructions: String,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeSkillsClearSnapshot {
    pub workspace: PathBuf,
    pub selections: Vec<SkillSelection>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeSkillsUiState {
    pub workspace: String,
    pub user_root: String,
    pub project_root: String,
    pub available: Vec<NativeSkillCatalogEntry>,
    pub selected: Vec<SkillSelection>,
    pub load_error: Option<String>,
    pub catalog_error: Option<String>,
    pub activation_error: Option<String>,
    pub reset_confirm_open: bool,
    /// Exact catalog observation being reviewed; never an activation snapshot.
    pub preview: Option<SkillSelection>,
    pub preview_return_focus: Option<String>,
    pub clear_workspace_confirm_open: bool,
    pub clear_workspace_snapshot: Option<NativeSkillsClearSnapshot>,
}

impl NativeSkillsUiState {
    pub fn from_settings(settings: &NativeSkillsSettings, data_root: &Path) -> Self {
        let mut state = Self {
            workspace: settings.workspace.display().to_string(),
            user_root: data_root.join("local-skills").display().to_string(),
            project_root: settings
                .workspace
                .join(".agents")
                .join("skills")
                .display()
                .to_string(),
            selected: settings.selections.clone(),
            ..Self::default()
        };
        state.refresh(settings, data_root);
        state
    }

    pub fn with_load_error(workspace: &Path, data_root: &Path, error: String) -> Self {
        Self {
            workspace: workspace.display().to_string(),
            user_root: data_root.join("local-skills").display().to_string(),
            project_root: workspace
                .join(".agents")
                .join("skills")
                .display()
                .to_string(),
            load_error: Some(error),
            ..Self::default()
        }
    }

    pub fn refresh(&mut self, settings: &NativeSkillsSettings, data_root: &Path) {
        self.preview = None;
        self.preview_return_focus = None;
        self.clear_workspace_confirm_open = false;
        self.clear_workspace_snapshot = None;
        if self.load_error.is_some() {
            return;
        }
        self.available.clear();
        self.catalog_error = None;
        self.activation_error = None;
        match settings.discover(data_root) {
            Ok(skills) => {
                self.available = skills
                    .into_iter()
                    .map(|skill| {
                        let selection = skill.selection();
                        NativeSkillCatalogEntry {
                            name: skill.name,
                            description: skill.short_description.unwrap_or(skill.description),
                            origin: skill.provenance.origin,
                            document: skill.provenance.document,
                            selection,
                            instructions: skill.instructions,
                        }
                    })
                    .collect();
                if let Err(error) = settings.activate(data_root) {
                    self.activation_error = Some(error);
                }
            }
            Err(error) => self.catalog_error = Some(error),
        }
    }

    /// Return a proposed exact selection. The host persists it, then rebuilds
    /// this view from the saved settings. A changed skill needs this click to
    /// accept its new content hash.
    pub fn toggle(&self, index: usize) -> Result<Vec<SkillSelection>, String> {
        self.ensure_editable()?;
        let entry = self
            .available
            .get(index)
            .ok_or_else(|| "La skill seleccionada ya no existe en el catálogo".to_owned())?;
        let mut next = self.selected.clone();
        match next
            .iter()
            .position(|old| old.name.to_lowercase() == entry.name.to_lowercase())
        {
            Some(position) if next[position] == entry.selection => {
                next.remove(position);
            }
            Some(position) => next[position] = entry.selection.clone(),
            None if next.len() >= MAX_ACTIVE_SKILLS => {
                return Err(format!(
                    "Se pueden seleccionar hasta {MAX_ACTIVE_SKILLS} skills"
                ));
            }
            None => next.push(entry.selection.clone()),
        }
        Ok(next)
    }

    pub fn remove(&self, index: usize) -> Result<Vec<SkillSelection>, String> {
        self.ensure_editable()?;
        let mut next = self.selected.clone();
        if index >= next.len() {
            return Err("La selección ya no existe".into());
        }
        next.remove(index);
        Ok(next)
    }

    pub fn open_preview(&mut self, index: usize) -> Result<(), String> {
        self.ensure_editable()?;
        let entry = self
            .available
            .get(index)
            .ok_or_else(|| "El documento ya no está disponible para revisar".to_owned())?;
        self.preview = Some(entry.selection.clone());
        self.preview_return_focus = Some(format!("native_skill_preview:{index}"));
        Ok(())
    }

    /// A stale selection reviews the freshly observed document of the same
    /// name, showing both hashes. Missing documents have nothing to preview.
    pub fn open_selected_preview(&mut self, index: usize) -> Result<(), String> {
        self.ensure_editable()?;
        let selection = self
            .selected
            .get(index)
            .ok_or_else(|| "La selección ya no existe".to_owned())?;
        let available_index = self
            .available
            .iter()
            .position(|entry| entry.name.eq_ignore_ascii_case(&selection.name))
            .ok_or_else(|| {
                "El documento seleccionado ya no está disponible; puedes quitar su selección"
                    .to_owned()
            })?;
        self.open_preview(available_index)?;
        self.preview_return_focus = Some(format!("native_skill_preview_selected:{index}"));
        Ok(())
    }

    pub fn close_preview(&mut self) {
        self.preview = None;
        self.preview_return_focus = None;
    }

    pub fn preview_entry(&self) -> Option<&NativeSkillCatalogEntry> {
        let preview = self.preview.as_ref()?;
        self.available
            .iter()
            .find(|entry| entry.selection == *preview)
    }

    /// Use only the exact document currently displayed in the preview. Saving
    /// this proposal still invokes fresh activation and rejects a disk change.
    pub fn toggle_preview(&self) -> Result<Vec<SkillSelection>, String> {
        self.ensure_editable()?;
        let preview = self
            .preview
            .as_ref()
            .ok_or_else(|| "Abre el documento antes de cambiar su selección".to_owned())?;
        let index = self
            .available
            .iter()
            .position(|entry| entry.selection == *preview)
            .ok_or_else(|| {
                "El documento observado cambió; actualiza y vuelve a revisarlo".to_owned()
            })?;
        self.toggle(index)
    }

    fn ensure_editable(&self) -> Result<(), String> {
        if self.load_error.is_some() {
            return Err("La configuración de skills no se pudo cargar".into());
        }
        if self.catalog_error.is_some() {
            return Err("El catálogo de skills no se pudo validar".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    workspaces: Vec<WorkspaceSelections>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSelections {
    workspace: PathBuf,
    selections: Vec<SkillSelection>,
}

impl NativeSkillsSettings {
    pub fn load(data_root: &Path, workspace: &Path) -> Result<Self, String> {
        let workspace = canonical_workspace(workspace)?;
        let document = load_document(data_root)?;
        let selections = document
            .workspaces
            .into_iter()
            .find(|entry| workspace_key(&entry.workspace) == workspace_key(&workspace))
            .map_or_else(Vec::new, |entry| entry.selections);
        Ok(Self {
            workspace,
            selections,
        })
    }

    pub fn discover(&self, data_root: &Path) -> Result<Vec<DiscoveredSkill>, String> {
        let roots = roots(data_root, &self.workspace)?;
        discover_skills(&roots).map_err(|error| format!("Catálogo de skills locales: {error}"))
    }

    pub fn activate(&self, data_root: &Path) -> Result<ActivatedSkills, String> {
        let roots = roots(data_root, &self.workspace)?;
        activate_skills(&roots, &self.selections)
            .map_err(|error| format!("Selección de skills locales: {error}"))
    }

    pub fn with_selections(&self, selections: Vec<SkillSelection>) -> Self {
        Self {
            workspace: self.workspace.clone(),
            selections,
        }
    }

    pub fn save(&self, data_root: &Path) -> Result<(), String> {
        self.activate(data_root)?;
        let mut document = load_document(data_root)?;
        if let Some(entry) = document
            .workspaces
            .iter_mut()
            .find(|entry| workspace_key(&entry.workspace) == workspace_key(&self.workspace))
        {
            entry.selections = self.selections.clone();
        } else {
            if document.workspaces.len() >= MAX_WORKSPACES {
                return Err(format!(
                    "Hay más de {MAX_WORKSPACES} proyectos con selección de skills"
                ));
            }
            document.workspaces.push(WorkspaceSelections {
                workspace: self.workspace.clone(),
                selections: self.selections.clone(),
            });
        }
        write_document(data_root, &document)
    }

    /// Explicit recovery for a corrupt configuration file. It clears all
    /// workspace selections because the old file cannot be trusted or parsed.
    pub fn reset_all(data_root: &Path, workspace: &Path) -> Result<Self, String> {
        let workspace = canonical_workspace(workspace)?;
        write_document(
            data_root,
            &Document {
                version: VERSION,
                workspaces: Vec::new(),
            },
        )?;
        Ok(Self {
            workspace,
            selections: Vec::new(),
        })
    }
}

fn roots(data_root: &Path, workspace: &Path) -> Result<Vec<SkillRoot>, String> {
    let checked_workspace = canonical_workspace(workspace)?;
    if checked_workspace != workspace {
        return Err("La ruta del proyecto cambió desde que se cargaron sus skills".into());
    }
    let data_root = checked_data_root(data_root)?;
    let mut roots = Vec::with_capacity(2);
    if let Some(root) = optional_root(&data_root, &["local-skills"])? {
        roots.push(SkillRoot::user(root));
    }
    if let Some(root) = optional_root(workspace, &[".agents", "skills"])? {
        roots.push(SkillRoot::project(root));
    }
    Ok(roots)
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("No se pudo inspeccionar el proyecto: {error}"))?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err("El proyecto debe ser una carpeta real, sin enlaces".into());
    }
    fs::canonicalize(path).map_err(|error| format!("No se pudo resolver el proyecto: {error}"))
}

fn checked_data_root(path: &Path) -> Result<PathBuf, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_link_or_reparse(&metadata) {
                return Err("La carpeta de datos debe ser real, sin enlaces".into());
            }
            fs::canonicalize(path)
                .map_err(|error| format!("No se pudo resolver la carpeta de datos: {error}"))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::path::absolute(path)
            .map_err(|error| format!("No se pudo resolver la carpeta de datos: {error}")),
        Err(error) => Err(format!(
            "No se pudo inspeccionar la carpeta de datos: {error}"
        )),
    }
}

fn optional_root(base: &Path, parts: &[&str]) -> Result<Option<PathBuf>, String> {
    let mut current = base.to_path_buf();
    for part in parts {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || is_link_or_reparse(&metadata) {
                    return Err(format!(
                        "La raíz de skills debe ser una carpeta real: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "No se pudo inspeccionar la raíz de skills {}: {error}",
                    current.display()
                ));
            }
        }
        let canonical = fs::canonicalize(&current).map_err(|error| {
            format!("No se pudo resolver la raíz {}: {error}", current.display())
        })?;
        if !canonical.starts_with(base) {
            return Err(format!(
                "La raíz de skills escapa de su carpeta: {}",
                current.display()
            ));
        }
        current = canonical;
    }
    Ok(Some(current))
}

fn load_document(data_root: &Path) -> Result<Document, String> {
    let data_root = checked_data_root(data_root)?;
    let path = data_root.join(FILE_NAME);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Document {
                version: VERSION,
                workspaces: Vec::new(),
            });
        }
        Err(error) => {
            return Err(format!(
                "No se pudo inspeccionar las skills locales: {error}"
            ));
        }
    };
    if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_FILE_BYTES {
        return Err("El archivo de selección de skills no es válido o es demasiado grande".into());
    }
    let file = open_no_follow(&path)
        .map_err(|error| format!("No se pudo abrir la selección de skills: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("No se pudo inspeccionar las skills locales: {error}"))?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_FILE_BYTES {
        return Err("El archivo de selección de skills no es válido o es demasiado grande".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("No se pudo leer la selección de skills: {error}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("El archivo de selección de skills excede el límite de tamaño".into());
    }
    let document: Document = serde_json::from_slice(&bytes)
        .map_err(|error| format!("La selección de skills locales no es válida: {error}"))?;
    validate_document(&document)?;
    Ok(document)
}

fn validate_document(document: &Document) -> Result<(), String> {
    if document.version != VERSION {
        return Err(format!(
            "Versión de selección de skills no compatible: {}",
            document.version
        ));
    }
    if document.workspaces.len() > MAX_WORKSPACES {
        return Err("La selección de skills tiene demasiados proyectos".into());
    }
    let mut workspaces = BTreeSet::new();
    for entry in &document.workspaces {
        if !entry.workspace.is_absolute()
            || entry.workspace.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES
            || !workspaces.insert(workspace_key(&entry.workspace))
            || entry.selections.len() > MAX_ACTIVE_SKILLS
        {
            return Err("La selección de skills contiene un proyecto inválido o repetido".into());
        }
    }
    Ok(())
}

fn write_document(data_root: &Path, document: &Document) -> Result<(), String> {
    validate_document(document)?;
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("No se pudo codificar la selección de skills: {error}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("La selección de skills excede el límite de tamaño".into());
    }
    fs::create_dir_all(data_root)
        .map_err(|error| format!("No se pudo crear la carpeta de datos: {error}"))?;
    let data_root = checked_data_root(data_root)?;
    let temporary = data_root.join(format!(
        ".native-local-skills-{}-{}.tmp",
        std::process::id(),
        SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_atomically(&temporary, &data_root.join(FILE_NAME))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("No se pudo guardar la selección de skills: {error}"))
}

fn workspace_key(path: &Path) -> String {
    let key = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, name: &str, body: &str) {
        let folder = root.join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} description\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn workspace_selection_is_explicit_and_durable() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let project = root.path().join("project");
        fs::create_dir_all(&project).unwrap();
        skill(
            &project.join(".agents/skills"),
            "review",
            "Review carefully",
        );
        let mut settings = NativeSkillsSettings::load(&data, &project).unwrap();
        assert_eq!(settings.discover(&data).unwrap().len(), 1);
        assert!(settings.activate(&data).unwrap().instructions.is_empty());
        let view = NativeSkillsUiState::from_settings(&settings, &data);
        settings = settings.with_selections(view.toggle(0).unwrap());
        settings.save(&data).unwrap();
        let reopened = NativeSkillsSettings::load(&data, &project).unwrap();
        assert_eq!(reopened.selections.len(), 1);
        assert!(reopened
            .activate(&data)
            .unwrap()
            .instructions
            .contains("Review carefully"));
        fs::write(
            project.join(".agents/skills/review/SKILL.md"),
            "---\nname: review\ndescription: review description\n---\nChanged",
        )
        .unwrap();
        assert!(reopened.activate(&data).is_err());
        let view = NativeSkillsUiState::from_settings(&reopened, &data);
        assert!(view.activation_error.is_some());
        assert_ne!(view.available[0].selection, reopened.selections[0]);
        let updated = reopened.with_selections(view.toggle(0).unwrap());
        updated.save(&data).unwrap();
        assert!(updated
            .activate(&data)
            .unwrap()
            .instructions
            .contains("Changed"));
    }

    #[test]
    fn preview_retains_full_observed_document_and_revalidates_exact_hash_before_save() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let project = root.path().join("project");
        fs::create_dir_all(&project).unwrap();
        skill(
            &project.join(".agents/skills"),
            "review",
            "Contenido completo ñ\nFIN-OBSERVADO",
        );
        let settings = NativeSkillsSettings::load(&data, &project).unwrap();
        let mut view = NativeSkillsUiState::from_settings(&settings, &data);
        let document = fs::read_to_string(&view.available[0].document).unwrap();
        view.open_preview(0).unwrap();
        assert_eq!(view.preview_entry().unwrap().instructions, document);
        assert_eq!(
            view.preview.as_ref().unwrap(),
            &settings.discover(&data).unwrap()[0].selection()
        );
        assert!(
            !data.join(FILE_NAME).exists(),
            "review cannot save a selection"
        );
        let selected = settings.with_selections(view.toggle_preview().unwrap());
        selected.save(&data).unwrap();
        let saved = fs::read(data.join(FILE_NAME)).unwrap();
        assert!(
            !String::from_utf8(saved.clone())
                .unwrap()
                .contains("FIN-OBSERVADO"),
            "the complete document is not persisted in selections"
        );
        let mut view = NativeSkillsUiState::from_settings(&selected, &data);
        view.open_selected_preview(0).unwrap();
        assert!(
            view.toggle_preview().unwrap().is_empty(),
            "reviewing an exact selected skill permits explicit removal"
        );
        let changed = document.replace("FIN-OBSERVADO", "FIN-CAMBIADO");
        fs::write(&view.available[0].document, &changed).unwrap();
        // The preview stays an honest old observation; saving its exact hash
        // rejects the changed disk document rather than accepting unseen text.
        assert_eq!(view.preview_entry().unwrap().instructions, document);
        let stale = settings.with_selections(vec![view.preview.clone().unwrap()]);
        assert!(stale.save(&data).is_err());
        assert_eq!(fs::read(data.join(FILE_NAME)).unwrap(), saved);
        view.refresh(&selected, &data);
        assert!(view.preview.is_none());
        assert!(view.preview_return_focus.is_none());
        assert!(view.activation_error.is_some());
        view.open_selected_preview(0).unwrap();
        assert_eq!(view.preview_entry().unwrap().instructions, changed);
        assert_ne!(view.preview.as_ref().unwrap(), &selected.selections[0]);
        let accepted = selected.with_selections(view.toggle_preview().unwrap());
        accepted.save(&data).unwrap();
        let reopened = NativeSkillsSettings::load(&data, &project).unwrap();
        assert_eq!(reopened.selections, accepted.selections);
        assert!(reopened
            .activate(&data)
            .unwrap()
            .instructions
            .contains("FIN-CAMBIADO"));
        assert!(!fs::read_to_string(data.join(FILE_NAME))
            .unwrap()
            .contains("FIN-CAMBIADO"));
        let mut view = NativeSkillsUiState::from_settings(&reopened, &data);
        view.open_selected_preview(0).unwrap();
        fs::remove_file(&view.available[0].document).unwrap();
        view.refresh(&reopened, &data);
        assert!(view.available.is_empty());
        assert!(view.open_selected_preview(0).is_err());
        assert!(view.preview.is_none());
    }

    #[test]
    fn workspace_scopes_and_corrupt_file_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        skill(&data.join("local-skills"), "user", "User skill");
        skill(
            &first.join(".agents/skills"),
            "project",
            "First project skill",
        );
        let original = NativeSkillsSettings::load(&data, &first).unwrap();
        let catalog = original.discover(&data).unwrap();
        let selected = original.with_selections(vec![catalog[1].selection()]);
        selected.save(&data).unwrap();
        assert_eq!(
            NativeSkillsSettings::load(&data, &second)
                .unwrap()
                .selections
                .len(),
            0
        );
        fs::write(data.join(FILE_NAME), b"{broken").unwrap();
        assert!(NativeSkillsSettings::load(&data, &first).is_err());
        NativeSkillsSettings::reset_all(&data, &first).unwrap();
        assert!(NativeSkillsSettings::load(&data, &first)
            .unwrap()
            .selections
            .is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn project_junction_root_is_rejected() {
        use std::os::windows::fs::symlink_dir;
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let outside = root.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(project.join(".agents")).unwrap();
        if symlink_dir(&outside, project.join(".agents/skills")).is_err() {
            return; // Windows symlink privilege may be unavailable.
        }
        let settings = NativeSkillsSettings::load(root.path(), &project).unwrap();
        assert!(settings.discover(root.path()).is_err());
    }
}
