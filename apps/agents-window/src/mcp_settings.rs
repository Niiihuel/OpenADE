//! Strict, atomic preferences for explicitly configured local MCP servers.
//!
//! The document stores environment variable *names*, never credential values.
//! Literal command arguments remain user-authored text, so the UI must steer
//! users to pass credentials through `credential_env_names` instead of args.

use openide_harness::{McpHttpConfig, McpStdioConfig, OAuthConfig, OAuthRegistration};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SETTINGS_FILE: &str = "mcp-servers.json";
const VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_SERVERS: usize = 16;
const MAX_ARGS: usize = 32;
const MAX_ARG_BYTES: usize = 2048;
const MAX_ARGS_TOTAL_BYTES: usize = 16 * 1024;
const MAX_PROGRAM_BYTES: usize = 1024;
const MAX_ENV_NAMES: usize = 16;
const MAX_ENV_NAME_BYTES: usize = 64;
const MAX_ID_BYTES: usize = 32;
const MAX_URL_BYTES: usize = 2048;
const MAX_OAUTH_CLIENT_ID_BYTES: usize = 2048;
const MAX_OAUTH_SCOPES: usize = 64;
const MAX_OAUTH_SCOPE_BYTES: usize = 256;
const MAX_OAUTH_SCOPES_TOTAL_BYTES: usize = 4096;
static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[path = "plugin_runtime_tests.rs"]
mod plugin_runtime_tests;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct McpSettings {
    pub servers: Vec<McpServerSettings>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpServerSettings {
    /// Stable lowercase ID used for the `mcp__<id>__<tool>` namespace.
    pub id: String,
    pub enabled: bool,
    /// Bare executable name or absolute path; executed without a shell.
    pub program: String,
    pub args: Vec<String>,
    /// Relative directory below the active workspace. None means workspace.
    pub cwd: Option<PathBuf>,
    /// Only names are persisted. Values are read from the environment at launch.
    pub credential_env_names: Vec<String>,
    /// If present, use MCP 2026-07-28 Streamable HTTP instead of stdio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Name only. The bearer token value is resolved at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_bearer_token_env_name: Option<String>,
    /// Public client registration only. Tokens are stored by TokenStore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_oauth: Option<McpHttpOAuthSettings>,
    /// Headers sent with every HTTP request, each from an environment
    /// variable: only its name is persisted, as with the bearer token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_env_headers: Vec<McpEnvHeader>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpEnvHeader {
    pub header: String,
    pub env_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpHttpOAuthSettings {
    pub client_id: String,
    pub issuer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

impl McpServerSettings {
    /// Resolve credential values only for a launch, keeping them out of the
    /// serialized document. The subprocess receives only a small process
    /// bootstrap environment and the credential variables named here.
    pub fn runtime_config(&self, workspace: &Path) -> Result<McpStdioConfig, String> {
        self.runtime_config_with(workspace, |name| std::env::var_os(name))
    }

    pub fn runtime_http_config(&self) -> Result<McpHttpConfig, String> {
        self.runtime_http_config_with(|name| std::env::var_os(name))
    }

    /// Registration metadata contains no token or client secret. Authorization
    /// is an explicit UI step before an OAuth server is usable by the runner.
    pub fn oauth_config(&self) -> Result<Option<OAuthConfig>, String> {
        McpSettings {
            servers: vec![self.clone()],
        }
        .validate_structure()?;
        let Some(oauth) = &self.http_oauth else {
            return Ok(None);
        };
        let url = self
            .url
            .as_ref()
            .ok_or_else(|| format!("El servidor MCP OAuth {} necesita una URL HTTP", self.id))?;
        Ok(Some(OAuthConfig::new(
            url.clone(),
            OAuthRegistration::PreRegistered {
                client_id: oauth.client_id.clone(),
                issuer: oauth.issuer.clone(),
            },
        )))
    }

    fn runtime_http_config_with<F>(&self, mut resolve_env: F) -> Result<McpHttpConfig, String>
    where
        F: FnMut(&str) -> Option<OsString>,
    {
        if !self.enabled {
            return Err(format!("El servidor MCP {} está deshabilitado", self.id));
        }
        McpSettings {
            servers: vec![self.clone()],
        }
        .validate_structure()?;
        let url = self
            .url
            .as_ref()
            .ok_or_else(|| format!("El servidor MCP {} no usa HTTP", self.id))?;
        let mut config = McpHttpConfig::new(url);
        if let Some(name) = &self.http_bearer_token_env_name {
            let value = resolve_env(name)
                .ok_or_else(|| format!("Falta la variable de credenciales MCP {name}"))?;
            config.bearer_token = Some(value.into_string().map_err(|_| {
                format!("La variable de credenciales MCP {name} debe ser texto UTF-8")
            })?);
        }
        for entry in &self.http_env_headers {
            let name = &entry.env_name;
            let value = resolve_env(name)
                .ok_or_else(|| format!("Falta la variable de entorno MCP {name}"))?
                .into_string()
                .map_err(|_| format!("La variable de entorno MCP {name} debe ser texto UTF-8"))?;
            config.env_headers.push((entry.header.clone(), value));
        }
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    fn runtime_config_with<F>(
        &self,
        workspace: &Path,
        mut resolve_env: F,
    ) -> Result<McpStdioConfig, String>
    where
        F: FnMut(&str) -> Option<OsString>,
    {
        if !self.enabled {
            return Err(format!("El servidor MCP {} está deshabilitado", self.id));
        }
        if self.url.is_some() {
            return Err(format!("El servidor MCP {} usa HTTP, no stdio", self.id));
        }
        McpSettings {
            servers: vec![self.clone()],
        }
        .validate(workspace)?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("Workspace MCP no disponible: {error}"))?;
        let cwd = match &self.cwd {
            Some(relative) => resolve_cwd(relative, &workspace, &self.id)?,
            None => workspace,
        };
        let mut config = McpStdioConfig::new(self.program.as_str());
        config.args = self.args.iter().map(OsString::from).collect();
        config.cwd = Some(cwd);
        for name in bootstrap_env_names() {
            if let Some(value) = resolve_env(name) {
                config.env.insert(OsString::from(name), value);
            }
        }
        for name in &self.credential_env_names {
            let value = resolve_env(name)
                .ok_or_else(|| format!("Falta la variable de credenciales MCP {name}"))?;
            config.env.insert(OsString::from(name), value);
        }
        Ok(config)
    }
}

fn bootstrap_env_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "PATH",
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ]
    } else {
        &[
            "PATH",
            "HOME",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
        ]
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    servers: Vec<McpServerSettings>,
}

impl McpSettings {
    /// Missing settings are an empty list. Existing malformed, oversized, or
    /// unsupported documents are returned as errors rather than reset.
    pub fn load(root: &Path, _workspace: &Path) -> Result<Self, String> {
        let path = root.join(SETTINGS_FILE);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("No se pudo abrir la configuración MCP: {error}")),
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("No se pudo inspeccionar la configuración MCP: {error}"))?;
        if !metadata.is_file() || metadata.len() > MAX_SETTINGS_BYTES {
            return Err("El archivo MCP no es válido o excede el límite de tamaño".to_owned());
        }
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("No se pudo leer la configuración MCP: {error}"))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err("El archivo MCP excede el límite de tamaño".to_owned());
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|error| format!("La configuración MCP está corrupta: {error}"))?;
        if document.version != VERSION {
            return Err(format!(
                "Versión de configuración MCP no compatible: {}",
                document.version
            ));
        }
        let settings = Self {
            servers: document.servers,
        };
        settings.validate_structure()?;
        Ok(settings)
    }

    /// Resolve the next Build turn's external tools without starting a process.
    /// Installed plugins retain their own registry and workspace activation;
    /// they must never appear in, or be saved back through, the MCP editor.
    pub fn load_runtime(root: &Path, workspace: &Path) -> Result<Self, String> {
        let mut settings = Self::load(root, workspace)?;
        settings
            .servers
            .extend(crate::plugins::PluginStore::runtime_servers(
                root, workspace,
            )?);
        // Validate the merged namespace and limit, not only each independent
        // source. A manually configured server cannot shadow an active plugin.
        settings.validate_structure()?;
        Ok(settings)
    }

    /// Validate first, then write a unique temporary file in the same folder
    /// and replace the previous document atomically.
    pub fn save(&self, root: &Path, _workspace: &Path) -> Result<(), String> {
        self.validate_structure()?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            servers: self.servers.clone(),
        })
        .map_err(|error| format!("No se pudo codificar la configuración MCP: {error}"))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err("La configuración MCP excede el límite de tamaño".to_owned());
        }
        fs::create_dir_all(root).map_err(|error| {
            format!("No se pudo crear la carpeta de configuración MCP: {error}")
        })?;
        let temporary = root.join(format!(
            ".mcp-servers-{}-{}.tmp",
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
        result.map_err(|error| format!("No se pudo guardar la configuración MCP: {error}"))
    }

    /// Validate the stored document without tying it to a particular project.
    /// A saved relative cwd may exist in one workspace and not another.
    pub(crate) fn validate_structure(&self) -> Result<(), String> {
        if self.servers.len() > MAX_SERVERS {
            return Err(format!(
                "No se permiten más de {MAX_SERVERS} servidores MCP"
            ));
        }
        let mut ids = BTreeSet::new();
        for server in &self.servers {
            validate_id(&server.id)?;
            if !ids.insert(server.id.as_str()) {
                return Err(format!("ID MCP duplicado: {}", server.id));
            }
            if let Some(url) = &server.url {
                if url.is_empty() || url.len() > MAX_URL_BYTES {
                    return Err(format!("URL MCP HTTP no válida para {}", server.id));
                }
                McpHttpConfig::new(url).validate().map_err(|error| {
                    format!("URL MCP HTTP no válida para {}: {error}", server.id)
                })?;
                if !server.program.is_empty()
                    || !server.args.is_empty()
                    || server.cwd.is_some()
                    || !server.credential_env_names.is_empty()
                {
                    return Err(format!(
                        "El servidor MCP HTTP {} no puede tener campos de stdio",
                        server.id
                    ));
                }
                if let Some(name) = &server.http_bearer_token_env_name {
                    validate_env_names(std::slice::from_ref(name), &server.id)?;
                }
                let names: Vec<String> =
                    server.http_env_headers.iter().map(|entry| entry.env_name.clone()).collect();
                validate_env_names(&names, &server.id)?;
                // The transport refuses a header it owns; a placeholder value checks the names.
                let mut probe = McpHttpConfig::new(url);
                probe.env_headers =
                    server.http_env_headers.iter().map(|entry| (entry.header.clone(), "x".to_owned())).collect();
                let mut unique = BTreeSet::new();
                if probe.validate().is_err()
                    || !server.http_env_headers.iter().all(|entry| unique.insert(entry.header.to_ascii_lowercase()))
                {
                    return Err(format!(
                        "Encabezado MCP HTTP no válido, reservado o duplicado en {}",
                        server.id
                    ));
                }
                if let Some(oauth) = &server.http_oauth {
                    if server.http_bearer_token_env_name.is_some() {
                        return Err(format!(
                            "El servidor MCP HTTP {} debe elegir bearer por entorno u OAuth",
                            server.id
                        ));
                    }
                    validate_oauth(oauth, &server.id)?;
                }
            } else {
                if server.http_bearer_token_env_name.is_some()
                    || server.http_oauth.is_some()
                    || !server.http_env_headers.is_empty()
                {
                    return Err(format!(
                        "El servidor MCP stdio {} no puede tener autenticación HTTP",
                        server.id
                    ));
                }
                validate_program(&server.program, &server.id)?;
                validate_args(&server.args, &server.id)?;
                validate_env_names(&server.credential_env_names, &server.id)?;
                if let Some(cwd) = &server.cwd {
                    validate_cwd_syntax(cwd, &server.id)?;
                }
            }
        }
        Ok(())
    }

    /// Validate paths against the active workspace at launch. In particular,
    /// canonicalization catches missing directories and symlinks that escape
    /// the workspace even when the saved relative path is structurally valid.
    pub fn validate(&self, workspace: &Path) -> Result<(), String> {
        self.validate_structure()?;
        if self.servers.iter().all(|server| server.url.is_some()) {
            return Ok(());
        }
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("Workspace MCP no disponible: {error}"))?;
        if !workspace.is_dir() {
            return Err("El workspace MCP debe ser una carpeta".to_owned());
        }
        for server in &self.servers {
            if let Some(cwd) = &server.cwd {
                resolve_cwd(cwd, &workspace, &server.id)?;
            }
        }
        Ok(())
    }

    pub fn enabled_servers(&self) -> impl Iterator<Item = &McpServerSettings> {
        self.servers.iter().filter(|server| server.enabled)
    }
}

fn validate_oauth(oauth: &McpHttpOAuthSettings, id: &str) -> Result<(), String> {
    if oauth.client_id.is_empty()
        || oauth.client_id.len() > MAX_OAUTH_CLIENT_ID_BYTES
        || oauth.client_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(format!("Client ID OAuth no válido para {id}"));
    }
    let issuer =
        Url::parse(&oauth.issuer).map_err(|_| format!("Issuer OAuth no válido para {id}"))?;
    if oauth.issuer.len() > MAX_URL_BYTES
        || issuer.scheme() != "https"
        || issuer.host_str().is_none()
        || issuer.query().is_some()
        || issuer.fragment().is_some()
        || !issuer.username().is_empty()
        || issuer.password().is_some()
    {
        return Err(format!(
            "Issuer OAuth debe ser HTTPS sin credenciales para {id}"
        ));
    }
    if oauth.scopes.len() > MAX_OAUTH_SCOPES {
        return Err(format!("Demasiados scopes OAuth para {id}"));
    }
    let mut unique = BTreeSet::new();
    let mut total = 0usize;
    for scope in &oauth.scopes {
        total = total.saturating_add(scope.len()).saturating_add(1);
        if scope.is_empty()
            || scope.len() > MAX_OAUTH_SCOPE_BYTES
            || !scope
                .bytes()
                .all(|byte| matches!(byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
            || !unique.insert(scope)
            || total > MAX_OAUTH_SCOPES_TOTAL_BYTES
        {
            return Err(format!("Scope OAuth no válido o duplicado para {id}"));
        }
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(format!("ID MCP no válido: {id:?}"))
    }
}

fn validate_program(program: &str, id: &str) -> Result<(), String> {
    if program.is_empty()
        || program.trim() != program
        || program.len() > MAX_PROGRAM_BYTES
        || program.chars().any(char::is_control)
    {
        return Err(format!("Programa MCP no válido para {id}"));
    }
    let path = Path::new(program);
    let mut components = path.components();
    let is_bare_program =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !path.is_absolute() && !is_bare_program {
        return Err(format!(
            "El programa MCP de {id} debe ser un nombre ejecutable o una ruta absoluta"
        ));
    }
    Ok(())
}

fn validate_args(args: &[String], id: &str) -> Result<(), String> {
    if args.len() > MAX_ARGS {
        return Err(format!("Demasiados argumentos MCP para {id}"));
    }
    let mut total = 0usize;
    for arg in args {
        if arg.len() > MAX_ARG_BYTES || arg.chars().any(char::is_control) {
            return Err(format!("Argumento MCP no válido para {id}"));
        }
        total = total.saturating_add(arg.len());
        if total > MAX_ARGS_TOTAL_BYTES {
            return Err(format!("Argumentos MCP demasiado grandes para {id}"));
        }
    }
    Ok(())
}

fn validate_env_names(names: &[String], id: &str) -> Result<(), String> {
    if names.len() > MAX_ENV_NAMES {
        return Err(format!(
            "Demasiadas variables de credenciales MCP para {id}"
        ));
    }
    let mut unique = BTreeSet::new();
    for name in names {
        let valid = !name.is_empty()
            && name.len() <= MAX_ENV_NAME_BYTES
            && name
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_uppercase() || byte == b'_')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid || !unique.insert(name) {
            return Err(format!(
                "Nombre de variable de credenciales MCP no válido o duplicado en {id}: {name:?}"
            ));
        }
    }
    Ok(())
}

fn validate_cwd_syntax(cwd: &Path, id: &str) -> Result<(), String> {
    if cwd.as_os_str().is_empty()
        || cwd
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("Cwd MCP no válido para {id}"));
    }
    Ok(())
}

fn resolve_cwd(cwd: &Path, workspace: &Path, id: &str) -> Result<PathBuf, String> {
    validate_cwd_syntax(cwd, id)?;
    let resolved = workspace
        .join(cwd)
        .canonicalize()
        .map_err(|error| format!("Cwd MCP no disponible para {id}: {error}"))?;
    if !resolved.starts_with(workspace) || !resolved.is_dir() {
        return Err(format!("Cwd MCP fuera del workspace para {id}"));
    }
    Ok(resolved)
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

    fn server() -> McpServerSettings {
        McpServerSettings {
            id: "filesystem".to_owned(),
            enabled: true,
            program: "npx".to_owned(),
            args: vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
            ],
            cwd: Some(PathBuf::from("repo")),
            credential_env_names: vec!["MCP_API_KEY".to_owned()],
            url: None,
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        }
    }

    fn http_server(url: &str) -> McpServerSettings {
        McpServerSettings {
            id: "remote".to_owned(),
            enabled: true,
            program: String::new(),
            args: Vec::new(),
            cwd: None,
            credential_env_names: Vec::new(),
            url: Some(url.to_owned()),
            http_bearer_token_env_name: Some("MCP_HTTP_TOKEN".to_owned()),
            http_oauth: None,
            http_env_headers: Vec::new(),
        }
    }

    #[test]
    fn http_settings_roundtrip_without_workspace_and_resolve_token_only_at_launch() {
        let root = tempfile::tempdir().unwrap();
        let missing_workspace = root.path().join("does-not-exist");
        let settings = McpSettings {
            servers: vec![http_server("https://example.com/mcp")],
        };
        settings.save(root.path(), &missing_workspace).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), &missing_workspace).unwrap(),
            settings
        );
        assert!(settings.validate(&missing_workspace).is_ok());
        let saved = fs::read_to_string(root.path().join(SETTINGS_FILE)).unwrap();
        assert!(saved.contains("MCP_HTTP_TOKEN"));
        assert!(!saved.contains("secret-token"));
        let config = settings.servers[0]
            .runtime_http_config_with(|name| {
                (name == "MCP_HTTP_TOKEN").then(|| OsString::from("secret-token"))
            })
            .unwrap();
        assert_eq!(config.bearer_token.as_deref(), Some("secret-token"));
        assert!(settings.servers[0]
            .runtime_http_config_with(|_| None)
            .is_err());
    }

    #[test]
    fn http_headers_persist_names_only_resolve_at_launch_and_refuse_what_the_transport_owns() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("does-not-exist");
        let header = |header: &str, env_name: &str| McpEnvHeader { header: header.into(), env_name: env_name.into() };
        let mut server = http_server("https://example.com/mcp");
        server.http_env_headers = vec![header("X-Api-Key", "MCP_API_KEY"), header("X-Team", "MCP_TEAM")];
        let settings = McpSettings { servers: vec![server.clone()] };
        settings.save(root.path(), &workspace).unwrap();
        assert_eq!(McpSettings::load(root.path(), &workspace).unwrap(), settings);
        let saved = fs::read_to_string(root.path().join(SETTINGS_FILE)).unwrap();
        assert!(saved.contains("MCP_API_KEY") && !saved.contains("secret-key"));

        let env = |name: &str| match name {
            "MCP_HTTP_TOKEN" => Some(OsString::from("secret-token")),
            "MCP_API_KEY" => Some(OsString::from("secret-key")),
            "MCP_TEAM" => Some(OsString::from("plataforma")),
            _ => None,
        };
        let config = server.runtime_http_config_with(env).unwrap();
        assert_eq!(
            config.env_headers,
            [("X-Api-Key".to_owned(), "secret-key".to_owned()), ("X-Team".to_owned(), "plataforma".to_owned())]
        );
        // A variable that is not set stops the launch instead of sending the request without it.
        assert!(server.runtime_http_config_with(|name| env(name).filter(|_| name != "MCP_TEAM")).is_err());

        let refused = |entries: Vec<McpEnvHeader>| {
            let mut server = server.clone();
            server.http_env_headers = entries;
            McpSettings { servers: vec![server] }.validate_structure().is_err()
        };
        for owned in ["Authorization", "authorization", "Mcp-Session-Id", "Content-Type", "Host", "con espacio", ""] {
            assert!(refused(vec![header(owned, "MCP_API_KEY")]), "{owned}");
        }
        assert!(refused(vec![header("X-Api-Key", "minuscula")]));
        assert!(refused(vec![header("X-Api-Key", "MCP_API_KEY"), header("x-api-key", "MCP_TEAM")]));
        // Headers are for an HTTP server only.
        let mut stdio = super::tests::server();
        stdio.http_env_headers = vec![header("X-Api-Key", "MCP_API_KEY")];
        assert!(McpSettings { servers: vec![stdio] }.validate_structure().is_err());
    }

    #[test]
    fn oauth_settings_persist_registration_only_and_reject_conflicting_auth() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut server = http_server("https://mcp.example.test/mcp");
        server.http_bearer_token_env_name = None;
        server.http_oauth = Some(McpHttpOAuthSettings {
            client_id: "public-native-client".into(),
            issuer: "https://auth.example.test/tenant".into(),
            scopes: vec!["read".into(), "write".into()],
        });
        let settings = McpSettings {
            servers: vec![server.clone()],
        };
        settings.save(root.path(), workspace.path()).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), workspace.path()).unwrap(),
            settings
        );
        let saved = fs::read_to_string(root.path().join(SETTINGS_FILE)).unwrap();
        assert!(saved.contains("public-native-client"));
        assert!(saved.contains("https://auth.example.test/tenant"));
        assert!(!saved.contains("access_token"));
        assert!(!saved.contains("refresh_token"));
        let mut injected: serde_json::Value = serde_json::from_str(&saved).unwrap();
        injected["servers"][0]["http_oauth"]["access_token"] =
            serde_json::Value::String("secret-token".into());
        fs::write(
            root.path().join(SETTINGS_FILE),
            serde_json::to_vec(&injected).unwrap(),
        )
        .unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());
        settings.save(root.path(), workspace.path()).unwrap();
        let config = server.oauth_config().unwrap().unwrap();
        assert_eq!(config.resource_url, "https://mcp.example.test/mcp");
        assert!(server.runtime_http_config().unwrap().bearer_token.is_none());
        server.http_bearer_token_env_name = Some("MCP_HTTP_TOKEN".into());
        assert!(McpSettings {
            servers: vec![server.clone()]
        }
        .save(root.path(), workspace.path())
        .is_err());
        server.http_bearer_token_env_name = None;
        server.http_oauth.as_mut().unwrap().issuer = "http://auth.example.test".into();
        assert!(server.oauth_config().is_err());
        server.http_oauth.as_mut().unwrap().issuer = "https://auth.example.test".into();
        server
            .http_oauth
            .as_mut()
            .unwrap()
            .scopes
            .push("read".into());
        assert!(server.oauth_config().is_err());
    }

    #[test]
    fn http_settings_reject_url_credentials_query_and_stdio_fields() {
        let workspace = tempfile::tempdir().unwrap();
        let mut settings = McpSettings {
            servers: vec![http_server("https://example.com/mcp?token=secret")],
        };
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].url = Some("https://user:pass@example.com/mcp".into());
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].url = Some("http://example.com/mcp".into());
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].url = Some("http://127.0.0.1:1234/mcp".into());
        assert!(settings.validate(workspace.path()).is_ok());
        settings.servers[0].program = "npx".into();
        assert!(settings.validate(workspace.path()).is_err());
    }

    #[test]
    fn missing_file_roundtrip_and_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), workspace.path()).unwrap(),
            McpSettings::default()
        );
        let mut settings = McpSettings {
            servers: vec![server()],
        };
        settings.save(root.path(), workspace.path()).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), workspace.path()).unwrap(),
            settings
        );
        settings.servers[0].enabled = false;
        settings.save(root.path(), workspace.path()).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), workspace.path()).unwrap(),
            settings
        );
        assert_eq!(settings.enabled_servers().count(), 0);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn relative_cwd_remains_editable_across_workspaces_but_is_checked_at_launch() {
        let root = tempfile::tempdir().unwrap();
        let first_workspace = tempfile::tempdir().unwrap();
        let second_workspace = tempfile::tempdir().unwrap();
        fs::create_dir(first_workspace.path().join("repo")).unwrap();
        let mut settings = McpSettings {
            servers: vec![server()],
        };
        settings.save(root.path(), first_workspace.path()).unwrap();

        assert_eq!(
            McpSettings::load(root.path(), second_workspace.path()).unwrap(),
            settings
        );
        settings.servers[0].program = "node".to_owned();
        settings.save(root.path(), second_workspace.path()).unwrap();
        assert_eq!(
            McpSettings::load(root.path(), second_workspace.path()).unwrap(),
            settings
        );

        let error = settings.servers[0]
            .runtime_config_with(second_workspace.path(), |_| Some(OsString::from("present")))
            .unwrap_err();
        assert!(error.contains("Cwd MCP no disponible"), "{error}");
        let runtime = settings.servers[0]
            .runtime_config_with(first_workspace.path(), |_| Some(OsString::from("present")))
            .unwrap();
        assert_eq!(
            runtime.cwd,
            Some(first_workspace.path().join("repo").canonicalize().unwrap())
        );
    }

    #[test]
    fn duplicate_and_corrupt_config_fail_without_resetting_saved_file() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        let settings = McpSettings {
            servers: vec![server()],
        };
        settings.save(root.path(), workspace.path()).unwrap();
        let saved = fs::read(root.path().join(SETTINGS_FILE)).unwrap();
        let duplicate = McpSettings {
            servers: vec![server(), server()],
        };
        assert!(duplicate
            .save(root.path(), workspace.path())
            .unwrap_err()
            .contains("duplicado"));
        assert_eq!(fs::read(root.path().join(SETTINGS_FILE)).unwrap(), saved);
        fs::write(root.path().join(SETTINGS_FILE), b"{broken").unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());
        fs::write(
            root.path().join(SETTINGS_FILE),
            br#"{"version":99,"servers":[]}"#,
        )
        .unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());
    }

    #[test]
    fn rejects_cwd_escape_absolute_path_and_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        let mut settings = McpSettings {
            servers: vec![server()],
        };
        settings.servers[0].cwd = Some(PathBuf::from(".."));
        assert!(settings.validate(workspace.path()).is_err());
        assert!(settings.save(root.path(), workspace.path()).is_err());
        settings.servers[0].cwd = Some(workspace.path().join("repo"));
        assert!(settings.validate(workspace.path()).is_err());
        assert!(settings.save(root.path(), workspace.path()).is_err());
        fs::write(
            root.path().join(SETTINGS_FILE),
            serde_json::to_vec(&Document {
                version: VERSION,
                servers: settings.servers.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());
        let outside = tempfile::tempdir().unwrap();
        let link = workspace.path().join("linked-outside");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        if linked {
            settings.servers[0].cwd = Some(PathBuf::from("linked-outside"));
            settings.save(root.path(), workspace.path()).unwrap();
            assert_eq!(
                McpSettings::load(root.path(), workspace.path()).unwrap(),
                settings
            );
            assert!(settings.validate(workspace.path()).is_err());
            assert!(settings.servers[0]
                .runtime_config_with(workspace.path(), |_| Some(OsString::from("present")))
                .is_err());
        }
    }

    #[test]
    fn persists_only_credential_variable_names_and_rejects_value_fields() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        let settings = McpSettings {
            servers: vec![server()],
        };
        let runtime = settings.servers[0]
            .runtime_config_with(workspace.path(), |name| match name {
                "MCP_API_KEY" => Some(OsString::from("top-secret-value")),
                "PATH" => Some(OsString::from("safe-bin-path")),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            runtime.env[&OsString::from("MCP_API_KEY")],
            OsString::from("top-secret-value")
        );
        assert!(!runtime.inherit_env);
        assert_eq!(
            runtime.env[&OsString::from("PATH")],
            OsString::from("safe-bin-path")
        );
        assert_eq!(
            runtime.cwd,
            Some(workspace.path().join("repo").canonicalize().unwrap())
        );
        settings.save(root.path(), workspace.path()).unwrap();
        let saved = fs::read_to_string(root.path().join(SETTINGS_FILE)).unwrap();
        assert!(saved.contains("MCP_API_KEY"));
        assert!(!saved.contains("top-secret-value"));

        let mut value: serde_json::Value = serde_json::from_str(&saved).unwrap();
        value["servers"][0]["credential_env_values"] =
            serde_json::json!({"MCP_API_KEY":"top-secret-value"});
        fs::write(
            root.path().join(SETTINGS_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());
        assert!(settings.servers[0]
            .runtime_config_with(workspace.path(), |_| None)
            .is_err());
    }

    #[test]
    fn enforces_server_argument_environment_and_document_limits() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        let mut settings = McpSettings {
            servers: vec![server()],
        };
        settings.servers[0]
            .credential_env_names
            .push("MCP_API_KEY".to_owned());
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].credential_env_names.pop();
        settings.servers[0].args = vec!["x".repeat(MAX_ARG_BYTES + 1)];
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].args.clear();
        settings.servers = (0..=MAX_SERVERS)
            .map(|index| McpServerSettings {
                id: format!("server{index}"),
                ..server()
            })
            .collect();
        assert!(settings.validate(workspace.path()).is_err());

        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(SETTINGS_FILE),
            vec![b' '; MAX_SETTINGS_BYTES as usize + 1],
        )
        .unwrap();
        assert!(McpSettings::load(root.path(), workspace.path()).is_err());

        let oversized = McpSettings {
            servers: (0..5)
                .map(|index| McpServerSettings {
                    id: format!("server{index}"),
                    args: vec!["x".repeat(MAX_ARG_BYTES); 8],
                    ..server()
                })
                .collect(),
        };
        let empty_root = tempfile::tempdir().unwrap();
        assert!(oversized.save(empty_root.path(), workspace.path()).is_err());
        assert!(!empty_root.path().join(SETTINGS_FILE).exists());
    }

    #[test]
    fn rejects_program_traversal_and_invalid_identifiers() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("repo")).unwrap();
        let mut settings = McpSettings {
            servers: vec![server()],
        };
        settings.servers[0].program = "../server".to_owned();
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].program = "..".to_owned();
        assert!(settings.validate(workspace.path()).is_err());
        settings.servers[0].program = "npx".to_owned();
        settings.servers[0].id = "Invalid ID".to_owned();
        assert!(settings.validate(workspace.path()).is_err());
    }
}
