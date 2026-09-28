//! Model and reasoning controls shared by the home and session composers.
//! Choices are stored separately from the visible tree so changing a dock
//! does not silently reset the configuration of the selected runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

const DEFAULT_SELECTION: &str = "default";
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ComposerChoice {
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ComposerPreferences {
    #[serde(default)]
    choices: BTreeMap<String, ComposerChoice>,
}

impl ComposerPreferences {
    pub fn load(root: &Path) -> Self {
        let path = root.join("composer-preferences.json");
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 64 * 1024) {
            return Self::default();
        }
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn choice(&self, backend: &str, native_model: &str) -> ComposerChoice {
        self.choices.get(backend).cloned().unwrap_or_else(|| {
            if backend == "codex" {
                codex_local_defaults()
            } else if backend == "native" {
                ComposerChoice {
                    model: native_model.to_owned(),
                    effort: DEFAULT_SELECTION.to_owned(),
                }
            } else {
                ComposerChoice::default()
            }
        })
    }

    pub fn set(&mut self, backend: &str, choice: ComposerChoice) {
        self.choices.insert(backend.to_owned(), choice);
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(root)?;
        let payload = serde_json::to_vec_pretty(self)?;
        let temp = root.join(format!(
            ".composer-preferences-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            use std::io::Write;
            file.write_all(&payload)?;
            file.sync_all()?;
            std::fs::rename(&temp, root.join("composer-preferences.json"))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".codex")))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Reads only the two top-level scalar defaults. The Codex CLI remains the
/// authority for model availability and can reject a stale local preference.
fn codex_local_defaults() -> ComposerChoice {
    let Some(path) = codex_home().map(|home| home.join("config.toml")) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    if metadata.len() > 256 * 1024 {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    }
    let Ok(source) = std::fs::read_to_string(path) else {
        return ComposerChoice {
            model: DEFAULT_SELECTION.to_owned(),
            effort: DEFAULT_SELECTION.to_owned(),
        };
    };
    let mut choice = ComposerChoice {
        model: DEFAULT_SELECTION.to_owned(),
        effort: DEFAULT_SELECTION.to_owned(),
    };
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.split_once('"'))
            .map(|(value, _)| value);
        let Some(value) = value.filter(|value| {
            !value.is_empty()
                && value.len() <= 100
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
        }) else {
            continue;
        };
        match key.trim() {
            "model" => choice.model = value.to_owned(),
            "model_reasoning_effort" => choice.effort = value.to_owned(),
            _ => {}
        }
    }
    if !openide_cli::codex_known_models()
        .iter()
        .any(|(model, _)| *model == choice.model)
    {
        // Custom Codex providers still work through the CLI's own config.
        // A selector override is only safe for catalogued public models.
        choice.model = DEFAULT_SELECTION.to_owned();
        choice.effort = DEFAULT_SELECTION.to_owned();
    }
    choice
}

pub(crate) fn model_options(backend: &str, selected: &str) -> Vec<(String, String)> {
    let mut options = if backend == "codex" {
        let mut options = vec![(DEFAULT_SELECTION.to_owned(), "Predeterminado".to_owned())];
        options.extend(
            openide_cli::codex_known_models()
                .iter()
                .map(|(id, label)| ((*id).to_owned(), (*label).to_owned())),
        );
        options
    } else if backend == "native" {
        vec![
            (selected.to_owned(), display_model(selected)),
            (
                "openide:configure-model".to_owned(),
                "Configurar modelo…".to_owned(),
            ),
        ]
    } else {
        Vec::new()
    };
    if !selected.is_empty() && !options.iter().any(|(id, _)| id == selected) {
        options.push((selected.to_owned(), display_model(selected)));
    }
    options
}

pub(crate) fn effort_options(backend: &str, model: &str) -> Vec<(String, String)> {
    let levels = if backend == "codex" {
        openide_cli::codex_supported_efforts(model)
    } else if backend == "native" {
        openide_harness::native_agent_supported_efforts(model)
    } else {
        None
    };
    let Some(levels) = levels else {
        return Vec::new();
    };
    let mut options = if backend == "native" && levels == ["none"] {
        Vec::new()
    } else {
        vec![(DEFAULT_SELECTION.to_owned(), "Automático".to_owned())]
    };
    options.extend(levels.iter().map(|level| {
        (
            (*level).to_owned(),
            match *level {
                "none" => "Ninguno",
                "minimal" => "Mínimo",
                "low" => "Bajo",
                "medium" => "Medio",
                "high" => "Alto",
                "xhigh" => "Muy alto",
                "max" => "Máximo",
                "ultra" => "Ultra",
                _ => level,
            }
            .to_owned(),
        )
    }));
    options
}

pub(crate) fn display_model(id: &str) -> String {
    if id == DEFAULT_SELECTION || id.is_empty() {
        return "Modelo del agente".to_owned();
    }
    openide_cli::codex_known_models()
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or_else(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_preserve_model_and_effort_per_backend() {
        let root = tempfile::tempdir().unwrap();
        let mut prefs = ComposerPreferences::default();
        prefs.set(
            "codex",
            ComposerChoice {
                model: "gpt-6-sol".into(),
                effort: "ultra".into(),
            },
        );
        prefs.save(root.path()).unwrap();
        prefs.set(
            "native",
            ComposerChoice {
                model: "gpt-4.1".into(),
                effort: "default".into(),
            },
        );
        prefs.save(root.path()).unwrap();
        let loaded = ComposerPreferences::load(root.path());
        assert_eq!(loaded.choice("codex", ""), prefs.choice("codex", ""));
        assert_eq!(loaded.choice("native", "gpt-4.1").model, "gpt-4.1");
    }

    #[test]
    fn unknown_selected_model_remains_visible() {
        let options = model_options("codex", "custom-model");
        assert!(options.iter().any(|(id, _)| id == "custom-model"));
    }
}
