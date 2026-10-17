//! Per-tool permissions for the native Rust agent.
//!
//! The JSON file is a user preference. It is never applied to CLI backends or
//! scheduled automations, which have their own tool policies.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use openide_harness::{
    CommandIsolation, LocalTool, PermissionMode, PermissionPolicy, ToolPermissions,
};
use serde::{Deserialize, Serialize};

const SETTINGS_FILE: &str = "agent-tool-settings.json";
const VERSION: u32 = 3;
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
    command_isolation: CommandIsolation,
    full_access: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    permissions: ToolPermissions,
    #[serde(default)]
    command_isolation: Option<CommandIsolation>,
    #[serde(default)]
    full_access: bool,
}

impl Default for AgentToolSettings {
    fn default() -> Self {
        Self {
            permissions: ToolPermissions::default(),
            command_isolation: if cfg!(windows) {
                CommandIsolation::WorkspaceWriteOffline
            } else {
                CommandIsolation::HostUnrestricted
            },
            full_access: false,
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
        let command_isolation = match (document.version, document.command_isolation) {
            (1, None) => Self::default().command_isolation,
            (2 | VERSION, Some(isolation)) => isolation,
            _ => {
                return Err(format!(
                    "Versión o aislamiento de permisos del agente no compatible: {}",
                    document.version
                ));
            }
        };
        if document.full_access
            && (document.version != VERSION
                || command_isolation != CommandIsolation::HostUnrestricted
                || ALL_TOOLS
                    .iter()
                    .any(|tool| !document.permissions.allows(*tool)))
        {
            return Err(
                "El acceso completo requiere herramientas habilitadas y comandos sin aislamiento"
                    .into(),
            );
        }
        Ok(Self {
            permissions: document.permissions,
            command_isolation,
            full_access: document.full_access,
        })
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            permissions: self.permissions,
            command_isolation: Some(self.command_isolation),
            full_access: self.full_access,
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

    pub fn command_isolation(&self) -> CommandIsolation {
        self.command_isolation
    }

    pub fn full_access(&self) -> bool {
        self.full_access
    }

    /// Whether moving from `previous` to these settings takes away something
    /// a command already running relies on. Widening access never does: a
    /// server the agent started must survive the user granting more.
    pub fn revokes_commands_started_under(&self, previous: &Self) -> bool {
        !self.permissions().run_command
            || (previous.full_access() && !self.full_access())
            || (self.command_isolation() != previous.command_isolation()
                && self.command_isolation() != CommandIsolation::HostUnrestricted)
    }

    /// The host calls this only after the user confirms the full-access dialog.
    pub fn set_full_access(&mut self, enabled: bool) {
        self.full_access = enabled;
        if enabled {
            for tool in ALL_TOOLS {
                self.permissions.set(tool, true);
            }
            self.command_isolation = CommandIsolation::HostUnrestricted;
        }
    }

    pub fn set_command_isolation(&mut self, isolation: CommandIsolation) {
        self.command_isolation = isolation;
        if isolation != CommandIsolation::HostUnrestricted {
            self.full_access = false;
        }
    }

    pub fn enabled(&self, tool: LocalTool) -> bool {
        self.permissions.allows(tool)
    }

    pub fn set(&mut self, tool: LocalTool, enabled: bool) {
        self.permissions.set(tool, enabled);
        if !enabled {
            self.full_access = false;
        }
    }

    pub fn toggle(&mut self, tool: LocalTool) -> bool {
        let enabled = !self.enabled(tool);
        self.set(tool, enabled);
        enabled
    }

    pub fn mode_for(&self, tool: LocalTool) -> PermissionMode {
        if !self.enabled(tool)
            || tool == LocalTool::RunCommand && !self.command_isolation.is_available_on_host()
        {
            PermissionMode::Deny
        } else if matches!(tool, LocalTool::WriteFile | LocalTool::RunCommand) && !self.full_access
        {
            PermissionMode::Ask
        } else {
            PermissionMode::Allow
        }
    }

    fn persistent_modes(&self) -> [(&'static str, PermissionMode); 5] {
        let mutating = if self.enabled(LocalTool::RunCommand)
            && self.command_isolation.is_available_on_host()
        {
            if self.full_access {
                PermissionMode::Allow
            } else {
                PermissionMode::Ask
            }
        } else {
            PermissionMode::Deny
        };
        let poll = if mutating != PermissionMode::Deny {
            PermissionMode::Allow
        } else {
            PermissionMode::Deny
        };
        let resize =
            if self.command_isolation == CommandIsolation::HostUnrestricted && cfg!(windows) {
                mutating
            } else {
                PermissionMode::Deny
            };
        [
            ("start_command", mutating),
            ("poll_command", poll),
            ("write_stdin", mutating),
            ("terminate_command", mutating),
            ("resize_command", resize),
        ]
    }

    /// Deny unknown future tools by default. A disabled tool is also omitted
    /// from the model's advertised definitions by `LocalTools::with_permissions`.
    pub fn permission_policy(&self) -> PermissionPolicy {
        let policy = ALL_TOOLS
            .iter()
            .copied()
            .fold(
                PermissionPolicy::new().with_default_mode(PermissionMode::Deny),
                |policy, tool| policy.with_tool(tool.name(), self.mode_for(tool)),
            )
            .with_tool("apply_patch", self.mode_for(LocalTool::WriteFile))
            // Reads the turn's own catalog: the call it leads to is what asks.
            .with_tool(openide_harness::MCP_SEARCH_TOOL, PermissionMode::Allow);
        self.persistent_modes()
            .into_iter()
            .fold(policy, |policy, (name, mode)| policy.with_tool(name, mode))
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
pub(crate) fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(temporary, destination)
}

#[cfg(windows)]
pub(crate) fn replace_atomically(temporary: &Path, destination: &Path) -> io::Result<()> {
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
        assert_eq!(
            settings.command_isolation(),
            if cfg!(windows) {
                CommandIsolation::WorkspaceWriteOffline
            } else {
                CommandIsolation::HostUnrestricted
            }
        );
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
    fn persistent_command_modes_follow_run_permission_and_isolation() {
        let mut settings = AgentToolSettings::default();
        assert!(settings
            .persistent_modes()
            .iter()
            .all(|(_, mode)| *mode == PermissionMode::Deny));
        settings.set(LocalTool::RunCommand, true);
        settings.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        if CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
            assert_eq!(
                settings.persistent_modes(),
                [
                    ("start_command", PermissionMode::Ask),
                    ("poll_command", PermissionMode::Allow),
                    ("write_stdin", PermissionMode::Ask),
                    ("terminate_command", PermissionMode::Ask),
                    ("resize_command", PermissionMode::Deny),
                ]
            );
        } else {
            assert!(settings
                .persistent_modes()
                .iter()
                .all(|(_, mode)| *mode == PermissionMode::Deny));
        }
        settings.set_command_isolation(CommandIsolation::HostUnrestricted);
        assert_eq!(
            settings.persistent_modes(),
            [
                ("start_command", PermissionMode::Ask),
                ("poll_command", PermissionMode::Allow),
                ("write_stdin", PermissionMode::Ask),
                ("terminate_command", PermissionMode::Ask),
                (
                    "resize_command",
                    if cfg!(windows) {
                        PermissionMode::Ask
                    } else {
                        PermissionMode::Deny
                    },
                ),
            ]
        );
    }

    #[test]
    fn saves_and_replaces_preferences_without_losing_independent_choices() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = AgentToolSettings::default();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), settings);
        settings.set(LocalTool::RunCommand, true);
        settings.set(LocalTool::ReadFile, false);
        settings.set_command_isolation(CommandIsolation::HostUnrestricted);
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
        fs::write(
            &path,
            br#"{"version":2,"permissions":{},"command_isolation":"unexpected"}"#,
        )
        .unwrap();
        assert!(AgentToolSettings::load(root.path()).is_err());
        fs::write(&path, br#"{"version":2,"permissions":{}}"#).unwrap();
        assert!(AgentToolSettings::load(root.path()).is_err());
    }

    #[test]
    fn migrates_version_one_permissions_without_losing_command_policy() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(SETTINGS_FILE),
            br#"{"version":1,"permissions":{"run_command":true}}"#,
        )
        .unwrap();
        let loaded = AgentToolSettings::load(root.path()).unwrap();
        assert!(loaded.enabled(LocalTool::RunCommand));
        assert_eq!(
            loaded.command_isolation(),
            AgentToolSettings::default().command_isolation()
        );
        loaded.save(root.path()).unwrap();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), loaded);
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

    #[test]
    fn full_access_is_explicit_persistent_and_disabling_a_tool_revokes_it() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = AgentToolSettings::default();
        assert!(!settings.full_access());
        settings.set_full_access(true);
        assert!(settings.full_access());
        assert_eq!(
            settings.command_isolation(),
            CommandIsolation::HostUnrestricted
        );
        for tool in ALL_TOOLS {
            assert!(settings.enabled(tool));
            assert_eq!(settings.mode_for(tool), PermissionMode::Allow);
        }
        settings.save(root.path()).unwrap();
        assert_eq!(AgentToolSettings::load(root.path()).unwrap(), settings);
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(root.path().join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(document["version"], 3);
        assert_eq!(document["full_access"], true);

        settings.set(LocalTool::SearchFiles, false);
        assert!(!settings.full_access());
        assert_eq!(settings.mode_for(LocalTool::WriteFile), PermissionMode::Ask);
        assert_eq!(
            settings.mode_for(LocalTool::RunCommand),
            PermissionMode::Ask
        );
        settings.save(root.path()).unwrap();
        assert!(!AgentToolSettings::load(root.path()).unwrap().full_access());
    }

    #[test]
    fn version_two_never_migrates_to_full_access() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(SETTINGS_FILE),
            br#"{"version":2,"permissions":{"read_file":true,"list_files":true,"search_files":true,"write_file":true,"run_command":true},"command_isolation":"host_unrestricted"}"#,
        ).unwrap();
        let settings = AgentToolSettings::load(root.path()).unwrap();
        assert!(!settings.full_access());
        assert_eq!(settings.mode_for(LocalTool::WriteFile), PermissionMode::Ask);
        assert_eq!(
            settings.mode_for(LocalTool::RunCommand),
            PermissionMode::Ask
        );
        settings.save(root.path()).unwrap();
        assert!(!AgentToolSettings::load(root.path()).unwrap().full_access());
    }

    #[test]
    fn contradictory_or_legacy_full_access_documents_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = AgentToolSettings::default();
        settings.set_full_access(true);
        settings.save(root.path()).unwrap();
        let path = root.path().join(SETTINGS_FILE);
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for (field, value) in [
            ("version", serde_json::json!(2)),
            (
                "command_isolation",
                serde_json::json!("workspace_write_offline"),
            ),
        ] {
            let mut invalid = document.clone();
            invalid[field] = value;
            fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            assert!(AgentToolSettings::load(root.path()).is_err());
        }
        let mut invalid = document;
        invalid["permissions"]["run_command"] = serde_json::json!(false);
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(AgentToolSettings::load(root.path()).is_err());
    }

    #[test]
    fn sandbox_selection_revokes_full_access_and_persistent_commands_require_approval_again() {
        let mut settings = AgentToolSettings::default();
        settings.set_full_access(true);
        assert_eq!(
            settings.persistent_modes()[0],
            ("start_command", PermissionMode::Allow)
        );
        settings.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        assert!(!settings.full_access());
        settings.set_command_isolation(CommandIsolation::HostUnrestricted);
        assert!(!settings.full_access());
        assert_eq!(
            settings.mode_for(LocalTool::RunCommand),
            PermissionMode::Ask
        );
        assert_eq!(
            settings.persistent_modes()[0],
            ("start_command", PermissionMode::Ask)
        );
    }
    #[test]
    fn running_commands_survive_wider_access_and_stop_on_narrower() {
        let base = AgentToolSettings::default();
        let mut full = base;
        full.set_full_access(true);
        // Granting everything must not stop a server the agent already started.
        assert!(!full.revokes_commands_started_under(&base));
        // Taking it back does.
        let mut back = full;
        back.set_full_access(false);
        assert!(back.revokes_commands_started_under(&full));
        // Tighter containment does.
        let mut offline = base;
        offline.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        assert!(offline.revokes_commands_started_under(&base));
        // Unchanged settings that still allow commands revoke nothing.
        assert!(!full.revokes_commands_started_under(&full));
    }

}
