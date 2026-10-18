//! Small, persistent preferences that have a native effect in this window.
//!
//! This file owns the JSON format and validation. It deliberately does not
//! depend on SettingsPage or the UI tree so the app can load it before either
//! view is built. Provider credentials and permissions live elsewhere.

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use openide_harness::HostedWebSearchMode;
use serde::{Deserialize, Serialize};

use crate::image_runtime::{DEFAULT_QUALITY, IMAGE_MODELS};
use crate::keymap::KeymapProfile;

const FILE_NAME: &str = "settings-preferences.json";
// Thirty complete core themes can exceed 64 KiB with long escaped names and
// alpha colors. Bound the whole file with room for the other preferences.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const CURRENT_VERSION: u64 = 1;
const DEFAULT_SETTINGS_PAGE: &str = "settings_page:general";
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A small set of shells with known launch arguments, plus an explicit local
/// executable. `System` preserves the current OPENIDE_TERMINAL_SHELL behavior.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub(crate) enum TerminalShell {
    #[default]
    System,
    PowerShell,
    PowerShellCore,
    CommandPrompt,
    Bash,
    Zsh,
    Fish,
    Custom(PathBuf),
}

impl TerminalShell {
    /// Returns a validated executable and its fixed arguments. `None` means
    /// TerminalSession::spawn should choose the current system/default shell.
    pub fn command(&self) -> Result<Option<(OsString, Vec<OsString>)>, String> {
        let (program, args): (&str, &[&str]) = match self {
            Self::System => return Ok(None),
            Self::PowerShell => ("powershell.exe", &["-NoLogo"]),
            Self::PowerShellCore => (
                if cfg!(windows) { "pwsh.exe" } else { "pwsh" },
                &["-NoLogo"],
            ),
            Self::CommandPrompt => ("cmd.exe", &[]),
            Self::Bash => (if cfg!(windows) { "bash.exe" } else { "bash" }, &[]),
            Self::Zsh => (if cfg!(windows) { "zsh.exe" } else { "zsh" }, &[]),
            Self::Fish => (if cfg!(windows) { "fish.exe" } else { "fish" }, &[]),
            Self::Custom(path) => {
                let executable = validate_executable(path)?;
                return Ok(Some((executable.into_os_string(), Vec::new())));
            }
        };
        let executable = find_on_path(program)?;
        Ok(Some((
            executable.into_os_string(),
            args.iter().map(OsString::from).collect(),
        )))
    }
}

/// Only account identity and OpenIDE consent are persisted, never credentials.
/// Automatic preserves discovery for installations predating account selection.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", content = "login", rename_all = "snake_case")]
pub(crate) enum GithubAccountPreference {
    #[default]
    Automatic,
    Selected(String),
    Disconnected,
}

impl GithubAccountPreference {
    pub fn selected(login: &str) -> Result<Self, String> {
        if login.is_empty()
            || login.len() > 39
            || login.starts_with('-')
            || login.ends_with('-')
            || !login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("La cuenta de GitHub no es válida.".into());
        }
        Ok(Self::Selected(login.to_owned()))
    }

    fn valid(&self) -> bool {
        match self {
            Self::Selected(login) => Self::selected(login).is_ok(),
            Self::Automatic | Self::Disconnected => true,
        }
    }
}

/// What became of a guided tour the last time it was offered or shown.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct TourRecord {
    /// The tour's version then. A newer version is offered again.
    pub version: u32,
    /// Walked to its last step, rather than declined or closed on the way.
    pub completed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SettingsPreferences {
    pub appearance: crate::appearance_preferences::AppearancePreferences,
    pub reduced_motion: bool,
    /// Application shortcut preset. This does not enable modal text editing.
    pub keymap_profile: KeymapProfile,
    /// Modal editing is independent from the workbench shortcut preset.
    pub vim_mode: bool,
    /// Continue a turn paused on a provider limit when the limit renews.
    pub auto_resume_on_limit: bool,
    /// Ask before summarising a conversation that does not fit a newly
    /// chosen model, instead of doing it and saying so.
    pub ask_before_summary_on_switch: bool,
    /// `provider:model` routes offered, in this order, when a pause has no
    /// end in sight.
    pub fallback_models: Vec<String>,
    /// Move a paused turn to the first available fallback without asking.
    pub auto_switch_fallback: bool,
    pub terminal_shell: TerminalShell,
    pub reopen_last_workspace: bool,
    /// Check for new versions every few hours and download them in the background.
    pub auto_update: bool,
    pub last_workspace: Option<PathBuf>,
    /// Stores a `settings_page:<slug>` action ID. The app verifies that the
    /// page still exists with `SettingsPage::from_action_id` on startup.
    pub last_settings_page: String,
    /// Hosted Responses search is an explicit opt-in because it may contact
    /// external sources and its exact mode is bound to native resume identity.
    pub hosted_web_search_mode: Option<HostedWebSearchMode>,
    pub github_account: GithubAccountPreference,
    /// Guided tours already offered, by tour ID.
    pub tours: std::collections::BTreeMap<String, TourRecord>,
    /// Where a click on a link of the conversation opens.
    pub chat_links: crate::chat_links::ChatLinkOpening,
    /// When the preview of a server the agent starts opens.
    pub preview_opening: crate::preview::PreviewOpening,
    /// Whether agent tools may automate the embedded browser. Manual browsing
    /// is independent; existing clients also observe this revocable setting.
    pub browser_automation_enabled: bool,
    /// The model and quality `generate_image` uses, as the user chose them.
    /// `image_runtime::image_choice` says what a call sends for the pair.
    pub image_model: String,
    pub image_quality: String,
}

impl Default for SettingsPreferences {
    fn default() -> Self {
        Self {
            appearance: Default::default(),
            reduced_motion: env::var("OPENIDE_REDUCED_MOTION").is_ok_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                )
            }),
            keymap_profile: KeymapProfile::default(),
            vim_mode: false,
            auto_resume_on_limit: true,
            ask_before_summary_on_switch: false,
            fallback_models: Vec::new(),
            auto_switch_fallback: false,
            terminal_shell: TerminalShell::System,
            reopen_last_workspace: false,
            auto_update: true,
            last_workspace: None,
            last_settings_page: DEFAULT_SETTINGS_PAGE.to_owned(),
            hosted_web_search_mode: None,
            github_account: GithubAccountPreference::Automatic,
            tours: Default::default(),
            chat_links: Default::default(),
            browser_automation_enabled: true,
            preview_opening: Default::default(),
            image_model: IMAGE_MODELS[0].id.to_owned(),
            image_quality: DEFAULT_QUALITY.to_owned(),
        }
    }
}

impl SettingsPreferences {
    /// Missing, malformed, future-version, and oversized files use defaults.
    /// Each valid v1 field is recovered independently from malformed siblings.
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE_NAME);
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_FILE_BYTES) {
            return Self::default();
        }
        let Some(value) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            return Self::default();
        };
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(CURRENT_VERSION) {
            return Self::default();
        }
        let mut preferences = Self::default();
        if let Some(reduced_motion) = value
            .get("reduced_motion")
            .and_then(serde_json::Value::as_bool)
        {
            preferences.reduced_motion = reduced_motion;
        }
        preferences.appearance = crate::appearance_preferences::AppearancePreferences::load_value(
            value.get("appearance"),
            preferences.reduced_motion,
        );
        if let Some(profile) = value
            .get("keymap_profile")
            .and_then(serde_json::Value::as_str)
            .and_then(KeymapProfile::from_id)
        {
            preferences.keymap_profile = profile;
        }
        if let Some(enabled) = value.get("vim_mode").and_then(serde_json::Value::as_bool) {
            preferences.vim_mode = enabled;
        }
        if let Some(shell) = value
            .get("terminal_shell")
            .and_then(|value| serde_json::from_value::<TerminalShell>(value.clone()).ok())
        {
            // Keep the configured choice even if a shell was uninstalled since
            // the last run; command() then reports the precise launch error.
            if !matches!(&shell, TerminalShell::Custom(path) if !valid_custom_path_syntax(path)) {
                preferences.terminal_shell = shell;
            }
        }
        if let Some(enabled) = value
            .get("auto_resume_on_limit")
            .and_then(serde_json::Value::as_bool)
        {
            preferences.auto_resume_on_limit = enabled;
        }
        if let Some(enabled) = value
            .get("ask_before_summary_on_switch")
            .and_then(serde_json::Value::as_bool)
        {
            preferences.ask_before_summary_on_switch = enabled;
        }
        if let Some(models) = value.get("fallback_models").and_then(serde_json::Value::as_array) {
            preferences.fallback_models = models
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|route| route.len() <= 640 && route.contains(':'))
                .take(MAX_FALLBACK_MODELS)
                .map(str::to_owned)
                .collect();
        }
        if let Some(enabled) = value
            .get("auto_switch_fallback")
            .and_then(serde_json::Value::as_bool)
        {
            preferences.auto_switch_fallback = enabled;
        }
        if let Some(enabled) = value.get("auto_update").and_then(serde_json::Value::as_bool) {
            preferences.auto_update = enabled;
        }
        if let Some(reopen) = value
            .get("reopen_last_workspace")
            .and_then(serde_json::Value::as_bool)
        {
            preferences.reopen_last_workspace = reopen;
        }
        preferences.last_workspace = value
            .get("last_workspace")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_dir());
        if let Some(page) = value
            .get("last_settings_page")
            .and_then(serde_json::Value::as_str)
            .filter(|page| valid_page_id(page))
        {
            preferences.last_settings_page = page.to_owned();
        }
        preferences.hosted_web_search_mode = value
            .get("hosted_web_search_mode")
            .and_then(|mode| serde_json::from_value(mode.clone()).ok());
        if let Some(account) = value.get("github_account") {
            // A damaged explicit choice must never reconnect an arbitrary
            // system account. An absent field alone retains legacy discovery.
            preferences.github_account =
                serde_json::from_value::<GithubAccountPreference>(account.clone())
                    .ok()
                    .filter(GithubAccountPreference::valid)
                    .unwrap_or(GithubAccountPreference::Disconnected);
        }
        if let Some(tours) = value.get("tours").and_then(serde_json::Value::as_object) {
            // A record that does not parse is dropped: its tour is offered again.
            preferences.tours = tours
                .iter()
                .filter(|(id, _)| id.len() <= 32)
                .filter_map(|(id, record)| {
                    Some((id.clone(), serde_json::from_value(record.clone()).ok()?))
                })
                .take(16)
                .collect();
        }
        if let Some(opening) = value
            .get("chat_links")
            .and_then(|opening| serde_json::from_value(opening.clone()).ok())
        {
            preferences.chat_links = opening;
        }
        if let Some(opening) = value
            .get("preview_opening")
            .and_then(|opening| serde_json::from_value(opening.clone()).ok())
        {
            preferences.preview_opening = opening;
        }
        if let Some(enabled) = value.get("browser_automation_enabled") {
            preferences.browser_automation_enabled = enabled.as_bool().unwrap_or(false);
        }
        // A model or level this build does not list keeps the default.
        let known = |key: &str, listed: &dyn Fn(&str) -> bool| {
            value.get(key)?.as_str().filter(|id| listed(id)).map(str::to_owned)
        };
        if let Some(model) = known("image_model", &|id| IMAGE_MODELS.iter().any(|model| model.id == id)) {
            preferences.image_model = model;
        }
        if let Some(quality) = known("image_quality", &|id| {
            IMAGE_MODELS.iter().any(|model| model.qualities.contains(&id))
        }) {
            preferences.image_quality = quality;
        }
        preferences
    }

    /// Only opt-in reopening changes the app's initial workspace.
    pub fn startup_workspace(&self) -> Option<&Path> {
        if self.reopen_last_workspace {
            self.last_workspace.as_deref()
        } else {
            None
        }
    }

    /// Record a directory selected by the user. The stored path is absolute
    /// and canonical, so launching from another current directory is safe.
    pub fn set_last_workspace(&mut self, workspace: &Path) -> Result<(), String> {
        let path = validate_workspace(workspace)?;
        self.last_workspace = Some(path);
        Ok(())
    }

    /// Reject an unavailable shell before changing the in-memory selection.
    /// A custom choice must be an absolute path to an executable file.
    pub fn set_terminal_shell(&mut self, shell: TerminalShell) -> Result<(), String> {
        shell.command()?;
        self.terminal_shell = shell;
        Ok(())
    }

    pub fn set_last_settings_page(&mut self, action_id: &str) -> Result<(), String> {
        if !valid_page_id(action_id) {
            return Err("La sección de Settings no es válida".to_owned());
        }
        self.last_settings_page = action_id.to_owned();
        Ok(())
    }

    /// Resolve the selected shell immediately before creating a PTY. The
    /// system choice delegates to TerminalSession::spawn.
    pub fn terminal_command(&self) -> Result<Option<(OsString, Vec<OsString>)>, String> {
        self.terminal_shell.command()
    }

    /// Write to a unique temporary file, fsync it, then atomically replace the
    /// previous JSON (including on Windows). A failed write leaves it intact.
    pub fn save(&self, root: &Path) -> Result<(), String> {
        self.appearance.validate()?;
        if !self.github_account.valid() {
            return Err("La cuenta de GitHub no es válida.".into());
        }
        if matches!(&self.terminal_shell, TerminalShell::Custom(path) if !valid_custom_path_syntax(path))
        {
            return Err("La ruta del shell debe ser absoluta y válida".to_owned());
        }
        if !valid_page_id(&self.last_settings_page) {
            return Err("La sección de Settings no es válida".to_owned());
        }
        let workspace_text = self
            .last_workspace
            .as_deref()
            .map(|path| {
                if !path.is_absolute() {
                    return Err("La ruta del proyecto debe ser absoluta".to_owned());
                }
                path.to_str()
                    .ok_or_else(|| "La ruta del proyecto no se puede guardar en JSON".to_owned())
            })
            .transpose()?;
        fs::create_dir_all(root)
            .map_err(|error| format!("No se pudo crear la carpeta de configuración: {error}"))?;
        let payload = serde_json::to_vec_pretty(&serde_json::json!({
            "version": CURRENT_VERSION,
            "appearance": self.appearance,
            "reduced_motion": self.reduced_motion,
            "keymap_profile": self.keymap_profile.id(),
            "vim_mode": self.vim_mode,
            "auto_resume_on_limit": self.auto_resume_on_limit,
            "ask_before_summary_on_switch": self.ask_before_summary_on_switch,
            "fallback_models": self.fallback_models,
            "auto_switch_fallback": self.auto_switch_fallback,
            "terminal_shell": self.terminal_shell,
            "reopen_last_workspace": self.reopen_last_workspace,
            "auto_update": self.auto_update,
            "last_workspace": workspace_text,
            "last_settings_page": self.last_settings_page,
            "hosted_web_search_mode": self.hosted_web_search_mode,
            "github_account": self.github_account,
            "tours": self.tours,
            "chat_links": self.chat_links,
            "preview_opening": self.preview_opening,
            "browser_automation_enabled": self.browser_automation_enabled,
            "image_model": self.image_model,
            "image_quality": self.image_quality,
        }))
        .map_err(|error| format!("No se pudieron serializar las preferencias: {error}"))?;
        if payload.len() as u64 > MAX_FILE_BYTES {
            return Err("Las preferencias superan el límite de 512 KiB.".into());
        }
        let temp = root.join(format!(
            ".settings-preferences-{}-{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&payload)?;
            file.sync_all()?;
            drop(file);
            replace_atomically(&temp, &root.join(FILE_NAME))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error| format!("No se pudieron guardar las preferencias: {error}"))
    }
}

fn valid_page_id(page: &str) -> bool {
    let Some(slug) = page.strip_prefix("settings_page:") else {
        return false;
    };
    !slug.is_empty()
        && slug.len() <= 48
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_workspace(workspace: &Path) -> Result<PathBuf, String> {
    let path = workspace
        .canonicalize()
        .map_err(|error| format!("No se pudo abrir el último proyecto: {error}"))?;
    if !path.is_dir() {
        return Err("El último proyecto debe ser una carpeta".to_owned());
    }
    Ok(user_facing_path(path))
}

#[cfg(not(windows))]
fn user_facing_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(windows)]
fn user_facing_path(path: PathBuf) -> PathBuf {
    // canonicalize() returns Windows verbatim paths. They work for file I/O
    // but make the workspace field and CLI arguments unnecessarily awkward.
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(network) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{network}"));
    }
    if let Some(local) = text.strip_prefix(r"\\?\") {
        if local.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(local);
        }
    }
    path
}

fn valid_custom_path_syntax(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| {
            !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
        })
}

fn validate_executable(path: &Path) -> Result<PathBuf, String> {
    if !valid_custom_path_syntax(path) {
        return Err(
            "La ruta del shell debe ser absoluta y no contener caracteres de control".to_owned(),
        );
    }
    let executable = path
        .canonicalize()
        .map_err(|error| format!("No se pudo encontrar el shell: {error}"))?;
    if !executable.is_file() || !is_executable(&executable) {
        return Err("El shell configurado no es un archivo ejecutable".to_owned());
    }
    Ok(executable)
}

fn find_on_path(program: &str) -> Result<PathBuf, String> {
    let Some(path) = env::var_os("PATH") else {
        return Err(format!("No se encontró {program} en PATH"));
    };
    for directory in env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() && is_executable(&candidate) {
            if let Ok(absolute) = candidate.canonicalize() {
                return Ok(absolute);
            }
        }
    }
    Err(format!("No se encontró {program} en PATH"))
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
        })
}

#[cfg(not(windows))]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
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
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let destination_wide = wide(destination);
    let temporary_wide = wide(temporary);
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

    #[test]
    fn browser_automation_reopens_disabled_and_invalid_explicit_values_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        assert!(SettingsPreferences::load(root.path()).browser_automation_enabled);
        let prefs = SettingsPreferences { browser_automation_enabled: false, ..Default::default() };
        prefs.save(root.path()).unwrap();
        assert_eq!(SettingsPreferences::load(root.path()), prefs);
        for invalid in [serde_json::json!(null), serde_json::json!("true"), serde_json::json!(1)] {
            fs::write(root.path().join(FILE_NAME), serde_json::to_vec(&serde_json::json!({
                "version": CURRENT_VERSION, "browser_automation_enabled": invalid, "vim_mode": true,
            })).unwrap()).unwrap();
            let loaded = SettingsPreferences::load(root.path());
            assert!(!loaded.browser_automation_enabled);
            assert!(loaded.vim_mode);
        }
    }

    #[test]
    fn thirty_complete_custom_themes_round_trip_without_exceeding_file_bound() {
        use crate::custom_theme::{CustomTheme, ThemeAppearance, MAX_CUSTOM_THEMES};
        let root = tempfile::tempdir().unwrap();
        let mut preferences = SettingsPreferences::default();
        preferences.appearance.custom_themes = (0..MAX_CUSTOM_THEMES)
            .map(|index| {
                // Valid maximum-length metadata and alpha colors cover the
                // former 64 KiB bound, not only the shortest possible themes.
                let mut theme = CustomTheme::new(&format!("{}{index:02}", "\"".repeat(118)), ThemeAppearance::Dark);
                theme.id = format!("theme-{index:02}-{}", "x".repeat(71));
                for value in theme.colors.values_mut() { *value = "#12345678".into(); }
                theme
            })
            .collect();
        preferences.appearance.active_custom_theme = Some(preferences.appearance.custom_themes[0].id.clone());
        preferences.save(root.path()).unwrap();
        let length = fs::metadata(root.path().join(FILE_NAME)).unwrap().len();
        assert!(length > 64 * 1024, "the fixture must cover the previous read limit: {length}");
        assert!(length <= MAX_FILE_BYTES);
        assert_eq!(SettingsPreferences::load(root.path()), preferences);
    }

    #[test]
    fn appearance_round_trips_and_invalid_fields_preserve_their_valid_siblings() {
        use crate::appearance_preferences::{AppearancePreferences, MotionPreference, ToolCallDensity};
        let root = tempfile::tempdir().unwrap();
        let preferences = SettingsPreferences {
            appearance: AppearancePreferences {
                tool_call_density: ToolCallDensity::Detailed,
                code_word_wrap: true,
                themed_diff_backgrounds: false,
                hue_degrees: 215,
                tint_intensity: 640,
                reduce_transparency: true,
                ui_font_size: 19,
                code_font_size: 17,
                ui_font_family: "Ubuntu".into(),
                code_font_family: "Hack".into(),
                reduce_motion: MotionPreference::Never,
                hide_email: true,
                ..Default::default()
            },
            ..Default::default()
        };
        preferences.save(root.path()).unwrap();
        assert_eq!(SettingsPreferences::load(root.path()), preferences);

        let mut value = serde_json::to_value(&preferences.appearance).unwrap();
        value["ui_font_size"] = serde_json::json!(100);
        value["code_font_family"] = serde_json::json!("bad\nfont");
        value["tool_call_density"] = serde_json::json!("future_density");
        fs::write(root.path().join(FILE_NAME), serde_json::to_vec(&serde_json::json!({
            "version": CURRENT_VERSION, "appearance": value, "vim_mode": true,
        })).unwrap()).unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert_eq!(loaded.appearance.ui_font_size, AppearancePreferences::default().ui_font_size);
        assert_eq!(loaded.appearance.code_font_family, AppearancePreferences::default().code_font_family);
        assert_eq!(loaded.appearance.tool_call_density, ToolCallDensity::Grouped);
        assert_eq!(loaded.appearance.hue_degrees, 215);
        assert_eq!(loaded.appearance.reduce_motion, MotionPreference::Never);
        assert!(loaded.vim_mode);

        let before = fs::read(root.path().join(FILE_NAME)).unwrap();
        let mut invalid = preferences;
        invalid.appearance.tint_intensity = 1001;
        assert!(invalid.save(root.path()).is_err());
        assert_eq!(fs::read(root.path().join(FILE_NAME)).unwrap(), before);
    }

    #[test]
    fn github_choice_round_trips_and_malformed_choice_never_discovers_another_account() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            SettingsPreferences::load(root.path()).github_account,
            GithubAccountPreference::Automatic
        );
        for github_account in [
            GithubAccountPreference::Selected("octocat".into()),
            GithubAccountPreference::Disconnected,
        ] {
            let preferences = SettingsPreferences {
                github_account: github_account.clone(),
                ..SettingsPreferences::default()
            };
            preferences.save(root.path()).unwrap();
            assert_eq!(
                SettingsPreferences::load(root.path()).github_account,
                github_account
            );
        }
        for invalid in [
            serde_json::json!({"mode":"selected", "login":"../escape"}),
            serde_json::json!("invalid"),
            serde_json::Value::Null,
        ] {
            fs::write(
                root.path().join(FILE_NAME),
                serde_json::to_vec(&serde_json::json!({
                    "version": CURRENT_VERSION, "github_account": invalid, "vim_mode": true
                }))
                .unwrap(),
            )
            .unwrap();
            let loaded = SettingsPreferences::load(root.path());
            assert_eq!(loaded.github_account, GithubAccountPreference::Disconnected);
            assert!(loaded.vim_mode);
        }
        let invalid = SettingsPreferences {
            github_account: GithubAccountPreference::Selected("../escape".into()),
            ..SettingsPreferences::default()
        };
        assert!(invalid.save(root.path()).is_err());
    }

    #[test]
    fn keyboard_profiles_round_trip_as_stable_ids() {
        let root = tempfile::tempdir().unwrap();
        for profile in KeymapProfile::ALL.iter().copied() {
            let preferences = SettingsPreferences {
                keymap_profile: profile,
                vim_mode: true,
                reduced_motion: true,
                reopen_last_workspace: true,
                ..SettingsPreferences::default()
            };
            preferences.save(root.path()).unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&fs::read(root.path().join(FILE_NAME)).unwrap()).unwrap();
            assert_eq!(json["keymap_profile"], profile.id());
            assert_eq!(json["vim_mode"], true);
            assert_eq!(SettingsPreferences::load(root.path()), preferences);
        }
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn vim_recovers_independently_and_is_disabled_for_existing_settings() {
        let root = tempfile::tempdir().unwrap();
        for value in [
            None,
            Some(serde_json::json!("true")),
            Some(serde_json::json!(1)),
        ] {
            let mut json = serde_json::json!({"version": CURRENT_VERSION, "keymap_profile": "zed"});
            if let Some(value) = value {
                json["vim_mode"] = value;
            }
            fs::write(
                root.path().join(FILE_NAME),
                serde_json::to_vec(&json).unwrap(),
            )
            .unwrap();
            let loaded = SettingsPreferences::load(root.path());
            assert_eq!(loaded.keymap_profile, KeymapProfile::Zed);
            assert!(!loaded.vim_mode);
        }
        fs::write(
            root.path().join(FILE_NAME),
            br#"{"version":1,"keymap_profile":false,"vim_mode":true}"#,
        )
        .unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert!(loaded.vim_mode);
        assert_eq!(loaded.keymap_profile, KeymapProfile::default());
    }

    #[test]
    fn missing_or_invalid_keyboard_profile_preserves_other_preferences() {
        let root = tempfile::tempdir().unwrap();
        for value in [
            None,
            Some(serde_json::json!("future-profile")),
            Some(serde_json::json!(false)),
            Some(serde_json::json!({"id": "zed"})),
        ] {
            let mut json = serde_json::json!({
                "version": CURRENT_VERSION,
                "reduced_motion": true,
                "reopen_last_workspace": true,
                "last_settings_page": "settings_page:shortcuts"
            });
            if let Some(value) = value {
                json["keymap_profile"] = value;
            }
            fs::write(
                root.path().join(FILE_NAME),
                serde_json::to_vec(&json).unwrap(),
            )
            .unwrap();
            let preferences = SettingsPreferences::load(root.path());
            assert_eq!(preferences.keymap_profile, KeymapProfile::default());
            assert!(preferences.reduced_motion && preferences.reopen_last_workspace);
            assert_eq!(preferences.last_settings_page, "settings_page:shortcuts");
        }
        for profile in KeymapProfile::ALL.iter().copied() {
            fs::write(
                root.path().join(FILE_NAME),
                serde_json::to_vec(&serde_json::json!({
                    "version": CURRENT_VERSION,
                    "keymap_profile": profile.id(),
                    "terminal_shell": {"kind": "unknown"},
                    "last_settings_page": 42
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                SettingsPreferences::load(root.path()).keymap_profile,
                profile
            );
        }
    }

    #[test]
    fn round_trip_and_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let mut preferences = SettingsPreferences {
            reduced_motion: true,
            reopen_last_workspace: true,
            ..SettingsPreferences::default()
        };
        preferences.set_last_workspace(project.path()).unwrap();
        preferences.hosted_web_search_mode = Some(HostedWebSearchMode::Indexed);
        preferences.tours.insert("agents".into(), TourRecord { version: 2, completed: true });
        preferences.image_model = "gpt-image-2.5-flare".into();
        preferences.image_quality = "max".into();
        preferences
            .set_last_settings_page("settings_page:providers")
            .unwrap();
        preferences.save(root.path()).unwrap();
        assert_eq!(SettingsPreferences::load(root.path()), preferences);
        assert_eq!(
            preferences.startup_workspace(),
            preferences.last_workspace.as_deref()
        );

        preferences.reduced_motion = false;
        preferences.save(root.path()).unwrap();
        assert_eq!(SettingsPreferences::load(root.path()), preferences);
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            1,
            "atomic replacement must not leave a temporary file"
        );
    }

    #[test]
    fn an_image_model_or_quality_this_build_does_not_list_keeps_the_default() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(FILE_NAME),
            r#"{"version":1,"vim_mode":true,"image_model":"retired-model","image_quality":"xhigh"}"#,
        )
        .unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert!(loaded.vim_mode);
        assert_eq!((loaded.image_model.as_str(), loaded.image_quality.as_str()), ("gpt-image-2", "xhigh"));
        fs::write(
            root.path().join(FILE_NAME),
            r#"{"version":1,"image_model":"gpt-image-1.5","image_quality":"ultra"}"#,
        )
        .unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert_eq!((loaded.image_model.as_str(), loaded.image_quality.as_str()), ("gpt-image-1.5", "medium"));
    }

    #[test]
    fn a_damaged_tour_record_is_dropped_without_touching_the_others() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(FILE_NAME),
            r#"{"version":1,"reduced_motion":true,"tours":{"agents":{"version":1,"completed":false},"ide":{"version":"x"}}}"#,
        )
        .unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert!(loaded.reduced_motion);
        assert_eq!(
            loaded.tours.get("agents"),
            Some(&TourRecord { version: 1, completed: false })
        );
        assert_eq!(loaded.tours.get("ide"), None);
    }

    #[test]
    fn malformed_fields_do_not_discard_valid_preferences() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(FILE_NAME),
            r#"{"version":1,"reduced_motion":true,"terminal_shell":{"kind":"unknown"},"reopen_last_workspace":true,"last_workspace":"./relative","last_settings_page":"../../oops"}"#,
        )
        .unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert!(loaded.reduced_motion);
        assert!(loaded.reopen_last_workspace);
        assert_eq!(loaded.terminal_shell, TerminalShell::System);
        assert_eq!(loaded.last_workspace, None);
        assert_eq!(loaded.last_settings_page, DEFAULT_SETTINGS_PAGE);
        assert_eq!(loaded.startup_workspace(), None);
    }

    #[test]
    fn last_workspace_does_not_reopen_until_enabled() {
        let project = tempfile::tempdir().unwrap();
        let mut preferences = SettingsPreferences::default();
        preferences.set_last_workspace(project.path()).unwrap();
        assert_eq!(preferences.startup_workspace(), None);
        preferences.reopen_last_workspace = true;
        assert_eq!(
            preferences.startup_workspace(),
            preferences.last_workspace.as_deref()
        );
    }

    #[test]
    fn a_removed_last_workspace_does_not_block_other_preferences() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("removed-project");
        fs::create_dir(&project).unwrap();
        let mut preferences = SettingsPreferences::default();
        preferences.set_last_workspace(&project).unwrap();
        fs::remove_dir(&project).unwrap();
        preferences.reduced_motion = true;
        preferences.save(root.path()).unwrap();
        let loaded = SettingsPreferences::load(root.path());
        assert!(loaded.reduced_motion);
        assert!(loaded.last_workspace.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn canonical_workspace_keeps_a_normal_windows_path() {
        assert_eq!(
            user_facing_path(PathBuf::from(r"\\?\C:\Users\Ada\Code")),
            PathBuf::from(r"C:\Users\Ada\Code")
        );
        assert_eq!(
            user_facing_path(PathBuf::from(r"\\?\UNC\host\share\Code")),
            PathBuf::from(r"\\host\share\Code")
        );
    }

    #[test]
    fn invalid_choice_does_not_replace_previous_file() {
        let root = tempfile::tempdir().unwrap();
        let mut preferences = SettingsPreferences::default();
        preferences.save(root.path()).unwrap();
        let before = fs::read(root.path().join(FILE_NAME)).unwrap();

        preferences.terminal_shell = TerminalShell::Custom(PathBuf::from("relative.exe"));
        assert!(preferences.save(root.path()).is_err());
        assert_eq!(fs::read(root.path().join(FILE_NAME)).unwrap(), before);

        preferences.terminal_shell = TerminalShell::System;
        preferences.last_settings_page = "settings_page:bad/path".to_owned();
        assert!(preferences.save(root.path()).is_err());
        assert_eq!(fs::read(root.path().join(FILE_NAME)).unwrap(), before);
    }

    #[test]
    fn custom_shell_requires_an_existing_executable() {
        let current_exe = env::current_exe().unwrap();
        let shell = TerminalShell::Custom(current_exe.clone());
        let command = shell.command().unwrap().unwrap();
        assert_eq!(
            PathBuf::from(command.0),
            current_exe.canonicalize().unwrap()
        );
        assert!(command.1.is_empty());
        assert!(
            TerminalShell::Custom(current_exe.with_file_name("absent-shell"))
                .command()
                .is_err()
        );
        let mut preferences = SettingsPreferences::default();
        assert!(preferences
            .set_terminal_shell(TerminalShell::Custom(PathBuf::from("relative.exe")))
            .is_err());
        assert_eq!(preferences.terminal_shell, TerminalShell::System);
        preferences.set_terminal_shell(shell).unwrap();
        assert_eq!(TerminalShell::System.command().unwrap(), None);
    }

    #[test]
    fn invalid_or_future_file_uses_defaults() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(FILE_NAME), "{").unwrap();
        assert_eq!(
            SettingsPreferences::load(root.path()),
            SettingsPreferences::default()
        );
        fs::write(
            root.path().join(FILE_NAME),
            r#"{"version":2,"reduced_motion":true}"#,
        )
        .unwrap();
        assert_eq!(
            SettingsPreferences::load(root.path()),
            SettingsPreferences::default()
        );
        fs::write(
            root.path().join(FILE_NAME),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(
            SettingsPreferences::load(root.path()),
            SettingsPreferences::default()
        );
    }
}

/// A list a person orders by hand stays short.
pub(crate) const MAX_FALLBACK_MODELS: usize = 8;
