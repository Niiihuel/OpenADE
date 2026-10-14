//! The user's own CLI profiles: `cli-profiles.json` in the data folder, a
//! list in the format of `openide_cli::CliProfile`. An entry with the ID of
//! a built-in profile replaces it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use openide_cli::{CliProfile, PromptTransport};

const FILE: &str = "cli-profiles.json";

/// The profiles of the user's file as written; none when there is no file.
pub(crate) fn user_profiles(root: &Path) -> Result<Vec<CliProfile>, String> {
    match std::fs::read(root.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("Los perfiles de CLI no son válidos: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("No se pudieron leer los perfiles de CLI: {error}")),
    }
}

pub(crate) fn save_user_profiles(root: &Path, profiles: &[CliProfile]) -> Result<(), String> {
    let temporary = root.join(format!(".{FILE}-{}.tmp", std::process::id()));
    std::fs::create_dir_all(root)
        .and_then(|()| std::fs::write(&temporary, serde_json::to_vec_pretty(profiles).unwrap_or_default()))
        .and_then(|()| crate::agent_tool_settings::replace_atomically(&temporary, &root.join(FILE)))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            format!("No se pudieron guardar los perfiles de CLI: {error}")
        })
}

fn env_lines(env: &BTreeMap<String, String>) -> String {
    env.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("\n")
}

/// The form of one of the user's own profiles. `args` is a JSON list and
/// `env` is `NAME=value` lines, as the file stores the values themselves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CliProfileDraft {
    /// The saved ID being edited; `None` means the form creates a profile.
    pub editing: Option<String>,
    pub id: String,
    pub label: String,
    pub executable: String,
    pub args: String,
    pub env: String,
    pub prompt_transport: PromptTransport,
}

impl CliProfileDraft {
    pub fn new() -> Self {
        Self {
            editing: None,
            id: String::new(),
            label: String::new(),
            executable: String::new(),
            args: "[]".to_owned(),
            env: String::new(),
            prompt_transport: PromptTransport::Stdin,
        }
    }

    pub fn edit(saved: &CliProfile) -> Self {
        Self {
            editing: Some(saved.id.clone()),
            id: saved.id.clone(),
            label: saved.label.clone(),
            executable: saved.executable.to_string_lossy().into_owned(),
            args: serde_json::to_string_pretty(&saved.args).unwrap_or_else(|_| "[]".to_owned()),
            env: env_lines(&saved.env),
            prompt_transport: saved.prompt_transport,
        }
    }

    /// The profile to store, or why it could not start a run. An edit starts
    /// from the saved one, so what the form does not show survives it.
    pub fn profile(&self, saved: Option<&CliProfile>) -> Result<CliProfile, String> {
        // The ID becomes a session's engine, where `native` and `cli:` have
        // their own meaning. A saved one is kept as the user wrote it.
        let id = saved.map_or(self.id.trim(), |saved| saved.id.as_str());
        if saved.is_none()
            && (id.is_empty()
                || id.len() > 64
                || id == "native"
                || !id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)))
        {
            return Err("El ID admite hasta 64 minúsculas, números, guiones y guiones bajos, y no puede ser native".into());
        }
        let (label, executable) = (self.label.trim(), self.executable.trim());
        if label.is_empty() || executable.is_empty() {
            return Err("El perfil necesita nombre y ejecutable".into());
        }
        let args: Vec<String> = serde_json::from_str(&self.args)
            .map_err(|error| format!("Los argumentos deben ser una lista JSON de textos: {error}"))?;
        // Lines cannot hold a value with a line break, so an environment the
        // form did not touch is kept rather than read back from them.
        let env = match saved.filter(|saved| env_lines(&saved.env) == self.env) {
            Some(saved) => saved.env.clone(),
            None => self
                .env
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| match line.split_once('=') {
                    Some((name, value)) if !name.trim().is_empty() => Ok((name.trim().to_owned(), value.to_owned())),
                    _ => Err("Cada variable de entorno va como NOMBRE=valor".to_owned()),
                })
                .collect::<Result<_, _>>()?,
        };
        let profile = CliProfile {
            id: id.to_owned(),
            label: label.to_owned(),
            executable: PathBuf::from(executable),
            args,
            env,
            prompt_transport: self.prompt_transport,
            model: saved.and_then(|saved| saved.model.clone()),
            reasoning_effort: saved.and_then(|saved| saved.reasoning_effort.clone()),
        };
        profile.validate()?;
        Ok(profile)
    }
}
