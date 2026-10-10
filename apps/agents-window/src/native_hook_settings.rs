//! User-authored command hooks for interactive native turns.
//!
//! This document is a trusted host preference. The model cannot write it via
//! tool calls. Every launch revalidates paths and computes the harness digest.

use crate::agent_tool_settings::replace_atomically;
use openide_harness::{CommandHookConfig, CommandHooks, HookPhase};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const FILE_NAME: &str = "native-command-hooks.json";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 64 * 1024;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeHookSettings {
    pub hooks: Vec<CommandHookConfig>,
}

/// Transient form state; only validated `NativeHookSettings` are persisted.
#[derive(Clone, Debug)]
pub(crate) struct HookUiState {
    pub hooks: Vec<CommandHookConfig>,
    pub load_error: Option<String>,
    pub editor_open: bool,
    pub selected_id: Option<String>,
    pub draft_id: String,
    pub draft_phase: HookPhase,
    pub draft_tool_name: String,
    pub draft_executable: String,
    pub draft_args: String,
    pub draft_cwd: String,
    pub draft_timeout_ms: String,
    pub draft_max_output_bytes: String,
}

impl Default for HookUiState {
    fn default() -> Self {
        Self {
            hooks: Vec::new(),
            load_error: None,
            editor_open: false,
            selected_id: None,
            draft_id: String::new(),
            draft_phase: HookPhase::PreToolUse,
            draft_tool_name: String::new(),
            draft_executable: String::new(),
            draft_args: "[]".into(),
            draft_cwd: String::new(),
            draft_timeout_ms: "5000".into(),
            draft_max_output_bytes: "4096".into(),
        }
    }
}

impl HookUiState {
    pub fn from_load(result: Result<NativeHookSettings, String>) -> Self {
        match result {
            Ok(settings) => Self {
                hooks: settings.hooks,
                ..Self::default()
            },
            Err(error) => Self {
                load_error: Some(error),
                ..Self::default()
            },
        }
    }

    pub fn clear_draft(&mut self) {
        let hooks = std::mem::take(&mut self.hooks);
        let load_error = self.load_error.take();
        *self = Self {
            hooks,
            load_error,
            ..Self::default()
        };
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    hooks: Vec<CommandHookConfig>,
}

impl NativeHookSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(FILE_NAME);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(format!(
                    "No se pudo abrir la configuración de hooks: {error}"
                ))
            }
        };
        let metadata = file.metadata().map_err(|error| {
            format!("No se pudo inspeccionar la configuración de hooks: {error}")
        })?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err("El archivo de hooks no es válido o excede el límite de tamaño".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudo leer la configuración de hooks: {error}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("El archivo de hooks excede el límite de tamaño".into());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("La configuración de hooks no es válida: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de configuración de hooks no compatible: {}",
                document.version
            ));
        }
        // Keep a syntactically valid document editable when an executable was
        // moved. Native dispatch still calls `registry` and fails closed.
        Ok(Self {
            hooks: document.hooks,
        })
    }

    pub fn registry(&self) -> Result<CommandHooks, String> {
        CommandHooks::new(self.hooks.clone())
            .map_err(|error| format!("Configuración de hooks no válida: {error}"))
    }

    /// The registry a run uses: the native hooks plus the Claude Code hooks
    /// the user enabled, if they still exist unchanged in their source file.
    /// A hook that cannot be validated fails the run rather than being skipped,
    /// because the user may rely on it to block something.
    pub fn registry_for(
        &self,
        root: &Path,
        workspace: &Path,
        source: Option<&Path>,
    ) -> Result<CommandHooks, String> {
        let registry = self.registry()?;
        let enabled = crate::compat_hook_settings::CompatHookSettings::load(root)?;
        // Nothing enabled means another tool's settings are not even read.
        let Some(source) = source.filter(|_| !enabled.is_empty()) else {
            return Ok(registry);
        };
        let discovered = crate::compat_hook_settings::discover(source)?;
        registry
            .with_compat(enabled.configs(&discovered, workspace))
            .map_err(|error| {
                format!("Hook de Claude Code no válido ({error}); desactivalo en Ajustes → Hooks")
            })
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        self.registry()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            hooks: self.hooks.clone(),
        })
        .map_err(|error| format!("No se pudo codificar la configuración de hooks: {error}"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("La configuración de hooks excede el límite de tamaño".into());
        }
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let temporary = root.join(format!(
            ".native-command-hooks-{}-{}.tmp",
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
            replace_atomically(&temporary, &root.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|error| format!("No se pudo guardar la configuración de hooks: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_harness::HookPhase;
    use std::path::PathBuf;

    fn hook(root: &Path) -> CommandHookConfig {
        let executable = root.join("hook-bin");
        fs::write(&executable, b"hook").unwrap();
        CommandHookConfig {
            id: "audit".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "write_file".into(),
            executable,
            args: vec!["--check".into()],
            cwd: PathBuf::from(root),
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
        }
    }

    #[test]
    fn settings_round_trip_and_corruption_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let settings = NativeHookSettings {
            hooks: vec![hook(root.path())],
        };
        settings.save(root.path()).unwrap();
        let loaded = NativeHookSettings::load(root.path()).unwrap();
        assert_eq!(
            settings.registry().unwrap().digest_hex().unwrap(),
            loaded.registry().unwrap().digest_hex().unwrap()
        );
        fs::write(root.path().join(FILE_NAME), b"{broken").unwrap();
        assert!(NativeHookSettings::load(root.path()).is_err());
    }

    #[test]
    fn rejects_relative_and_missing_executables() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = NativeHookSettings {
            hooks: vec![hook(root.path())],
        };
        settings.hooks[0].executable = PathBuf::from("relative-bin");
        assert!(settings.save(root.path()).is_err());
        settings.hooks[0].executable = root.path().join("missing");
        assert!(settings.save(root.path()).is_err());
        assert!(!root.path().join(FILE_NAME).exists());
    }
}
