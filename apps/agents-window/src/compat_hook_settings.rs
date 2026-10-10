//! Opt-in use of hooks written for Claude Code by OpenIDE's native agent.
//!
//! Nothing in another tool's settings runs until the user enables that exact
//! entry here. An entry is remembered by event, matcher and command line, so
//! editing it in the source file switches it off again.

use std::fs;
use std::path::{Path, PathBuf};

use openide_harness::{CompatEvent, CompatHookConfig, MAX_HOOK_TIMEOUT_MS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FILE_NAME: &str = "compat-hooks.json";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 256 * 1024;
/// Claude Code's own default when an entry sets no timeout, in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Whether OpenIDE acts on this event. A listed hook nothing calls must not
/// be switchable: that would be a false promise. Every event discovery knows
/// runs today; a new one starts here as `false` until it is wired.
pub(crate) const fn runs_today(event: CompatEvent) -> bool {
    match event {
        CompatEvent::SessionStart
        | CompatEvent::UserPromptSubmit
        | CompatEvent::PreToolUse
        | CompatEvent::PostToolUse
        | CompatEvent::SubagentStart
        | CompatEvent::Stop => true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookKey {
    /// The plugin that defines the hook, as `name@marketplace`. Empty for the
    /// user's own settings file, and absent in files saved before plugins
    /// were read.
    #[serde(default)]
    pub origin: String,
    pub event: String,
    pub matcher: String,
    pub command: String,
}

/// One command hook found in a Claude Code settings file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredHook {
    pub event: CompatEvent,
    pub key: HookKey,
    pub timeout_ms: u64,
    /// Set for a plugin's hook; handed to it as `CLAUDE_PLUGIN_ROOT`.
    pub plugin_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompatHookSettings {
    enabled: Vec<HookKey>,
    /// Plugins offered to the native agent, with the plugin folder that was
    /// installed when the user switched them on. Skills, commands and agent
    /// personas are all read from inside that folder.
    skill_plugins: std::collections::BTreeMap<String, PathBuf>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    enabled: Vec<HookKey>,
    #[serde(default)]
    skill_plugins: std::collections::BTreeMap<String, PathBuf>,
}

/// An enabled Claude Code plugin that ships skills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredPlugin {
    pub id: String,
    /// The installed plugin folder, canonical.
    pub root: PathBuf,
    pub skills_dir: PathBuf,
    pub skill_count: usize,
}

/// A prompt a plugin offers as a slash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginCommand {
    /// `plugin:name`, the form that cannot collide with a built-in command.
    pub label: String,
    pub description: String,
    pub file: PathBuf,
}

/// An agent persona a plugin defines: instructions for a delegated agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPersona {
    pub name: String,
    pub description: String,
    pub file: PathBuf,
}

const MAX_PLUGIN_DOCUMENT_BYTES: u64 = 256 * 1024;

fn plugin_name(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// A folder named by a plugin's manifest, kept only if it is a real folder
/// inside the plugin.
fn inside(root: &Path, relative: &str) -> Option<PathBuf> {
    root.join(relative)
        .canonicalize()
        .ok()
        .filter(|dir| dir.starts_with(root) && dir.is_dir())
}

fn manifest(root: &Path) -> serde_json::Value {
    read_json(&root.join(".claude-plugin").join("plugin.json"), "plugin")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The skills folder of an installed plugin, as its manifest names it.
pub(crate) fn plugin_skills_dir(root: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    inside(&root, manifest(&root)["skills"].as_str().unwrap_or("skills"))
}

/// Markdown documents directly inside `dir`: regular files, not links, of a
/// bounded size, in name order.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && metadata.len() <= MAX_PLUGIN_DOCUMENT_BYTES
            })
        })
        .collect();
    files.sort();
    files
}

/// Splits a document into its frontmatter fields and its body. A document
/// without frontmatter is all body.
pub(crate) fn split_frontmatter(text: &str) -> (std::collections::BTreeMap<String, String>, &str) {
    let mut fields = std::collections::BTreeMap::new();
    let Some(rest) = text.strip_prefix("---") else {
        return (fields, text);
    };
    let Some((header, body)) = rest.split_once("\n---") else {
        return (fields, text);
    };
    for line in header.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches(|quote| quote == '"' || quote == '\'');
            fields.insert(key.trim().to_owned(), value.to_owned());
        }
    }
    (fields, body.trim_start_matches(|ch| ch != '\n').trim_start())
}

/// The slash commands of an approved plugin folder. The manifest may name
/// several folders; only Markdown prompts are commands here.
pub(crate) fn plugin_commands(id: &str, root: &Path) -> Vec<PluginCommand> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    let declared = match &manifest(&root)["commands"] {
        serde_json::Value::String(dir) => vec![dir.clone()],
        serde_json::Value::Array(dirs) => dirs.iter().filter_map(|dir| dir.as_str().map(str::to_owned)).collect(),
        _ => vec!["commands".to_owned()],
    };
    let mut commands: Vec<PluginCommand> = Vec::new();
    for dir in declared.iter().filter_map(|dir| inside(&root, dir)) {
        for file in markdown_files(&dir) {
            let Some(name) = file.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let label = format!("{}:{name}", plugin_name(id));
            if commands.iter().any(|known| known.label == label) {
                continue;
            }
            let description = fs::read_to_string(&file)
                .map(|text| split_frontmatter(&text).0.remove("description").unwrap_or_default())
                .unwrap_or_default();
            commands.push(PluginCommand { label, description, file });
        }
    }
    commands
}

/// The agent personas of an approved plugin folder.
pub(crate) fn plugin_personas(root: &Path) -> Vec<PluginPersona> {
    let Some(dir) = root.canonicalize().ok().and_then(|root| inside(&root, "agents")) else {
        return Vec::new();
    };
    markdown_files(&dir)
        .into_iter()
        .filter_map(|file| {
            let text = fs::read_to_string(&file).ok()?;
            let mut fields = split_frontmatter(&text).0;
            Some(PluginPersona {
                name: fields.remove("name")?,
                description: fields.remove("description").unwrap_or_default(),
                file,
            })
        })
        .collect()
}

/// The text a command puts in the composer: its body with the argument
/// placeholder removed. The user types the request after it.
pub(crate) fn expand_command(file: &Path) -> Result<String, String> {
    let text = fs::read_to_string(file).map_err(|error| format!("No se pudo leer el comando: {error}"))?;
    let body = split_frontmatter(&text).1.replace("$ARGUMENTS", "");
    let body = body.trim();
    if body.is_empty() {
        return Err("El comando del plugin está vacío".into());
    }
    Ok(format!("{body}\n\n"))
}

/// Skill folders directly inside `dir`: real directories holding a `SKILL.md`.
pub(crate) fn skill_folders(dir: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .collect();
    folders.sort();
    folders
}

/// Lists enabled plugins that have a skills folder inside their own root.
pub(crate) fn discover_plugins(settings_path: &Path) -> Result<Vec<DiscoveredPlugin>, String> {
    let Some(document) = read_json(settings_path, "la configuración de Claude Code")? else {
        return Ok(Vec::new());
    };
    let installed = settings_path
        .parent()
        .map(|dir| dir.join("plugins").join("installed_plugins.json"))
        .and_then(|path| read_json(&path, "plugins").ok().flatten());
    let mut plugins = Vec::new();
    for (id, _) in document["enabledPlugins"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, on)| on.as_bool() == Some(true))
    {
        let Some(root) = installed
            .as_ref()
            .and_then(|installed| installed["plugins"][id][0]["installPath"].as_str())
            .map(Path::new)
            .filter(|root| root.is_absolute())
            .and_then(|root| root.canonicalize().ok())
        else {
            continue;
        };
        let Some(skills_dir) = plugin_skills_dir(&root) else {
            continue;
        };
        let skill_count = skill_folders(&skills_dir).len();
        if skill_count > 0 {
            plugins.push(DiscoveredPlugin {
                id: id.clone(),
                root,
                skills_dir,
                skill_count,
            });
        }
    }
    Ok(plugins)
}

/// The plugin folders the user switched on, by plugin ID. Any problem
/// reading the choice means no plugins, never a guess.
pub(crate) fn enabled_plugin_roots(root: &Path) -> Vec<(String, PathBuf)> {
    CompatHookSettings::load(root)
        .map(|settings| settings.skill_plugins.into_iter().collect())
        .unwrap_or_default()
}

/// The skills folders of the plugins the user switched on, for the skill index.
pub(crate) fn enabled_plugin_skill_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    enabled_plugin_roots(root)
        .into_iter()
        .filter_map(|(id, plugin)| Some((id, plugin_skills_dir(&plugin)?)))
        .collect()
}

pub(crate) fn claude_settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    Some(dir.join("settings.json"))
}

fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > MAX_BYTES => {
            Err("no es un archivo o excede el límite de tamaño".into())
        }
        Ok(_) => fs::read(path).map(Some).map_err(|error| error.to_string()),
    }
}

/// Adds the `type: "command"` hooks of one `hooks` object, for the events
/// OpenIDE knows, skipping any it already has.
fn collect(
    found: &mut Vec<DiscoveredHook>,
    hooks: &serde_json::Value,
    origin: &str,
    plugin_root: Option<&Path>,
) {
    let Some(events) = hooks.as_object() else {
        return;
    };
    for (name, entries) in events {
        let Some(event) = CompatEvent::from_name(name) else {
            continue;
        };
        for entry in entries.as_array().into_iter().flatten() {
            let matcher = entry["matcher"].as_str().unwrap_or_default();
            for hook in entry["hooks"].as_array().into_iter().flatten() {
                let Some(command) = hook["command"].as_str().filter(|command| {
                    hook["type"].as_str() == Some("command") && !command.trim().is_empty()
                }) else {
                    continue;
                };
                let seconds = hook["timeout"].as_u64().unwrap_or(DEFAULT_TIMEOUT_SECS);
                let discovered = DiscoveredHook {
                    event,
                    key: HookKey {
                        origin: origin.to_owned(),
                        event: name.clone(),
                        matcher: matcher.to_owned(),
                        command: command.to_owned(),
                    },
                    // OpenIDE's own ceiling applies; a longer hook is cut off there.
                    timeout_ms: seconds.saturating_mul(1000).clamp(1, MAX_HOOK_TIMEOUT_MS),
                    plugin_root: plugin_root.map(Path::to_path_buf),
                };
                if !found.iter().any(|known| known.key == discovered.key) {
                    found.push(discovered);
                }
            }
        }
    }
}

fn read_json(path: &Path, what: &str) -> Result<Option<serde_json::Value>, String> {
    let Some(bytes) =
        read_bounded(path).map_err(|error| format!("No se pudo leer {what}: {error}"))?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("{what} no es JSON válido: {error}"))
}

/// The hooks file of one installed plugin, if it declares or ships one. The
/// path named by the manifest must stay inside the plugin's own folder.
fn plugin_hooks(root: &Path) -> Option<serde_json::Value> {
    let root = root.canonicalize().ok()?;
    let manifest = read_json(&root.join(".claude-plugin").join("plugin.json"), "plugin").ok()??;
    let declared = manifest["hooks"].as_str().unwrap_or("hooks/hooks.json");
    let file = root.join(declared).canonicalize().ok()?;
    if !file.starts_with(&root) {
        return None;
    }
    read_json(&file, "hooks").ok()?
}

/// Lists command hooks from the user's Claude Code settings and from every
/// plugin enabled there. A missing settings file is an empty list; an
/// unreadable one is an error so nothing is guessed. A plugin that cannot be
/// read contributes nothing rather than failing the rest.
pub(crate) fn discover(settings_path: &Path) -> Result<Vec<DiscoveredHook>, String> {
    let Some(document) = read_json(settings_path, "la configuración de Claude Code")? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    collect(&mut found, &document["hooks"], "", None);

    let installed = settings_path
        .parent()
        .map(|dir| dir.join("plugins").join("installed_plugins.json"))
        .and_then(|path| read_json(&path, "plugins").ok().flatten());
    let enabled = document["enabledPlugins"].as_object();
    for (id, _) in enabled
        .into_iter()
        .flatten()
        .filter(|(_, on)| on.as_bool() == Some(true))
    {
        // Version 2 lists installs per plugin; the first is the one in use.
        let Some(root) = installed
            .as_ref()
            .and_then(|installed| installed["plugins"][id][0]["installPath"].as_str())
            .map(Path::new)
            .filter(|root| root.is_absolute())
        else {
            continue;
        };
        if let Some(hooks) = plugin_hooks(root) {
            collect(&mut found, &hooks["hooks"], id, Some(root));
        }
    }
    Ok(found)
}

impl CompatHookSettings {
    pub fn load(root: &Path) -> Result<Self, String> {
        let Some(bytes) = read_bounded(&root.join(FILE_NAME))
            .map_err(|error| format!("No se pudieron leer los hooks de compatibilidad: {error}"))?
        else {
            return Ok(Self::default());
        };
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Los hooks de compatibilidad no son válidos: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de hooks de compatibilidad no compatible: {}",
                document.version
            ));
        }
        Ok(Self {
            enabled: document.enabled,
            skill_plugins: document.skill_plugins,
        })
    }

    pub fn plugin_skills_enabled(&self, plugin: &DiscoveredPlugin) -> bool {
        self.skill_plugins.get(&plugin.id) == Some(&plugin.root)
    }

    /// Brings the approved plugins in line with what Claude Code has now.
    /// A plugin the user approved keeps working after an update moves it to
    /// a new folder; one that was uninstalled or disabled there is switched
    /// off here too, because it no longer appears in Settings to be switched
    /// off by hand. Returns the IDs that moved and the IDs that were removed.
    pub fn sync_plugins(&mut self, discovered: &[DiscoveredPlugin]) -> (Vec<String>, Vec<String>) {
        let mut moved = Vec::new();
        let mut removed = Vec::new();
        self.skill_plugins.retain(|id, root| {
            match discovered.iter().find(|plugin| &plugin.id == id) {
                Some(plugin) if plugin.root != *root => {
                    *root = plugin.root.clone();
                    moved.push(id.clone());
                    true
                }
                Some(_) => true,
                None => {
                    removed.push(id.clone());
                    false
                }
            }
        });
        (moved, removed)
    }

    /// Returns the new state.
    pub fn toggle_plugin_skills(&mut self, plugin: &DiscoveredPlugin) -> bool {
        if self.plugin_skills_enabled(plugin) {
            self.skill_plugins.remove(&plugin.id);
            false
        } else {
            self.skill_plugins
                .insert(plugin.id.clone(), plugin.root.clone());
            true
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            enabled: self.enabled.clone(),
            skill_plugins: self.skill_plugins.clone(),
        })
        .map_err(|error| error.to_string())?;
        let temporary = root.join(format!(".{FILE_NAME}-{}.tmp", std::process::id()));
        fs::create_dir_all(root)
            .and_then(|()| fs::write(&temporary, bytes))
            .and_then(|()| {
                crate::agent_tool_settings::replace_atomically(&temporary, &root.join(FILE_NAME))
            })
            .map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("No se pudieron guardar los hooks de compatibilidad: {error}")
            })
    }

    /// Whether any hook is enabled. Plugin skills are not hooks.
    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }

    pub fn is_enabled(&self, hook: &DiscoveredHook) -> bool {
        self.enabled.contains(&hook.key)
    }

    /// Returns the new state. An event OpenIDE does not run cannot be enabled.
    pub fn toggle(&mut self, hook: &DiscoveredHook) -> bool {
        if let Some(index) = self.enabled.iter().position(|key| key == &hook.key) {
            self.enabled.remove(index);
            false
        } else if runs_today(hook.event) {
            self.enabled.push(hook.key.clone());
            true
        } else {
            false
        }
    }

    /// Enabled hooks that still exist unchanged in the source file. The
    /// command line runs through `sh -c`, as Claude Code runs it.
    // shortcut: no shell-free launcher exists for Windows, so nothing is returned there.
    pub fn configs(&self, discovered: &[DiscoveredHook], workspace: &Path) -> Vec<CompatHookConfig> {
        if cfg!(not(unix)) {
            return Vec::new();
        }
        discovered
            .iter()
            .filter(|hook| self.is_enabled(hook) && runs_today(hook.event))
            .map(|hook| {
                let mut hasher = Sha256::new();
                hasher.update(hook.key.origin.as_bytes());
                hasher.update([0]);
                hasher.update(hook.key.matcher.as_bytes());
                hasher.update([0]);
                hasher.update(hook.key.command.as_bytes());
                let digest = format!("{:x}", hasher.finalize());
                CompatHookConfig::new(
                    format!("claude-{}-{}", hook.key.event, &digest[..12]),
                    hook.event,
                    hook.key.matcher.clone(),
                    "/bin/sh".into(),
                    vec!["-c".into(), hook.key.command.clone()],
                    workspace.to_path_buf(),
                    hook.timeout_ms,
                )
                .with_env(
                    hook.plugin_root
                        .iter()
                        .map(|root| ("CLAUDE_PLUGIN_ROOT".to_owned(), root.display().to_string()))
                        .collect(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"{"hooks":{
        "PreToolUse":[
            {"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]},
            {"matcher":"*","hooks":[{"type":"command","command":"report.sh","timeout":10},
                                    {"type":"prompt","prompt":"judge this"}]},
            {"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}
        ],
        "SessionStart":[{"hooks":[{"type":"command","command":"rules.sh","timeout":600}]}],
        "Stop":[{"hooks":[{"type":"command","command":"check.sh"}]}],
        "SubagentStart":[{"hooks":[{"type":"command","command":"child.sh"}]}],
        "TeammateIdle":[{"hooks":[{"type":"command","command":"other.sh"}]}]
    }}"#;

    fn source(dir: &Path) -> PathBuf {
        let path = dir.join("settings.json");
        fs::write(&path, SOURCE).unwrap();
        path
    }

    #[test]
    fn discovery_lists_known_command_hooks_once_with_bounded_timeouts() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover(&source(dir.path())).unwrap();
        let summary: Vec<_> = found
            .iter()
            .map(|hook| (hook.event, hook.key.matcher.as_str(), hook.key.command.as_str(), hook.timeout_ms))
            .collect();
        assert_eq!(
            summary,
            vec![
                (CompatEvent::PreToolUse, "Bash", "rtk hook claude", MAX_HOOK_TIMEOUT_MS),
                (CompatEvent::PreToolUse, "*", "report.sh", 10_000),
                (CompatEvent::SessionStart, "", "rules.sh", MAX_HOOK_TIMEOUT_MS),
                (CompatEvent::Stop, "", "check.sh", MAX_HOOK_TIMEOUT_MS),
                (CompatEvent::SubagentStart, "", "child.sh", MAX_HOOK_TIMEOUT_MS),
            ]
        );
        assert_eq!(discover(&dir.path().join("missing.json")).unwrap(), vec![]);
        fs::write(dir.path().join("bad.json"), "{").unwrap();
        assert!(discover(&dir.path().join("bad.json")).is_err());
    }

    #[test]
    fn nothing_runs_until_enabled_and_an_edited_entry_switches_itself_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = source(dir.path());
        let found = discover(&path).unwrap();
        let mut settings = CompatHookSettings::load(dir.path()).unwrap();
        assert!(settings.is_empty());
        assert!(settings.configs(&found, dir.path()).is_empty());

        assert!(settings.toggle(&found[0]));
        settings.save(dir.path()).unwrap();
        let settings = CompatHookSettings::load(dir.path()).unwrap();
        let configs = settings.configs(&found, dir.path());
        if cfg!(unix) {
            assert_eq!(configs.len(), 1);
            assert_eq!(configs[0].event, CompatEvent::PreToolUse);
            assert_eq!(configs[0].matcher, "Bash");
            assert_eq!(configs[0].command.args, vec!["-c", "rtk hook claude"]);
            assert!(configs[0].command.id.starts_with("claude-PreToolUse-"));
        }

        fs::write(&path, SOURCE.replace("rtk hook claude", "rtk hook claude --evil")).unwrap();
        assert!(settings.configs(&discover(&path).unwrap(), dir.path()).is_empty());
    }

    /// A Claude Code config folder with one enabled plugin that declares its
    /// hooks file, one that ships the default file, and one that is disabled.
    fn config_with_plugins(dir: &Path) -> PathBuf {
        let plugin = |name: &str, manifest: &str, hooks_file: &str, command: &str| {
            let root = dir.join("cache").join(name);
            fs::create_dir_all(root.join(".claude-plugin")).unwrap();
            fs::write(root.join(".claude-plugin/plugin.json"), manifest).unwrap();
            let file = root.join(hooks_file);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(
                file,
                format!(r#"{{"hooks":{{"SessionStart":[{{"matcher":"startup|resume","hooks":[{{"type":"command","command":"{command}","timeout":5}}]}}]}}}}"#),
            )
            .unwrap();
            root
        };
        let rules = plugin("rules", r#"{"name":"rules","hooks":"./hooks/custom.json"}"#, "hooks/custom.json", "rules.sh");
        let plain = plugin("plain", r#"{"name":"plain"}"#, "hooks/hooks.json", "plain.sh");
        let off = plugin("off", r#"{"name":"off"}"#, "hooks/hooks.json", "off.sh");
        // Its manifest points outside its own folder.
        let escaping = plugin("escaping", r#"{"name":"escaping","hooks":"../rules/hooks/custom.json"}"#, "hooks/unused.json", "x.sh");
        fs::create_dir_all(dir.join("plugins")).unwrap();
        fs::write(
            dir.join("plugins/installed_plugins.json"),
            serde_json::json!({"version": 2, "plugins": {
                "rules@market": [{"installPath": rules}],
                "plain@market": [{"installPath": plain}],
                "off@market": [{"installPath": off}],
                "escaping@market": [{"installPath": escaping}],
                "relative@market": [{"installPath": "cache/rules"}],
            }})
            .to_string(),
        )
        .unwrap();
        let settings = dir.join("settings.json");
        fs::write(
            &settings,
            r#"{"enabledPlugins":{"rules@market":true,"plain@market":true,"off@market":false,
                "escaping@market":true,"relative@market":true,"missing@market":true},
               "hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"own.sh"}]}]}}"#,
        )
        .unwrap();
        settings
    }

    #[test]
    fn plugin_hooks_are_listed_with_their_origin_and_only_from_enabled_readable_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover(&config_with_plugins(dir.path())).unwrap();
        let summary: Vec<_> = found
            .iter()
            .map(|hook| (hook.key.origin.as_str(), hook.key.command.as_str(), hook.plugin_root.is_some()))
            .collect();
        assert_eq!(
            summary,
            vec![("", "own.sh", false), ("plain@market", "plain.sh", true), ("rules@market", "rules.sh", true)]
        );
    }

    #[test]
    fn a_plugin_hook_gets_its_own_root_and_is_distinct_from_the_same_command_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover(&config_with_plugins(dir.path())).unwrap();
        let mut settings = CompatHookSettings::default();
        for hook in &found {
            assert!(settings.toggle(hook));
        }
        let configs = settings.configs(&found, dir.path());
        if cfg!(unix) {
            assert_eq!(configs.len(), 3);
            assert!(configs[0].env.is_empty());
            let root = dir.path().join("cache/rules").canonicalize().unwrap();
            assert_eq!(configs[2].env, vec![("CLAUDE_PLUGIN_ROOT".to_owned(), root.display().to_string())]);
            let ids: std::collections::HashSet<_> = configs.iter().map(|config| &config.command.id).collect();
            assert_eq!(ids.len(), 3);
        }
        // The same event, matcher and command under another origin is another hook.
        let mut copy = found[2].clone();
        copy.key.origin = "other@market".into();
        assert!(!settings.is_enabled(&copy));
    }

    /// The reason this reader exists, checked against the real plugin when it
    /// and `node` are installed. Skipped otherwise, unless
    /// `OPENIDE_REQUIRE_LIVE_PLUGINS` is set, which turns a skip into a
    /// failure so a pass proves the hook really ran. The hook's home folder
    /// is redirected so the test never writes into the user's own.
    #[cfg(unix)]
    #[test]
    fn installed_ponytail_session_hook_runs_unchanged_and_returns_its_rules() {
        let skip = |why: &str| {
            assert!(
                std::env::var_os("OPENIDE_REQUIRE_LIVE_PLUGINS").is_none(),
                "live plugin check could not run: {why}"
            );
        };
        let Some(source) = claude_settings_path().filter(|path| path.is_file()) else {
            return skip("no Claude Code settings file");
        };
        let on_path = |name: &str| {
            std::env::var_os("PATH")
                .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
        };
        let Ok(found) = discover(&source) else {
            return skip("settings could not be read");
        };
        let Some(hook) = found.iter().find(|hook| {
            hook.key.origin.starts_with("ponytail@") && hook.event == CompatEvent::SessionStart
        }) else {
            return skip("no enabled ponytail plugin with a session hook");
        };
        if !on_path("node") {
            return skip("node is not on PATH");
        }
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut settings = CompatHookSettings::default();
        assert!(settings.toggle(hook));
        let mut configs = settings.configs(std::slice::from_ref(hook), workspace.path());
        assert_eq!(configs.len(), 1);
        configs[0].env.push(("HOME".into(), home.path().display().to_string()));
        configs[0].env.push(("CLAUDE_CONFIG_DIR".into(), home.path().join(".claude").display().to_string()));
        let hooks = openide_harness::CommandHooks::new(vec![])
            .unwrap()
            .with_compat(configs)
            .unwrap();
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(hooks.run_event(
                CompatEvent::SessionStart,
                "startup",
                serde_json::json!({"source": "startup"}),
                &tokio_util::sync::CancellationToken::new(),
            ));
        let context = outcome.context.join("\n");
        assert!(outcome.blocked.is_none());
        assert!(
            context.to_uppercase().contains("PONYTAIL") && context.len() > 200,
            "the plugin's hook returned no rules: {context:?}"
        );
    }

    #[test]
    fn plugin_skills_are_found_inside_the_plugin_and_stay_off_until_switched_on() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = config_with_plugins(dir.path());
        let rules = dir.path().join("cache/rules");
        for (skill, body) in [("spec", "---\nname: spec\ndescription: Write a spec.\n---\nBODY"), ("empty", "")] {
            fs::create_dir_all(rules.join("skills").join(skill)).unwrap();
            if !body.is_empty() {
                fs::write(rules.join("skills").join(skill).join("SKILL.md"), body).unwrap();
            }
        }
        fs::write(rules.join("skills/README.md"), "not a skill").unwrap();
        let plugins = discover_plugins(&settings_path).unwrap();
        assert_eq!(plugins.len(), 1, "{plugins:?}");
        assert_eq!((plugins[0].id.as_str(), plugins[0].skill_count), ("rules@market", 1));
        assert!(plugins[0].skills_dir.starts_with(rules.canonicalize().unwrap()));

        assert!(enabled_plugin_skill_dirs(dir.path()).is_empty());
        let mut settings = CompatHookSettings::default();
        assert!(settings.toggle_plugin_skills(&plugins[0]));
        settings.save(dir.path()).unwrap();
        assert_eq!(
            enabled_plugin_skill_dirs(dir.path()),
            vec![("rules@market".to_owned(), plugins[0].skills_dir.clone())]
        );
        assert_eq!(
            enabled_plugin_roots(dir.path()),
            vec![("rules@market".to_owned(), plugins[0].root.clone())]
        );
        // The choice is tied to the installed folder; another folder is not approved.
        let mut moved = plugins[0].clone();
        moved.root = dir.path().join("cache/rules-2");
        assert!(!CompatHookSettings::load(dir.path()).unwrap().plugin_skills_enabled(&moved));
        assert!(!settings.toggle_plugin_skills(&plugins[0]));
    }

    /// Against the real agent-skills plugin when it is installed and enabled.
    /// Only reads the user's plugin files; the choice is saved to a temporary
    /// folder. Skipped otherwise unless `OPENIDE_REQUIRE_LIVE_PLUGINS` is set.
    #[test]
    fn installed_agent_skills_plugin_is_indexed_unchanged() {
        let skip = |why: &str| {
            assert!(
                std::env::var_os("OPENIDE_REQUIRE_LIVE_PLUGINS").is_none(),
                "live plugin check could not run: {why}"
            );
        };
        let Some(source) = claude_settings_path().filter(|path| path.is_file()) else {
            return skip("no Claude Code settings file");
        };
        let plugins = discover_plugins(&source).unwrap_or_default();
        let Some(plugin) = plugins.iter().find(|plugin| plugin.id.starts_with("agent-skills@")) else {
            return skip("no enabled agent-skills plugin");
        };
        assert!(plugin.skill_count >= 10, "{plugin:?}");
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut settings = CompatHookSettings::default();
        assert!(settings.toggle_plugin_skills(plugin));
        settings.save(data.path()).unwrap();
        let index = crate::marketplace::active_skill_instructions(data.path(), workspace.path()).unwrap();
        // Skills and personas share the `plugin:` prefix; skills are the
        // entries that carry a skill file.
        let listed = index.matches("\"skill_file\":").count();
        assert_eq!(listed, plugin.skill_count, "every skill fits the index");
        assert!(index.contains("\"scope\":\"plugin\""));
        let roots = crate::marketplace::active_skill_directories(data.path(), workspace.path()).unwrap();
        assert!(roots.iter().all(|root| root.starts_with(&plugin.root)));

        // Its commands and personas are found as shipped.
        let commands = plugin_commands(&plugin.id, &plugin.root);
        let labels: Vec<_> = commands.iter().map(|command| command.label.as_str()).collect();
        for expected in ["agent-skills:spec", "agent-skills:plan", "agent-skills:ship"] {
            assert!(labels.contains(&expected), "{expected} missing in {labels:?}");
        }
        for command in &commands {
            let body = expand_command(&command.file).unwrap();
            assert!(!body.contains("$ARGUMENTS") && body.len() > 20, "{}", command.label);
        }
        let personas = plugin_personas(&plugin.root);
        assert!(personas.iter().any(|persona| persona.name == "code-reviewer"), "{personas:?}");
        assert!(personas.len() >= 3);
        assert_eq!(index.matches("\"kind\":\"agent_persona\"").count(), personas.len());
        assert!(index.contains("\"name\":\"agent-skills:code-reviewer\""));
    }

    #[test]
    fn approved_plugins_follow_an_update_and_are_dropped_when_claude_code_drops_them() {
        let plugin = |id: &str, root: &str| DiscoveredPlugin {
            id: id.into(),
            root: root.into(),
            skills_dir: PathBuf::from(root).join("skills"),
            skill_count: 1,
        };
        let mut settings = CompatHookSettings::default();
        settings.toggle_plugin_skills(&plugin("kept@m", "/p/kept/1"));
        settings.toggle_plugin_skills(&plugin("updated@m", "/p/updated/1"));
        settings.toggle_plugin_skills(&plugin("gone@m", "/p/gone/1"));

        let now = [plugin("kept@m", "/p/kept/1"), plugin("updated@m", "/p/updated/2"), plugin("new@m", "/p/new/1")];
        let (moved, removed) = settings.sync_plugins(&now);
        assert_eq!((moved, removed), (vec!["updated@m".to_owned()], vec!["gone@m".to_owned()]));
        assert!(settings.plugin_skills_enabled(&now[0]));
        assert!(settings.plugin_skills_enabled(&now[1]), "the approval follows the update");
        // A plugin the user never approved is not switched on by a sync.
        assert!(!settings.plugin_skills_enabled(&now[2]));
        // Nothing changed: nothing reported.
        assert_eq!(settings.sync_plugins(&now), (vec![], vec![]));
    }

    #[test]
    fn commands_and_personas_are_read_from_inside_the_plugin_folder_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flow");
        for sub in [".claude-plugin", ".claude/commands", "commands", "agents", "outside"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"flow","commands":["./.claude/commands","./commands","../elsewhere"]}"#,
        )
        .unwrap();
        fs::write(
            root.join(".claude/commands/spec.md"),
            "---\ndescription: Write a spec first\n---\n\nInvoke the flow:spec skill.\n\n$ARGUMENTS\n",
        )
        .unwrap();
        fs::write(root.join(".claude/commands/plain.md"), "Just a prompt.").unwrap();
        // Same name in a later folder, a non-prompt twin, and an empty one.
        fs::write(root.join("commands/spec.md"), "---\ndescription: duplicate\n---\nother").unwrap();
        fs::write(root.join("commands/spec.toml"), "prompt = 'x'").unwrap();
        fs::write(root.join("commands/blank.md"), "---\ndescription: nothing\n---\n").unwrap();
        fs::write(
            root.join("agents/code-reviewer.md"),
            "---\nname: code-reviewer\ndescription: \"Reviews a change\"\n---\nYou review code.",
        )
        .unwrap();
        fs::write(root.join("agents/notes.md"), "no frontmatter, not a persona").unwrap();

        let commands = plugin_commands("flow@market", &root);
        let summary: Vec<_> = commands.iter().map(|command| (command.label.as_str(), command.description.as_str())).collect();
        assert_eq!(
            summary,
            vec![("flow:plain", ""), ("flow:spec", "Write a spec first"), ("flow:blank", "nothing")]
        );
        assert!(commands.iter().all(|command| command.file.starts_with(root.canonicalize().unwrap())));
        assert_eq!(expand_command(&commands[1].file).unwrap(), "Invoke the flow:spec skill.\n\n");
        assert_eq!(expand_command(&commands[0].file).unwrap(), "Just a prompt.\n\n");
        assert!(expand_command(&commands[2].file).is_err());

        let personas = plugin_personas(&root);
        assert_eq!(personas.len(), 1);
        assert_eq!((personas[0].name.as_str(), personas[0].description.as_str()), ("code-reviewer", "Reviews a change"));
        assert!(plugin_commands("x@y", &dir.path().join("missing")).is_empty());
        assert!(plugin_personas(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn toggling_twice_disables_and_unknown_documents_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let found = discover(&source(dir.path())).unwrap();
        let mut settings = CompatHookSettings::default();
        assert!(settings.toggle(&found[1]));
        assert!(!settings.toggle(&found[1]));
        assert!(settings.is_empty());
        fs::write(dir.path().join(FILE_NAME), r#"{"version":2,"enabled":[]}"#).unwrap();
        assert!(CompatHookSettings::load(dir.path()).is_err());
    }
}
