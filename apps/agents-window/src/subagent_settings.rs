//! Host preferences for future native delegation, separate from durable child state.

use openide_harness::{SubagentLimits, SubagentToolLimits};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "subagent-settings.json";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SubagentSettings {
    pub enabled: bool,
    pub max_depth: u8,
    pub max_children_per_parent: u8,
    pub max_concurrent_children: u8,
    pub max_total_descendants: u16,
    pub max_tasks: u8,
}

impl Default for SubagentSettings {
    fn default() -> Self {
        let coordinator = SubagentLimits::default();
        Self {
            enabled: true,
            max_depth: coordinator.max_depth,
            max_children_per_parent: coordinator.max_children_per_parent as u8,
            max_concurrent_children: coordinator.max_concurrent_children as u8,
            max_total_descendants: coordinator.max_total_descendants as u16,
            max_tasks: SubagentToolLimits::default().max_tasks as u8,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    settings: SubagentSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubagentLimit {
    Depth,
    Children,
    Concurrent,
    Descendants,
    Tasks,
}

impl SubagentLimit {
    pub const ALL: [Self; 5] = [
        Self::Depth,
        Self::Children,
        Self::Concurrent,
        Self::Descendants,
        Self::Tasks,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Depth => "max_depth",
            Self::Children => "max_children_per_parent",
            Self::Concurrent => "max_concurrent_children",
            Self::Descendants => "max_total_descendants",
            Self::Tasks => "max_tasks",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|limit| limit.key() == key)
    }

    pub fn max(self) -> u16 {
        match self {
            Self::Depth => 8,
            Self::Descendants => 128,
            _ => 32,
        }
    }

    pub fn value(self, settings: &SubagentSettings) -> u16 {
        match self {
            Self::Depth => u16::from(settings.max_depth),
            Self::Children => u16::from(settings.max_children_per_parent),
            Self::Concurrent => u16::from(settings.max_concurrent_children),
            Self::Descendants => settings.max_total_descendants,
            Self::Tasks => u16::from(settings.max_tasks),
        }
    }

    pub fn set(self, settings: &mut SubagentSettings, value: u16) -> Result<(), String> {
        if !(1..=self.max()).contains(&value) {
            return Err("El límite de subagentes está fuera de rango.".into());
        }
        match self {
            Self::Depth => settings.max_depth = value as u8,
            Self::Children => settings.max_children_per_parent = value as u8,
            Self::Concurrent => settings.max_concurrent_children = value as u8,
            Self::Descendants => settings.max_total_descendants = value,
            Self::Tasks => settings.max_tasks = value as u8,
        }
        Ok(())
    }
}

impl SubagentSettings {
    pub fn validate(self) -> Result<(), String> {
        if SubagentLimit::ALL
            .into_iter()
            .any(|limit| !(1..=limit.max()).contains(&limit.value(&self)))
        {
            return Err("Los límites de subagentes no son válidos.".into());
        }
        Ok(())
    }

    pub fn coordinator_limits(self) -> SubagentLimits {
        SubagentLimits {
            max_depth: self.max_depth,
            max_children_per_parent: usize::from(self.max_children_per_parent),
            max_concurrent_children: usize::from(self.max_concurrent_children),
            max_total_descendants: usize::from(self.max_total_descendants),
        }
    }

    pub fn tool_limits(self) -> SubagentToolLimits {
        SubagentToolLimits {
            max_tasks: usize::from(self.max_tasks),
        }
    }

    pub fn load(root: &Path) -> Result<Self, String> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(root.join(FILE_NAME)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Err(format!(
                    "No se pudo abrir la configuración de subagentes: {error}"
                ))
            }
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("La configuración de subagentes debe ser un archivo regular.".into());
        }
        if metadata.len() > MAX_BYTES {
            return Err("El archivo de configuración de subagentes es demasiado grande.".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("El archivo de configuración de subagentes es demasiado grande.".into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("La configuración de subagentes no es válida: {error}"))?;
        if document.version != VERSION {
            return Err("La versión de configuración de subagentes no es compatible.".into());
        }
        document.settings.validate()?;
        Ok(document.settings)
    }

    pub fn save(self, root: &Path) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            settings: self,
        })
        .map_err(|error| error.to_string())?;
        openide_harness::atomic_write(&root.join(FILE_NAME), &bytes)
            .map_err(|error| format!("No se pudo guardar la configuración de subagentes: {error}"))
    }

    /// Explicit recovery preserves even an oversized original without reading it.
    pub fn reset_with_backup(root: &Path) -> Result<(Self, PathBuf), String> {
        if Self::load(root).is_ok() {
            return Err(
                "La configuración ya es válida. Recargala para conservar sus valores.".into(),
            );
        }
        let path = root.join(FILE_NAME);
        let backup = root.join(format!(
            "subagent-settings-backup-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::rename(&path, &backup)
            .map_err(|error| format!("No se pudo conservar la configuración original: {error}"))?;
        let settings = Self::default();
        if let Err(error) = settings.save(root) {
            return match std::fs::rename(&backup, &path) {
                Ok(()) => Err(error),
                Err(restore) => Err(format!(
                    "{error}. La copia está en {}. No se pudo restaurar: {restore}",
                    backup.display()
                )),
            };
        }
        Ok((settings, backup))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SubagentPersona {
    pub name: String,
    pub description: String,
    pub origin: String,
    pub file: PathBuf,
}

pub(crate) fn personas(root: &Path) -> Vec<SubagentPersona> {
    let mut personas: Vec<_> = crate::compat_hook_settings::enabled_plugin_roots(root)
        .into_iter()
        .flat_map(|(origin, plugin)| {
            crate::compat_hook_settings::plugin_personas(&plugin)
                .into_iter()
                .map(move |persona| SubagentPersona {
                    name: format!(
                        "{}:{}",
                        origin.split('@').next().unwrap_or(&origin),
                        persona.name
                    ),
                    description: persona.description,
                    origin: origin.clone(),
                    file: persona.file,
                })
        })
        .collect();
    personas.sort_by(|a, b| (&a.name, &a.origin, &a.file).cmp(&(&b.name, &b.origin, &b.file)));
    personas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn non_regular_config_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FILE_NAME);
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated path; mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let config_root = root.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(SubagentSettings::load(&config_root));
        });
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("a FIFO must not block the loader")
            .unwrap_err()
            .contains("regular"));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(SubagentSettings::load(root.path()).is_err());
    }

    #[test]
    fn settings_reopen_exact_values_and_match_runtime_defaults_and_ceilings() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = SubagentSettings::load(root.path()).unwrap();
        assert_eq!(settings.coordinator_limits(), SubagentLimits::default());
        assert_eq!(settings.tool_limits(), SubagentToolLimits::default());
        settings.enabled = false;
        for limit in SubagentLimit::ALL {
            limit.set(&mut settings, limit.max()).unwrap();
            assert!(limit.set(&mut settings, 0).is_err());
            assert!(limit.set(&mut settings, limit.max() + 1).is_err());
        }
        settings.save(root.path()).unwrap();
        assert_eq!(SubagentSettings::load(root.path()).unwrap(), settings);
        let sessions = std::sync::Arc::new(
            openide_core::SessionStore::open(root.path().join("sessions")).unwrap(),
        );
        assert!(
            openide_harness::SubagentCoordinator::new(sessions, settings.coordinator_limits())
                .is_ok()
        );
    }

    #[test]
    fn invalid_documents_never_overwrite_valid_settings_and_reset_keeps_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let settings = SubagentSettings::default();
        settings.save(root.path()).unwrap();
        let path = root.path().join(FILE_NAME);
        let original = std::fs::read(&path).unwrap();
        let mut invalid = settings;
        invalid.max_depth = 0;
        assert!(invalid.save(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(SubagentSettings::reset_with_backup(root.path()).is_err());
        for broken in [
            br#"{"version":99,"settings":{}}"#.as_slice(),
            b"broken-json",
            &[b'x'; MAX_BYTES as usize + 1],
        ] {
            std::fs::write(&path, broken).unwrap();
            assert!(SubagentSettings::load(root.path()).is_err());
            let (recovered, backup) = SubagentSettings::reset_with_backup(root.path()).unwrap();
            assert_eq!(std::fs::read(backup).unwrap(), broken);
            assert_eq!(recovered, settings);
            assert_eq!(SubagentSettings::load(root.path()).unwrap(), settings);
        }
    }

    #[test]
    fn persona_catalog_contains_only_approved_plugin_resources() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugin");
        std::fs::create_dir_all(plugin.join("agents")).unwrap();
        std::fs::write(
            plugin.join("agents/review.md"),
            "---\nname: reviewer\ndescription: Inspect a change\n---\nReview carefully.",
        )
        .unwrap();
        assert!(personas(root.path()).is_empty());
        let mut settings = crate::compat_hook_settings::CompatHookSettings::default();
        settings.toggle_plugin_skills(&crate::compat_hook_settings::DiscoveredPlugin {
            id: "review-kit@market".into(),
            root: plugin.clone(),
            skills_dir: plugin.join("skills"),
            skill_count: 1,
        });
        settings.save(root.path()).unwrap();
        let catalog = personas(root.path());
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            (catalog[0].name.as_str(), catalog[0].origin.as_str()),
            ("review-kit:reviewer", "review-kit@market")
        );
        assert_eq!(
            catalog[0].file,
            plugin.canonicalize().unwrap().join("agents/review.md")
        );
    }
}
