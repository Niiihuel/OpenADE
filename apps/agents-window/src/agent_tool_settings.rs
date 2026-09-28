//! Per-tool permissions for the native Rust agent.
//!
//! The JSON file is a user preference. It is never applied to CLI backends or
//! scheduled automations, which have their own tool policies.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use openide_harness::{LocalTool, PermissionMode, PermissionPolicy, ToolPermissions};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "agent-tool-settings.json";
const VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const ALL_TOOLS: [LocalTool; 5] = [
    LocalTool::ReadFile,
    LocalTool::ListFiles,
    LocalTool::SearchFiles,
    LocalTool::WriteFile,
    LocalTool::RunCommand,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentToolSettings {
    permissions: ToolPermissions,
}

#[derive(Deserialize, Serialize)]
struct Document {
    version: u32,
    permissions: ToolPermissions,
}

impl Default for AgentToolSettings {
    fn default() -> Self {
        Self {
            permissions: ToolPermissions::default(),
        }
    }
}

impl AgentToolSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(SETTINGS_FILE);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(format!(
                    "No se pudieron abrir los permisos del agente: {error}"
                ))
            }
        };
        if file
            .metadata()
            .map_err(|error| {
                format!("No se pudieron inspeccionar los permisos del agente: {error}")
            })?
            .len()
            > MAX_SETTINGS_BYTES
        {
            return Err("El archivo de permisos del agente es demasiado grande".to_owned());
        }
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudieron leer los permisos del agente: {error}"))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err("El archivo de permisos del agente es demasiado grande".to_owned());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Los permisos del agente no son válidos: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de permisos del agente no compatible: {}",
                document.version
            ));
        }
        Ok(Self {
            permissions: document.permissions,
        })
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            permissions: self.permissions,
        })
        .map_err(|error| format!("No se pudieron codificar los permisos del agente: {error}"))?;
        let temporary = root.join(format!(
            ".agent-tool-settings-{}-{}.tmp",
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
            replace_atomically(&temporary, &root.join(SETTINGS_FILE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| format!("No se pudieron guardar los permisos del agente: {error}"))
    }

    pub fn permissions(&self) -> ToolPermissions {
        self.permissions
    }

    pub fn enabled(&self, tool: LocalTool) -> bool {
        self.permissions.allows(tool)
    }

    pub fn set(&mut self, tool: LocalTool, enabled: bool) {
        self.permissions.set(tool, enabled);
    }

    pub fn toggle(&mut self, tool: LocalTool) -> bool {
        let enabled = !self.enabled(tool);
        self.set(tool, enabled);
        enabled
    }

    pub fn mode_for(&self, tool: LocalTool) -> PermissionMode {
        if !self.enabled(tool) {
            PermissionMode::Deny
        } else if matches!(tool, LocalTool::WriteFile | LocalTool::RunCommand) {
            PermissionMode::Ask
        } else {
            PermissionMode::Allow
        }
    }

    /// Deny unknown future tools by default. A disabled tool is also omitted
    /// from the model's advertised definitions by `LocalTools::with_permissions`.
    pub fn permission_policy(&self) -> PermissionPolicy {
        ALL_TOOLS.iter().copied().fold(
            PermissionPolicy::new().with_default_mode(PermissionMode::Deny),
            |policy, tool| policy.with_tool(tool.name(), self.mode_for(tool)),
        )
    }

    pub fn tool_from_action_id(id: &str) -> Option<LocalTool> {
        match id {
            "agent_tool:read_file" => Some(LocalTool::ReadFile),
            "agent_tool:list_files" => Some(LocalTool::ListFiles),
            "agent_tool:search_files" => Some(LocalTool::SearchFiles),
            "agent_tool:write_file" => Some(LocalTool::WriteFile),
            "agent_tool:run_command" => Some(LocalTool::RunCommand),
            _ => None,
        }
    }
}

#[cfg(not(windows))]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    #[link(name = "Kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file: *const u16,
            replacement_file: *const u16,
            backup_file: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    if !destination.exists() {
        return fs::rename(temporary, destination);
    }
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary_wide: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if replaced != 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if !destination.exists() {
            fs::rename(temporary, destination)
        } else {
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_harness::{LocalTools, ToolExecutor};

    #[test]
    fn safe_defaults_and_independent_modes() {
        let mut settings = AgentToolSettings::default();
        for tool in [
            LocalTool::ReadFile,
            LocalTool::ListFiles,
            LocalTool::SearchFiles,
        ] {
            assert!(settings.enabled(tool));
            assert_eq!(settings.mode_for(tool), PermissionMode::Allow);
        }
        for tool in [LocalTool::WriteFile, LocalTool::RunCommand] {
            assert!(!settings.enabled(tool));
            assert_eq!(settings.mode_for(tool), PermissionMode::Deny);
        }
        settings.set(LocalTool::WriteFile, true);
        settings.set(LocalTool::SearchFiles, false);
        assert_eq!(settings.mode_for(LocalTool::WriteFile), PermissionMode::Ask);
        assert_eq!(
            settings.mode_for(LocalTool::RunCommand),
            PermissionMode::Deny
        );
        assert_eq!(
            settings.mode_for(LocalTool::SearchFiles),
            PermissionMode::Deny
        );

        let tools = LocalTools::new().with_permissions(settings.permissions());
        let names: Vec<_> = tools
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains(&"write_file".to_owned()));
        assert!(!names.contains(&"run_command".to_owned()));
        assert!(!names.contains(&"search_files".to_owned()));
    }

    #[test]
    fn saves_and_replaces_preferences_without_losing_independent_choices() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = AgentToolSettings::default();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), settings);
        settings.set(LocalTool::RunCommand, true);
        settings.set(LocalTool::ReadFile, false);
        settings.save(root.path()).unwrap();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), settings);
        settings.set(LocalTool::WriteFile, true);
        settings.set(LocalTool::RunCommand, false);
        settings.save(root.path()).unwrap();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), settings);
    }

    #[test]
    fn rejects_corrupt_or_unknown_version_instead_of_enabling_tools() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(SETTINGS_FILE);
        fs::write(&path, b"{broken").unwrap();
        assert!(AgentToolSettings::load(root.path()).is_err());
        fs::write(
            &path,
            br#"{"version":99,"permissions":{"write_file":true}}"#,
        )
        .unwrap();
        assert!(AgentToolSettings::load(root.path()).is_err());
    }

    #[test]
    fn recognizes_only_the_five_native_tool_actions() {
        for tool in ALL_TOOLS {
            let action = format!("agent_tool:{}", tool.name());
            assert_eq!(AgentToolSettings::tool_from_action_id(&action), Some(tool));
        }
        assert_eq!(
            AgentToolSettings::tool_from_action_id("agent_tool:mcp"),
            None
        );
    }
}
