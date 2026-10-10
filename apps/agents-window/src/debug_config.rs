//! Explicit project debug configuration. Reading a project never launches it.
use openide_debug::DebugConfig;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) const CONFIG_PATH: &str = ".openide/debug.json";
const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Adapter {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "generic")]
    id: String,
}
fn generic() -> String {
    "generic".into()
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration {
    pub name: String,
    adapter: Adapter,
    request: String,
    arguments: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    configurations: Vec<Configuration>,
}

pub(crate) fn read(root: &Path) -> Result<Vec<Configuration>, String> {
    // A project is allowed to have no launch configuration. Inspect the
    // directory itself so a dangling/outside symlink is still an error.
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    match std::fs::symlink_metadata(root.join(".openide")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let path = config_path(&root, false)?;
    let mut bytes = Vec::new();
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("La configuración debe ser un archivo regular".into());
    }
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    parse(&bytes)
}
fn config_path(root: &Path, create: bool) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let directory = root.join(".openide");
    if create && !directory.exists() {
        std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    }
    let directory = directory.canonicalize().map_err(|e| e.to_string())?;
    if !directory.starts_with(&root) {
        return Err(".openide debe estar dentro del proyecto".into());
    }
    let path = directory.join("debug.json");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() {
            return Err("debug.json debe ser un archivo regular sin enlaces simbólicos".into());
        }
    }
    Ok(path)
}
fn parse(bytes: &[u8]) -> Result<Vec<Configuration>, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("La configuración excede 256 KiB".into());
    }
    let file: File =
        serde_json::from_slice(bytes).map_err(|e| format!("Configuración de depuración: {e}"))?;
    if file.version != 1 || file.configurations.is_empty() || file.configurations.len() > 16 {
        return Err("Usa version: 1 y entre 1 y 16 configuraciones".into());
    }
    for config in &file.configurations {
        if config.request != "launch" || !config.arguments.is_object() {
            return Err("Se requiere request: launch y arguments como objeto".into());
        }
        if config.name.trim().is_empty() || config.adapter.command.trim().is_empty() {
            return Err("Cada configuración necesita name y adapter.command".into());
        }
        if config.adapter.args.len() > 64 || config.adapter.env.len() > 64 {
            return Err("Demasiados argumentos o variables de entorno".into());
        }
    }
    Ok(file.configurations)
}
fn expand(value: &mut Value, root: &str) {
    match value {
        Value::String(s) => *s = s.replace("${workspaceFolder}", root),
        Value::Array(values) => values.iter_mut().for_each(|v| expand(v, root)),
        Value::Object(values) => values.values_mut().for_each(|v| expand(v, root)),
        _ => {}
    }
}
impl Configuration {
    pub(crate) fn adapter_name(&self) -> String {
        self.adapter.command.clone()
    }
    pub(crate) fn launch(&self, workspace: &str) -> DebugConfig {
        let mut arguments = self.arguments.clone();
        expand(&mut arguments, workspace);
        let command = self
            .adapter
            .command
            .replace("${workspaceFolder}", workspace);
        let mut config = DebugConfig::new(PathBuf::from(command), workspace, arguments);
        config.args = self
            .adapter
            .args
            .iter()
            .map(|s| s.replace("${workspaceFolder}", workspace))
            .collect();
        config.env = self
            .adapter
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("${workspaceFolder}", workspace)))
            .collect();
        config.adapter_id = self.adapter.id.clone();
        config
    }
}

/// A template is created only by the explicit Configure action. Existing files
/// are preserved, including invalid JSON, so editing never destroys user data.
pub(crate) fn ensure_template(root: &Path) -> Result<(), String> {
    use std::io::Write;
    let path = config_path(root, true)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    match file {
        Ok(mut file) => file
            .write_all(TEMPLATE.as_bytes())
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
const TEMPLATE: &str = r#"{
  "version": 1,
  "configurations": [
    {
      "name": "Rust · LLDB",
      "adapter": { "command": "lldb-dap", "args": [], "id": "lldb" },
      "request": "launch",
      "arguments": {
        "program": "${workspaceFolder}/target/debug/mi-programa",
        "cwd": "${workspaceFolder}",
        "stopOnEntry": false,
        "disableASLR": false
      }
    }
  ]
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_configuration_is_empty_without_creating_files_or_hiding_invalid_json() {
        let root = tempfile::tempdir().unwrap();
        assert!(read(root.path()).unwrap().is_empty());
        assert!(!root.path().join(".openide").exists());
        std::fs::create_dir(root.path().join(".openide")).unwrap();
        assert!(read(root.path()).unwrap().is_empty());
        std::fs::write(root.path().join(CONFIG_PATH), "unfinished JSON").unwrap();
        assert!(
            read(root.path())
                .unwrap_err()
                .contains("Configuración de depuración")
        );
        std::fs::write(
            root.path().join(CONFIG_PATH),
            r#"{"version":1,"configurations":[]}"#,
        )
        .unwrap();
        assert!(
            read(root.path()).is_err(),
            "an existing invalid configuration is not onboarding"
        );
    }
    #[test]
    fn validates_and_expands_without_shell_interpolation() {
        let config = parse(TEMPLATE.as_bytes())
            .unwrap()
            .remove(0)
            .launch("/project with spaces");
        assert_eq!(
            config.launch["program"],
            "/project with spaces/target/debug/mi-programa"
        );
        assert_eq!(config.executable, PathBuf::from("lldb-dap"));
        assert!(parse(br#"{"version":2,"configurations":[]}"#).is_err());
        assert!(parse(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
    }
    #[test]
    fn configure_preserves_existing_user_file() {
        let root = tempfile::tempdir().unwrap();
        ensure_template(root.path()).unwrap();
        let path = root.path().join(CONFIG_PATH);
        std::fs::write(&path, "unfinished user config").unwrap();
        ensure_template(root.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "unfinished user config"
        );
    }
}
