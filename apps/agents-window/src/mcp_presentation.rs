//! Cosmetic MCP identity and transient inspector state. None of this grants tools
//! or participates in a runtime's authority fingerprint.
use crate::mcp_settings::McpSettingsScope;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::Path;

const FILE_NAME: &str = "mcp-presentation.json";
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "mode", content = "id", rename_all = "snake_case")]
pub(crate) enum BrandChoice {
    #[default]
    Auto,
    None,
    Brand(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPresentationPreferences {
    pub icons: BTreeMap<String, BrandChoice>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    icons: BTreeMap<String, BrandChoice>,
}

impl McpPresentationPreferences {
    pub fn load(root: &Path, workspace: &Path, scope: McpSettingsScope) -> Result<Self, String> {
        let path = scope.storage_root(root, workspace)?.join(FILE_NAME);
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("No se pudo abrir la apariencia de MCP".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "No se pudo revisar la apariencia de MCP")?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err("La apariencia de MCP debe ser un archivo de hasta 64 KiB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "No se pudo leer la apariencia de MCP")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("La apariencia de MCP supera 64 KiB".into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|_| "El archivo de apariencia MCP no es válido")?;
        if document.version != 1 {
            return Err("Versión de apariencia MCP no compatible".into());
        }
        let preferences = Self {
            icons: document.icons,
        };
        preferences.validate()?;
        Ok(preferences)
    }

    pub fn save(
        &self,
        root: &Path,
        workspace: &Path,
        scope: McpSettingsScope,
    ) -> Result<(), String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: 1,
            icons: self.icons.clone(),
        })
        .map_err(|_| "No se pudo codificar la apariencia de MCP")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("La apariencia de MCP supera 64 KiB".into());
        }
        openide_harness::atomic_write(
            &scope.storage_root(root, workspace)?.join(FILE_NAME),
            &bytes,
        )
        .map_err(|_| "No se pudo guardar la apariencia de MCP".into())
    }

    fn validate(&self) -> Result<(), String> {
        if self.icons.len() > crate::mcp_settings::MAX_SERVERS {
            return Err("Demasiados iconos MCP guardados".into());
        }
        for (server, choice) in &self.icons {
            if server.is_empty()
                || server.len() > 32
                || !server.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                })
            {
                return Err("Identidad de icono MCP no válida".into());
            }
            if let BrandChoice::Brand(id) = choice {
                if id.is_empty()
                    || id.len() > 128
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                {
                    return Err("Referencia de icono MCP no válida".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct McpPresentationUi {
    pub workspace: String,
    pub canonical_workspace: Option<std::path::PathBuf>,
    pub details: Option<String>,
    pub search: String,
    pub tool_query: String,
    pub expanded_tool: Option<String>,
    pub technical: bool,
    pub icon_picker: bool,
    pub icon_query: String,
    pub preferences: McpPresentationPreferences,
    pub preferences_error: Option<String>,
    pub discovery: BTreeMap<String, crate::mcp_discovery::McpLocalDiscovery>,
    pub discovery_error: Option<String>,
    pub selections: crate::mcp_tool_selection::McpToolSelections,
    pub selection_error: Option<String>,
    pub selection_draft: Option<BTreeSet<String>>,
    pub selection_dirty: bool,
    pub selection_server: Option<crate::mcp_settings::McpServerSettings>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cosmetic_choices_are_scoped_and_do_not_change_server_registration() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut prefs = McpPresentationPreferences::default();
        prefs.icons.insert(
            "local".into(),
            BrandChoice::Brand("svgl-unknown-future-id".into()),
        );
        prefs
            .save(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
        assert_eq!(
            McpPresentationPreferences::load(root.path(), workspace.path(), McpSettingsScope::User)
                .unwrap(),
            prefs
        );
        assert!(McpPresentationPreferences::load(
            root.path(),
            workspace.path(),
            McpSettingsScope::Project
        )
        .unwrap()
        .icons
        .is_empty());
        assert!(!root.path().join("mcp-servers.json").exists());
        std::fs::write(root.path().join(FILE_NAME), b"{corrupt").unwrap();
        assert!(McpPresentationPreferences::load(
            root.path(),
            workspace.path(),
            McpSettingsScope::User
        )
        .is_err());
    }
}
