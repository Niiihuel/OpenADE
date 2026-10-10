//! The agent CLIs OpenIDE knows how to start, kept as data. Adding one is an
//! entry in `agent-cli-definitions.json`; only a new way of connecting the
//! bridge needs code.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use openide_bridge::BridgeKind;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentCliDefinition {
    pub id: String,
    pub label: String,
    /// Names tried in order on the search path.
    pub executables: Vec<String>,
    /// Arguments that start the CLI without approval prompts. Empty means
    /// OpenIDE offers no accept-all profile for it.
    pub accept_all_args: Vec<String>,
    /// How OpenIDE hands this CLI its tools. `None` starts it unconnected.
    pub bridge: Option<BridgeKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    agents: Vec<AgentCliDefinition>,
}

pub(crate) fn definitions() -> &'static [AgentCliDefinition] {
    static DEFINITIONS: OnceLock<Vec<AgentCliDefinition>> = OnceLock::new();
    DEFINITIONS.get_or_init(|| {
        let document: Document = serde_json::from_str(include_str!("agent-cli-definitions.json"))
            .expect("bundled agent CLI definitions are valid");
        assert_eq!(document.version, 1, "bundled agent CLI definitions version");
        document.agents
    })
}

const ACCEPT_ALL_SUFFIX: &str = "_accept_all";

/// Resolves a terminal profile ID to its CLI and whether it is the
/// accept-all variant.
pub(crate) fn profile(id: &str) -> Option<(&'static AgentCliDefinition, bool)> {
    let (base, accept_all) = match id.strip_suffix(ACCEPT_ALL_SUFFIX) {
        Some(base) => (base, true),
        None => (id, false),
    };
    definitions()
        .iter()
        .find(|agent| agent.id == base && (!accept_all || !agent.accept_all_args.is_empty()))
        .map(|agent| (agent, accept_all))
}

impl AgentCliDefinition {
    /// Profile IDs and labels this CLI contributes to the terminal menu.
    pub fn profiles(&self) -> Vec<(String, String)> {
        let mut profiles = vec![(self.id.clone(), self.label.clone())];
        if !self.accept_all_args.is_empty() {
            profiles.push((
                format!("{}{ACCEPT_ALL_SUFFIX}", self.id),
                format!("{} · aceptar todo", self.label),
            ));
        }
        profiles
    }

    /// The executable to start: the user's own path when it still points at
    /// one, otherwise the first name found on `search_path`.
    pub fn resolve(&self, manual: Option<&Path>, search_path: Option<&OsStr>) -> Option<PathBuf> {
        if let Some(found) = manual.and_then(openide_cli::detect_executable) {
            return Some(found);
        }
        self.executables
            .iter()
            .find_map(|name| openide_cli::detect_executable_in(name, search_path))
    }
}

const PATHS_FILE: &str = "agent-cli-paths.json";

/// Executable paths the user set by hand, by CLI ID.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentCliPaths {
    version: u32,
    paths: std::collections::BTreeMap<String, PathBuf>,
}

impl AgentCliPaths {
    pub fn load(root: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(root.join(PATHS_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudieron leer las rutas de CLIs: {error}")),
        };
        let paths: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Las rutas de CLIs no son válidas: {error}"))?;
        if paths.version != 1 {
            return Err(format!("Versión de rutas de CLIs no compatible: {}", paths.version));
        }
        Ok(paths)
    }

    pub fn get(&self, id: &str) -> Option<&Path> {
        self.paths.get(id).map(PathBuf::as_path)
    }

    /// Stores a path for a known CLI, or clears it when `path` is empty.
    /// The path must be an executable file now; it is re-checked at launch.
    pub fn set(&mut self, id: &str, path: &str) -> Result<(), String> {
        if profile(id).is_none_or(|(_, accept_all)| accept_all) {
            return Err("CLI desconocida".into());
        }
        let path = path.trim();
        if path.is_empty() {
            self.paths.remove(id);
            return Ok(());
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() || openide_cli::detect_executable(&path).is_none() {
            return Err("La ruta debe ser absoluta y apuntar a un ejecutable".into());
        }
        self.paths.insert(id.to_owned(), path);
        Ok(())
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let document = Self { version: 1, paths: self.paths.clone() };
        let temporary = root.join(format!(".{PATHS_FILE}-{}.tmp", std::process::id()));
        std::fs::create_dir_all(root)
            .and_then(|()| std::fs::write(&temporary, serde_json::to_vec_pretty(&document).unwrap_or_default()))
            .and_then(|()| crate::agent_tool_settings::replace_atomically(&temporary, &root.join(PATHS_FILE)))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                format!("No se pudieron guardar las rutas de CLIs: {error}")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn manual_paths_accept_only_absolute_executables_of_known_clis_and_persist() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("my-codex");
        std::fs::write(&binary, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "x").unwrap();

        let mut paths = AgentCliPaths::load(dir.path()).unwrap();
        assert!(paths.set("bash", binary.to_str().unwrap()).is_err());
        assert!(paths.set("codex_accept_all", binary.to_str().unwrap()).is_err());
        assert!(paths.set("codex", "my-codex").is_err());
        assert!(paths.set("codex", plain.to_str().unwrap()).is_err());
        paths.set("codex", &format!("  {}  ", binary.display())).unwrap();
        paths.save(dir.path()).unwrap();
        let reloaded = AgentCliPaths::load(dir.path()).unwrap();
        assert_eq!(reloaded.get("codex"), Some(binary.as_path()));
        assert_eq!(reloaded.get("claude"), None);

        paths.set("codex", "").unwrap();
        assert_eq!(paths.get("codex"), None);
    }

    #[test]
    fn bundled_definitions_are_unique_and_launchable_as_written() {
        let agents = definitions();
        assert!(!agents.is_empty());
        let mut ids = std::collections::HashSet::new();
        for agent in agents {
            assert!(ids.insert(agent.id.as_str()), "duplicate id {}", agent.id);
            assert!(!agent.id.ends_with(ACCEPT_ALL_SUFFIX) && !agent.label.is_empty());
            assert!(!agent.executables.is_empty());
            assert!(agent.executables.iter().all(|name| !name.contains(['/', '\\'])));
        }
    }

    #[test]
    fn profiles_resolve_only_for_variants_a_cli_declares() {
        for agent in definitions() {
            for (id, _) in agent.profiles() {
                let (found, accept_all) = profile(&id).unwrap();
                assert_eq!(found.id, agent.id);
                assert_eq!(accept_all, id.ends_with(ACCEPT_ALL_SUFFIX));
            }
        }
        assert!(profile("bash").is_none() && profile("_accept_all").is_none());
        assert!(profile("claude_accept_all").is_some_and(|(_, accept_all)| accept_all));
    }

    #[cfg(unix)]
    #[test]
    fn a_manual_path_wins_and_a_dead_one_falls_back_to_the_search_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let on_path = make("claude");
        let manual = make("my-claude");
        let agent = profile("claude").unwrap().0;
        let search = dir.path().as_os_str();
        assert_eq!(agent.resolve(Some(&manual), Some(search)), Some(manual));
        assert_eq!(
            agent.resolve(Some(&dir.path().join("gone")), Some(search)),
            Some(on_path)
        );
        assert_eq!(agent.resolve(None, Some(OsStr::new("/nonexistent-dir"))), None);
    }
}
