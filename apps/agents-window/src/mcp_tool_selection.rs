//! Explicit raw MCP tool names, scoped to the saved registration they were
//! reviewed against. Discovery remains complete; only model advertisement filters.
use crate::mcp_settings::{McpServerSettings, McpSettings, McpSettingsScope, MAX_SERVERS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::Path;

const FILE_NAME: &str = "mcp-tool-selections.json";
const MAX_BYTES: u64 = 64 * 1024;
/// The discovery bound: a selection past the model's catalog is deferred
/// behind `search_mcp_tools`, not refused.
pub(crate) const MAX_SELECTED_TOOLS: usize = openide_harness::MAX_MCP_TOOLS;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct McpToolSelections {
    pub servers: BTreeMap<String, McpSelectedTools>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpSelectedTools {
    registration_digest: String,
    pub names: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    servers: BTreeMap<String, McpSelectedTools>,
}

fn registration_digest(server: &McpServerSettings) -> String {
    let mut server = server.clone();
    server.enabled = false;
    let mut hash = Sha256::new();
    hash.update(b"openade-mcp-tool-selection-registration-v1\0");
    hash.update(serde_json::to_vec(&server).expect("MCP registration serializes"));
    format!("{:x}", hash.finalize())
}

fn validate_names(names: &[String]) -> Result<(), String> {
    if names.len() > MAX_SELECTED_TOOLS {
        return Err(format!("La selección MCP supera {MAX_SELECTED_TOOLS} herramientas."));
    }
    let mut unique = BTreeSet::new();
    if names.iter().any(|name| {
        name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || !unique.insert(name)
    }) {
        return Err("La selección MCP contiene nombres no válidos o repetidos".into());
    }
    Ok(())
}

impl McpToolSelections {
    pub fn load_scope(
        root: &Path,
        workspace: &Path,
        scope: McpSettingsScope,
    ) -> Result<Self, String> {
        let path = scope.storage_root(root, workspace)?.join(FILE_NAME);
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("No se pudo abrir la selección de herramientas MCP".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "No se pudo revisar la selección MCP")?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err("La selección MCP no es un archivo regular o supera 64 KiB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "No se pudo leer la selección MCP")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("La selección MCP supera 64 KiB".into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|_| "El archivo de selección MCP no es válido")?;
        if document.version != 1 {
            return Err("Versión de selección MCP no compatible".into());
        }
        let selections = Self {
            servers: document.servers,
        };
        selections.validate()?;
        Ok(selections)
    }

    pub fn load_effective(root: &Path, workspace: &Path) -> Result<Self, String> {
        let user = McpSettings::load_scope(root, workspace, McpSettingsScope::User)?;
        let mut selected = Self::load_scope(root, workspace, McpSettingsScope::User)?;
        selected
            .servers
            .retain(|id, _| user.servers.iter().any(|server| &server.id == id));
        if workspace.is_dir() {
            let project = McpSettings::load_scope(root, workspace, McpSettingsScope::Project)?;
            let local = Self::load_scope(root, workspace, McpSettingsScope::Project)?;
            for server in project.servers {
                selected.servers.remove(&server.id);
                if let Some(selection) = local.servers.get(&server.id) {
                    selected.servers.insert(server.id, selection.clone());
                }
            }
        }
        Ok(selected)
    }

    pub fn set_for(
        &mut self,
        server: &McpServerSettings,
        names: Option<Vec<String>>,
    ) -> Result<(), String> {
        McpSettings {
            servers: vec![server.clone()],
        }
        .validate_structure()?;
        match names {
            None => {
                self.servers.remove(&server.id);
            }
            Some(mut names) => {
                validate_names(&names)?;
                names.sort();
                self.servers.insert(
                    server.id.clone(),
                    McpSelectedTools {
                        registration_digest: registration_digest(server),
                        names,
                    },
                );
            }
        }
        Ok(())
    }

    pub fn selected_for(&self, server: &McpServerSettings) -> Result<Option<&[String]>, String> {
        let Some(selection) = self.servers.get(&server.id) else {
            return Ok(None);
        };
        if selection.registration_digest != registration_digest(server) {
            return Err(format!("La configuración de {} cambió desde la selección de herramientas. Inspeccioná y revisá la selección, o elegí Todas explícitamente.", server.id));
        }
        validate_names(&selection.names)?;
        Ok(Some(&selection.names))
    }

    pub fn save_scope(
        &self,
        root: &Path,
        workspace: &Path,
        scope: McpSettingsScope,
    ) -> Result<(), String> {
        let saved = McpSettings::load_scope(root, workspace, scope)?;
        let mut kept = self.clone();
        kept.servers
            .retain(|id, _| saved.servers.iter().any(|server| &server.id == id));
        kept.validate()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: 1,
            servers: kept.servers,
        })
        .map_err(|_| "No se pudo codificar la selección MCP")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("La selección MCP supera 64 KiB".into());
        }
        openide_harness::atomic_write(
            &scope.storage_root(root, workspace)?.join(FILE_NAME),
            &bytes,
        )
        .map_err(|_| "No se pudo guardar la selección MCP".into())
    }

    fn validate(&self) -> Result<(), String> {
        if self.servers.len() > MAX_SERVERS {
            return Err("Demasiados registros en la selección MCP".into());
        }
        for (id, selection) in &self.servers {
            if id.is_empty()
                || id.len() > 32
                || !id.starts_with(|c: char| c.is_ascii_lowercase())
                || !id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
                || selection.registration_digest.len() != 64
                || !selection
                    .registration_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("Registro de selección MCP no válido".into());
            }
            validate_names(&selection.names)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn server() -> McpServerSettings {
        McpServerSettings {
            id: "rea".into(),
            enabled: true,
            program: "rea".into(),
            args: vec!["--mcp".into()],
            cwd: None,
            credential_env_names: vec![],
            env: Default::default(),
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: vec![],
        }
    }
    #[test]
    fn selection_round_trips_all_none_and_explicit_names_without_mutating_registration() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let server = server();
        McpSettings {
            servers: vec![server.clone()],
        }
        .save(root.path(), workspace.path())
        .unwrap();
        let original = std::fs::read(root.path().join("mcp-servers.json")).unwrap();
        let mut selected = McpToolSelections::default();
        assert_eq!(selected.selected_for(&server).unwrap(), None);
        selected
            .set_for(&server, Some(vec!["inspect_analysis_view".into()]))
            .unwrap();
        selected
            .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
        let mut reopened =
            McpToolSelections::load_effective(root.path(), workspace.path()).unwrap();
        assert_eq!(
            reopened.selected_for(&server).unwrap().unwrap(),
            ["inspect_analysis_view"]
        );
        let mut disabled = server.clone();
        disabled.enabled = false;
        assert!(reopened.selected_for(&disabled).unwrap().is_some());
        let mut edited = server.clone();
        edited.args.push("changed".into());
        assert!(reopened.selected_for(&edited).is_err());
        reopened.set_for(&server, Some(vec![])).unwrap();
        assert_eq!(reopened.selected_for(&server).unwrap(), Some([].as_slice()));
        reopened.set_for(&server, None).unwrap();
        assert_eq!(reopened.selected_for(&server).unwrap(), None);
        assert_eq!(
            std::fs::read(root.path().join("mcp-servers.json")).unwrap(),
            original
        );
    }
    #[test]
    fn project_override_uses_its_own_selection_including_all_without_inheriting_user_filter() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let user = server();
        McpSettings {
            servers: vec![user.clone()],
        }
        .save(root.path(), workspace.path())
        .unwrap();
        let mut selections = McpToolSelections::default();
        selections
            .set_for(&user, Some(vec!["user_tool".into()]))
            .unwrap();
        selections
            .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
        let mut project = user.clone();
        project.program = "project-rea".into();
        project.enabled = false;
        McpSettings {
            servers: vec![project.clone()],
        }
        .save_scope(root.path(), workspace.path(), McpSettingsScope::Project)
        .unwrap();
        assert_eq!(
            McpToolSelections::load_effective(root.path(), workspace.path())
                .unwrap()
                .selected_for(&project)
                .unwrap(),
            None
        );
        let mut local = McpToolSelections::default();
        local.set_for(&project, Some(vec![])).unwrap();
        local
            .save_scope(root.path(), workspace.path(), McpSettingsScope::Project)
            .unwrap();
        assert_eq!(
            McpToolSelections::load_effective(root.path(), workspace.path())
                .unwrap()
                .selected_for(&project)
                .unwrap(),
            Some([].as_slice())
        );
        assert_eq!(
            McpToolSelections::load_scope(root.path(), workspace.path(), McpSettingsScope::User)
                .unwrap()
                .selected_for(&user)
                .unwrap()
                .unwrap(),
            ["user_tool"]
        );
    }
    #[test]
    fn invalid_and_excess_selections_never_overwrite_the_saved_selection() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let server = server();
        McpSettings {
            servers: vec![server.clone()],
        }
        .save(root.path(), workspace.path())
        .unwrap();
        let mut selections = McpToolSelections::default();
        selections
            .set_for(&server, Some(vec!["valid".into()]))
            .unwrap();
        selections
            .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
        let original = std::fs::read(root.path().join(FILE_NAME)).unwrap();
        for names in [
            vec!["duplicate".into(), "duplicate".into()],
            vec!["invalid name".into()],
            (0..=MAX_SELECTED_TOOLS).map(|i| format!("tool_{i}")).collect(),
        ] {
            assert!(selections.set_for(&server, Some(names)).is_err());
        }
        assert_eq!(
            std::fs::read(root.path().join(FILE_NAME)).unwrap(),
            original
        );
        std::fs::write(root.path().join(FILE_NAME), b"{corrupt").unwrap();
        assert!(McpToolSelections::load_effective(root.path(), workspace.path()).is_err());
    }
}
