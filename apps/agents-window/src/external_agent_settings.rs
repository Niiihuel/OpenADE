//! What OpenIDE exposes to external agent CLIs such as Claude Code or Codex.
//!
//! The JSON file is a user preference. It never relaxes the exec policy or the
//! native agent's tool permissions: a tool call from an external CLI is
//! resolved here first and still goes through those checks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use openide_harness::PermissionMode;
use serde::{Deserialize, Serialize};

use crate::agent_tool_settings::replace_atomically;

const SETTINGS_FILE: &str = "external-agent-settings.json";
const VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const ALL_TOOLS: [ExternalTool; 4] = [
    ExternalTool::GitReview,
    ExternalTool::Browser,
    ExternalTool::Conversations,
    ExternalTool::Subagents,
];

/// A group of OpenIDE capabilities an external CLI can be offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalTool {
    GitReview,
    Browser,
    Conversations,
    Subagents,
}

impl ExternalTool {
    pub const fn name(self) -> &'static str {
        match self {
            Self::GitReview => "git_review",
            Self::Browser => "browser",
            Self::Conversations => "conversations",
            Self::Subagents => "subagents",
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        let name = id.strip_prefix("external_tool:")?;
        ALL_TOOLS.into_iter().find(|tool| tool.name() == name)
    }

    /// Reading the project's diff exposes nothing the CLI cannot already read.
    #[cfg_attr(not(test), allow(dead_code))]
    const fn needs_approval(self) -> bool {
        !matches!(self, Self::GitReview)
    }
}

/// How approvals for OpenIDE tools follow the CLI that calls them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPermissionMode {
    /// A CLI started in accept-all mode gets OpenIDE tools without prompts.
    #[default]
    MirrorCli,
    /// OpenIDE asks regardless of the CLI's own mode.
    AlwaysAsk,
}

impl ExternalPermissionMode {
    pub const fn action_id(self) -> &'static str {
        match self {
            Self::MirrorCli => "external_permission:mirror_cli",
            Self::AlwaysAsk => "external_permission:always_ask",
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        [Self::MirrorCli, Self::AlwaysAsk]
            .into_iter()
            .find(|mode| mode.action_id() == id)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::MirrorCli => "Seguir a la CLI",
            Self::AlwaysAsk => "Preguntar siempre",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalAgentSettings {
    enabled: bool,
    git_review: bool,
    browser: bool,
    conversations: bool,
    subagents: bool,
    permission_mode: ExternalPermissionMode,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    settings: ExternalAgentSettings,
}

impl Default for ExternalAgentSettings {
    /// Everything stays closed until the user opens the bridge. Conversations
    /// and delegation stay opt-in even then: both cross session boundaries.
    fn default() -> Self {
        Self {
            enabled: false,
            git_review: true,
            browser: true,
            conversations: false,
            subagents: false,
            permission_mode: ExternalPermissionMode::default(),
        }
    }
}

impl ExternalAgentSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let file = match File::open(root.join(SETTINGS_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(format!(
                    "No se pudieron abrir los ajustes de CLIs externas: {error}"
                ))
            }
        };
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudieron leer los ajustes de CLIs externas: {error}"))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err("El archivo de ajustes de CLIs externas es demasiado grande".to_owned());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Los ajustes de CLIs externas no son válidos: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de ajustes de CLIs externas no compatible: {}",
                document.version
            ));
        }
        Ok(document.settings)
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            settings: *self,
        })
        .map_err(|error| format!("No se pudieron codificar los ajustes de CLIs externas: {error}"))?;
        let temporary = root.join(format!(
            ".external-agent-settings-{}-{}.tmp",
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
        result
            .map_err(|error| format!("No se pudieron guardar los ajustes de CLIs externas: {error}"))
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn toggle_enabled(&mut self) -> bool {
        self.enabled = !self.enabled;
        self.enabled
    }

    pub fn allows(&self, tool: ExternalTool) -> bool {
        match tool {
            ExternalTool::GitReview => self.git_review,
            ExternalTool::Browser => self.browser,
            ExternalTool::Conversations => self.conversations,
            ExternalTool::Subagents => self.subagents,
        }
    }

    pub fn toggle(&mut self, tool: ExternalTool) -> bool {
        let slot = match tool {
            ExternalTool::GitReview => &mut self.git_review,
            ExternalTool::Browser => &mut self.browser,
            ExternalTool::Conversations => &mut self.conversations,
            ExternalTool::Subagents => &mut self.subagents,
        };
        *slot = !*slot;
        *slot
    }

    pub fn permission_mode(&self) -> ExternalPermissionMode {
        self.permission_mode
    }

    pub fn set_permission_mode(&mut self, mode: ExternalPermissionMode) {
        self.permission_mode = mode;
    }

    /// Decision for one call from an external CLI. `cli_accepts_all` is what
    /// OpenIDE knows about the CLI it launched, never what the CLI claims.
    /// `Allow` here still passes through the exec policy's deny rules.
    // shortcut: no caller outside tests, remove the allow when the bridge server lands.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn mode_for(&self, tool: ExternalTool, cli_accepts_all: bool) -> PermissionMode {
        if !self.enabled || !self.allows(tool) {
            PermissionMode::Deny
        } else if !tool.needs_approval()
            || cli_accepts_all && self.permission_mode == ExternalPermissionMode::MirrorCli
        {
            PermissionMode::Allow
        } else {
            PermissionMode::Ask
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> ExternalAgentSettings {
        let mut settings = ExternalAgentSettings::default();
        settings.toggle_enabled();
        settings
    }

    #[test]
    fn closed_bridge_denies_every_tool_even_for_an_accept_all_cli() {
        let settings = ExternalAgentSettings::default();
        for tool in ALL_TOOLS {
            assert_eq!(settings.mode_for(tool, true), PermissionMode::Deny);
        }
    }

    #[test]
    fn cross_session_tools_stay_denied_until_opted_in() {
        let mut settings = open();
        for tool in [ExternalTool::Conversations, ExternalTool::Subagents] {
            assert_eq!(settings.mode_for(tool, true), PermissionMode::Deny);
            assert!(settings.toggle(tool));
            assert_eq!(settings.mode_for(tool, true), PermissionMode::Allow);
            assert_eq!(settings.mode_for(tool, false), PermissionMode::Ask);
        }
    }

    #[test]
    fn mirror_follows_the_cli_and_always_ask_does_not() {
        let mut settings = open();
        assert_eq!(
            settings.mode_for(ExternalTool::Browser, true),
            PermissionMode::Allow
        );
        assert_eq!(
            settings.mode_for(ExternalTool::Browser, false),
            PermissionMode::Ask
        );
        settings.set_permission_mode(ExternalPermissionMode::AlwaysAsk);
        assert_eq!(
            settings.mode_for(ExternalTool::Browser, true),
            PermissionMode::Ask
        );
        // Reading the diff never prompts; it is still denied when switched off.
        assert_eq!(
            settings.mode_for(ExternalTool::GitReview, false),
            PermissionMode::Allow
        );
        assert!(!settings.toggle(ExternalTool::GitReview));
        assert_eq!(
            settings.mode_for(ExternalTool::GitReview, true),
            PermissionMode::Deny
        );
    }

    #[test]
    fn settings_round_trip_and_reject_unknown_documents() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            ExternalAgentSettings::load(root.path()).unwrap(),
            ExternalAgentSettings::default()
        );
        let mut settings = open();
        settings.toggle(ExternalTool::Conversations);
        settings.set_permission_mode(ExternalPermissionMode::AlwaysAsk);
        settings.save(root.path()).unwrap();
        assert_eq!(ExternalAgentSettings::load(root.path()).unwrap(), settings);

        let path = root.path().join(SETTINGS_FILE);
        let saved = fs::read_to_string(&path).unwrap();
        fs::write(&path, saved.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(ExternalAgentSettings::load(root.path())
            .unwrap_err()
            .contains("no compatible"));
        fs::write(&path, saved.replace("\"browser\"", "\"shell\"")).unwrap();
        assert!(ExternalAgentSettings::load(root.path()).is_err());
    }

    #[test]
    fn action_ids_resolve_only_known_values() {
        for tool in ALL_TOOLS {
            let id = format!("external_tool:{}", tool.name());
            assert_eq!(ExternalTool::from_action_id(&id), Some(tool));
        }
        assert_eq!(ExternalTool::from_action_id("external_tool:shell"), None);
        assert_eq!(
            ExternalPermissionMode::from_action_id("external_permission:always_ask"),
            Some(ExternalPermissionMode::AlwaysAsk)
        );
        assert_eq!(
            ExternalPermissionMode::from_action_id("external_permission:"),
            None
        );
    }
}
