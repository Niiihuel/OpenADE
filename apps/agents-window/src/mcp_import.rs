//! MCP servers another agent on this machine already has configured, read
//! from its own files to be offered here. Nothing is written to those files.
//!
//! Only what this app keeps is taken: programs, arguments, addresses and the
//! *names* of environment variables. A value written in the other agent's
//! file (an `env` entry, a literal header) is never copied.

use crate::mcp_settings::{McpEnvHeader, McpServerSettings, McpSettings, McpSettingsScope};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

pub(crate) struct McpImportController {
    pub scope: McpSettingsScope,
    pub workspace: PathBuf,
    pub known: Vec<McpServerSettings>,
    results: Receiver<Found>,
    terminal_seen: bool,
    invalidated: bool,
}

impl McpImportController {
    pub fn start(
        home: PathBuf,
        workspace: &Path,
        scope: McpSettingsScope,
        known: Vec<McpServerSettings>,
    ) -> Result<Self, String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|_| "El proyecto MCP no está disponible".to_owned())?;
        let worker_workspace = workspace.clone();
        let worker_known = known.clone();
        let (sender, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("mcp-import".into())
            .spawn(move || {
                let _ = sender.send(discover_scope(
                    &home,
                    &worker_workspace,
                    &worker_known,
                    scope,
                ));
            })
            .map_err(|_| "No se pudo iniciar la importación MCP".to_owned())?;
        Ok(Self {
            scope,
            workspace,
            known,
            results,
            terminal_seen: false,
            invalidated: false,
        })
    }

    pub fn matches_saved(
        &self,
        workspace: &Path,
        scope: McpSettingsScope,
        saved: &McpSettings,
    ) -> bool {
        !self.invalidated
            && self.scope == scope
            && workspace
                .canonicalize()
                .is_ok_and(|path| path == self.workspace)
            && self.known == saved.servers
    }

    /// Keep the worker owned until it finishes, but never publish its result
    /// after navigation, including A -> B -> A before the file reads finish.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    pub fn try_recv(&mut self) -> Option<Result<Found, String>> {
        if self.terminal_seen {
            return None;
        }
        let result = match self.results.try_recv() {
            Ok(found) => Some(Ok(found)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("La importación MCP terminó sin resultado".into()))
            }
        };
        self.terminal_seen = result.is_some();
        result
    }
}

const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMPORT_ENTRIES: usize = 1024;
const MAX_IMPORT_CANDIDATES: usize = crate::mcp_settings::MAX_SERVERS * 2;

/// What was found, ready to add, and how many entries could not be used.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub servers: Vec<McpServerSettings>,
    pub skipped: usize,
    /// Fixed diagnostics never include source lines, literal values or parser
    /// excerpts, which can contain credentials in another agent's document.
    pub errors: Vec<String>,
}

/// A name another agent gave a server, as an id of this app: lowercase
/// letters, digits, `-` and `_`, starting with a letter.
fn id_of(name: &str) -> Option<String> {
    let id: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|letter| {
            if letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_' {
                letter
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    id.starts_with(|letter: char| letter.is_ascii_lowercase())
        .then_some(id)
}

/// Names of environment variables this app can pass through: the others are
/// left out, for the user to see when they review the server.
fn env_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let valid = |name: &&str| {
        name.starts_with(|letter: char| letter.is_ascii_uppercase() || letter == '_')
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    };
    let mut names: Vec<String> = names.filter(valid).map(str::to_owned).collect();
    names.sort();
    names.dedup();
    names.truncate(16);
    names
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn keys(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_object)
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// One entry in the shape both agents share once their files are read as
/// JSON: `command` and `args` for a program, `url` for an endpoint.
fn server(name: &str, entry: &Value) -> Option<McpServerSettings> {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let url = text("url");
    // The old HTTP+SSE transport is not one this app speaks.
    if text("type") == Some("sse") || (url.is_none() && text("command").is_none()) {
        return None;
    }
    let args = if url.is_some() {
        Vec::new()
    } else {
        match entry.get("args") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        }
    };
    let mut candidate = McpServerSettings {
        id: id_of(name)?,
        // For the user to review before a turn starts it.
        enabled: false,
        program: if url.is_some() {
            String::new()
        } else {
            text("command")?.to_owned()
        },
        args,
        cwd: if url.is_some() {
            None
        } else {
            text("cwd").map(PathBuf::from)
        },
        credential_env_names: if url.is_some() {
            Vec::new()
        } else {
            // Codex names passed-through variables in `env_vars`; both keep values in `env`.
            let passed = strings(entry.get("env_vars"));
            env_names(
                keys(entry.get("env"))
                    .into_iter()
                    .chain(passed.iter().map(String::as_str)),
            )
        },
        env: Default::default(),
        url: url.map(str::to_owned),
        http_bearer_token_env_name: url.and(text("bearer_token_env_var")).map(str::to_owned),
        http_oauth: None,
        http_env_headers: url
            .and(entry.get("env_http_headers"))
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(header, name)| {
                        Some(McpEnvHeader {
                            header: header.clone(),
                            env_name: name.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    if url.is_some() {
        for (header, value) in entry
            .get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(value) = value.as_str() else {
                continue;
            };
            if header.eq_ignore_ascii_case("authorization") {
                if candidate.http_bearer_token_env_name.is_none() {
                    candidate.http_bearer_token_env_name = value
                        .strip_prefix("Bearer ")
                        .and_then(cursor_env_name)
                        .map(str::to_owned);
                }
            } else if let Some(env_name) = cursor_env_name(value) {
                candidate.http_env_headers.push(McpEnvHeader {
                    header: header.clone(),
                    env_name: env_name.to_owned(),
                });
            }
        }
    }
    McpSettings {
        servers: vec![candidate.clone()],
    }
    .validate_structure()
    .is_ok()
    .then_some(candidate)
}

fn cursor_env_name(value: &str) -> Option<&str> {
    let name = value.strip_prefix("${env:")?.strip_suffix('}')?;
    (env_names(std::iter::once(name)).len() == 1).then_some(name)
}

fn collect(found: &mut Found, servers: Option<&Value>) {
    for (index, (name, entry)) in servers
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if index >= MAX_IMPORT_ENTRIES {
            found.errors.push("Una lista MCP supera el límite de entradas de importación; se revisaron sólo las primeras 1024".into());
            break;
        }
        match server(name, entry) {
            Some(server) if !found.servers.iter().any(|known| known.id == server.id) => {
                if found.servers.len() < MAX_IMPORT_CANDIDATES {
                    found.servers.push(server)
                } else {
                    found.skipped += 1;
                }
            }
            Some(_) => {}
            None => found.skipped += 1,
        }
    }
}

/// Codex keeps its servers under `[mcp_servers.<name>]` of `config.toml`.
pub(crate) fn from_codex(config: &str, found: &mut Found) {
    let Ok(document) = config.parse::<toml_edit::DocumentMut>() else {
        found
            .errors
            .push("Configuración MCP de Codex: TOML no válido".into());
        return;
    };
    // Read through JSON: one shape for both agents.
    let servers = document
        .get("mcp_servers")
        .and_then(|item| item.as_table_like())
        .map(|table| {
            Value::Object(
                table
                    .iter()
                    .map(|(name, item)| (name.to_owned(), toml_json(item)))
                    .collect(),
            )
        });
    collect(found, servers.as_ref());
}

fn toml_json(item: &toml_edit::Item) -> Value {
    if let Some(table) = item.as_table_like() {
        return Value::Object(
            table
                .iter()
                .map(|(key, item)| (key.to_owned(), toml_json(item)))
                .collect(),
        );
    }
    match item.as_value() {
        Some(toml_edit::Value::String(text)) => Value::String(text.value().clone()),
        Some(toml_edit::Value::Array(items)) => Value::Array(
            items
                .iter()
                // Preserve unsupported entries as null so structural parsing
                // rejects a mixed arguments array instead of silently dropping
                // a required argument before it reaches validation.
                .map(|item| {
                    item.as_str()
                        .map(|text| Value::String(text.to_owned()))
                        .unwrap_or(Value::Null)
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

/// Claude Code keeps them under `mcpServers`: of its own file, of a project
/// inside it, and of a project's `.mcp.json`.
#[cfg(test)]
pub(crate) fn from_claude(config: &str, workspace: &Path, found: &mut Found) {
    let Some(document) = json_document(config, "Claude Code", found) else {
        return;
    };
    collect(found, document.get("mcpServers"));
    let project = document
        .get("projects")
        .and_then(|projects| projects.get(workspace.to_string_lossy().as_ref()));
    collect(found, project.and_then(|project| project.get("mcpServers")));
}

fn json_document(config: &str, source: &str, found: &mut Found) -> Option<Value> {
    match serde_json::from_str(config) {
        Ok(document) => Some(document),
        Err(_) => {
            found
                .errors
                .push(format!("Configuración MCP de {source}: JSON no válido"));
            None
        }
    }
}

fn read_config(path: &Path, found: &mut Found) -> Option<String> {
    // O_NONBLOCK prevents a substituted FIFO from blocking the UI before the
    // opened handle's metadata can reject it. Symlinks to regular files work.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let read = || -> io::Result<String> {
        let file: File = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_IMPORT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid settings file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized settings file",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 settings file"))
    };
    match read() {
        Ok(config) => Some(config),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            found.errors.push(format!("No se pudo importar {}: archivo no disponible, no regular, demasiado grande o sin texto UTF-8", path.display()));
            None
        }
    }
}

/// Import sources match the scope being edited. A project registration is
/// never silently promoted to the user's configuration for every workspace.
pub(crate) fn discover_scope(
    home: &Path,
    workspace: &Path,
    known: &[McpServerSettings],
    scope: McpSettingsScope,
) -> Found {
    let mut found = Found::default();
    if scope == McpSettingsScope::User {
        if let Some(config) = read_config(&home.join(".codex/config.toml"), &mut found) {
            from_codex(&config, &mut found);
        }
    }
    if let Some(config) = read_config(&home.join(".claude.json"), &mut found) {
        if let Some(document) = json_document(&config, "Claude Code", &mut found) {
            match scope {
                McpSettingsScope::User => collect(&mut found, document.get("mcpServers")),
                McpSettingsScope::Project => {
                    let project = document
                        .get("projects")
                        .and_then(|projects| projects.get(workspace.to_string_lossy().as_ref()));
                    collect(
                        &mut found,
                        project.and_then(|project| project.get("mcpServers")),
                    );
                }
            }
        }
    }
    if scope == McpSettingsScope::Project {
        if let Some(config) = read_config(&workspace.join(".mcp.json"), &mut found) {
            if let Some(document) = json_document(&config, "Claude Code del proyecto", &mut found) {
                collect(&mut found, document.get("mcpServers"));
            }
        }
    }
    let cursor_path = match scope {
        McpSettingsScope::User => home.join(".cursor/mcp.json"),
        McpSettingsScope::Project => workspace.join(".cursor/mcp.json"),
    };
    if let Some(config) = read_config(&cursor_path, &mut found) {
        if let Some(document) = json_document(&config, "Cursor", &mut found) {
            collect(&mut found, document.get("mcpServers"));
        }
    }
    found
        .servers
        .retain(|server| !known.iter().any(|known| known.id == server.id));
    found
}

/// Everything the agents installed for this user have, without the servers
/// already known here by `known` ids.
#[cfg(test)]
pub(crate) fn discover(home: &Path, workspace: &Path, known: &[McpServerSettings]) -> Found {
    let mut found = discover_scope(home, workspace, known, McpSettingsScope::User);
    let mut known = known.to_vec();
    known.extend(found.servers.iter().cloned());
    let project = discover_scope(home, workspace, &known, McpSettingsScope::Project);
    found.servers.extend(project.servers);
    found.skipped += project.skipped;
    found.errors.extend(project.errors);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_mixed_toml_arguments_are_rejected_without_dropping_required_values() {
        let mut found = Found::default();
        from_codex(
            r#"
[mcp_servers.valid]
command = "fixture"
args = ["--port", "4000"]
[mcp_servers.mixed]
command = "fixture"
args = ["--port", 4000]
[mcp_servers.scalar]
command = "fixture"
args = 4000
"#,
            &mut found,
        );
        assert_eq!(
            found
                .servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["valid"]
        );
        assert_eq!(found.servers[0].args, ["--port", "4000"]);
        assert_eq!(found.skipped, 2);
        assert!(found.errors.is_empty());
    }

    #[test]
    fn import_keeps_relative_cwd_and_refuses_malformed_arguments_and_excess_candidates() {
        let mut found = Found::default();
        from_claude(
            r#"{"mcpServers":{"valid":{"command":"fixture","args":["--literal"],"cwd":"tools/mcp"},"bad-args":{"command":"fixture","args":["--required",9]},"absolute-cwd":{"command":"fixture","cwd":"/outside"}}}"#,
            Path::new("/project"),
            &mut found,
        );
        assert_eq!(found.servers.len(), 1);
        assert_eq!(
            found.servers[0].cwd.as_deref(),
            Some(Path::new("tools/mcp"))
        );
        assert_eq!(found.skipped, 2);
        let entries: serde_json::Map<String, Value> = (0..(MAX_IMPORT_ENTRIES + 3))
            .map(|index| {
                (
                    format!("server-{index}"),
                    serde_json::json!({"command":"fixture"}),
                )
            })
            .collect();
        let mut bounded = Found::default();
        collect(&mut bounded, Some(&Value::Object(entries)));
        assert_eq!(bounded.servers.len(), MAX_IMPORT_CANDIDATES);
        assert_eq!(bounded.skipped, MAX_IMPORT_ENTRIES - MAX_IMPORT_CANDIDATES);
        assert_eq!(bounded.errors.len(), 1);
    }

    #[test]
    fn cursor_imports_stay_in_their_source_scope_and_copy_only_env_references() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        for directory in [home.path(), project.path()] {
            std::fs::create_dir(directory.join(".cursor")).unwrap();
        }
        std::fs::write(home.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"personal":{"url":"https://example.test/mcp","headers":{"Authorization":"Bearer ${env:MCP_TOKEN}","X-Api-Key":"${env:API_KEY}","X-Literal":"private-literal-header"}}}}"#).unwrap();
        std::fs::write(project.path().join(".cursor/mcp.json"), r#"{"mcpServers":{"project":{"command":"fixture-mcp","env":{"SECRET":"private-literal-variable"}}}}"#).unwrap();
        let user = discover_scope(home.path(), project.path(), &[], McpSettingsScope::User);
        let local = discover_scope(home.path(), project.path(), &[], McpSettingsScope::Project);
        assert!(user.errors.is_empty());
        assert!(local.errors.is_empty());
        assert_eq!(user.servers.len(), 1);
        assert_eq!(local.servers.len(), 1);
        assert_eq!(user.servers[0].id, "personal");
        assert_eq!(local.servers[0].id, "project");
        assert_eq!(
            user.servers[0].http_bearer_token_env_name.as_deref(),
            Some("MCP_TOKEN")
        );
        assert_eq!(
            user.servers[0].http_env_headers,
            vec![McpEnvHeader {
                header: "X-Api-Key".into(),
                env_name: "API_KEY".into()
            }]
        );
        assert_eq!(local.servers[0].credential_env_names, ["SECRET"]);
        assert!(user
            .servers
            .iter()
            .chain(&local.servers)
            .all(|server| !server.enabled));
        assert!(!format!("{user:?}{local:?}").contains("private-literal"));
    }

    #[test]
    fn import_reports_corrupt_oversized_and_non_regular_files_without_source_excerpts() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex/config.toml"),
            "private-parser-secret = = =",
        )
        .unwrap();
        std::fs::create_dir(home.path().join(".claude.json")).unwrap();
        std::fs::create_dir(home.path().join(".cursor")).unwrap();
        File::create(home.path().join(".cursor/mcp.json"))
            .unwrap()
            .set_len(MAX_IMPORT_BYTES + 1)
            .unwrap();
        let found = discover_scope(home.path(), project.path(), &[], McpSettingsScope::User);
        assert!(found.servers.is_empty());
        assert_eq!(found.errors.len(), 3);
        assert!(!found.errors.join(" ").contains("private-parser-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_a_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config-fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let mut found = Found::default();
        assert!(read_config(&path, &mut found).is_none());
        assert_eq!(found.errors.len(), 1);
    }

    #[test]
    fn import_controller_rejects_scope_workspace_saved_change_and_return_to_original_scope() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut controller = McpImportController::start(
            home.path().into(),
            project.path(),
            McpSettingsScope::User,
            vec![],
        )
        .unwrap();
        let saved = McpSettings::default();
        assert!(controller.matches_saved(project.path(), McpSettingsScope::User, &saved));
        assert!(!controller.matches_saved(other.path(), McpSettingsScope::User, &saved));
        assert!(!controller.matches_saved(project.path(), McpSettingsScope::Project, &saved));
        let mut changed = Found::default();
        from_claude(
            r#"{"mcpServers":{"new":{"command":"fixture"}}}"#,
            project.path(),
            &mut changed,
        );
        assert!(!controller.matches_saved(
            project.path(),
            McpSettingsScope::User,
            &McpSettings {
                servers: changed.servers
            }
        ));
        controller.invalidate();
        assert!(!controller.matches_saved(project.path(), McpSettingsScope::User, &saved));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(result) = controller.try_recv() {
                assert_eq!(result.unwrap(), Found::default());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(controller.try_recv().is_none());
    }

    #[test]
    fn codex_and_claude_servers_come_in_disabled_with_names_and_never_values() {
        let codex = r#"
model = "gpt"
[mcp_servers.Node_REPL]
command = "node"
args = ["repl.js", "--stdio"]
startup_timeout_sec = 20
env_vars = ["GITHUB_TOKEN", "lowercase"]
[mcp_servers.Node_REPL.env]
API_KEY = "secreto-del-archivo"
[mcp_servers.remoto]
url = "https://mcp.example.com/mcp"
bearer_token_env_var = "REMOTO_TOKEN"
[mcp_servers.remoto.env_http_headers]
X-Api-Key = "REMOTO_KEY"
[mcp_servers."9malo"]
command = "x"
"#;
        let claude = r#"{"mcpServers": {"rea": {"command": "npx", "args": ["-y", "rea"]}, "viejo": {"type": "sse", "url": "https://sse.example.com"}, "remoto": {"command": "duplicado"}},
            "projects": {"/proyecto": {"mcpServers": {"local": {"type": "http", "url": "http://localhost:9000/mcp", "headers": {"Authorization": "Bearer secreto"}}}}}}"#;
        let mut found = Found::default();
        from_codex(codex, &mut found);
        from_claude(claude, Path::new("/proyecto"), &mut found);
        let ids: Vec<&str> = found
            .servers
            .iter()
            .map(|server| server.id.as_str())
            .collect();
        assert_eq!(ids, ["node_repl", "remoto", "rea", "local"]);
        // A name that is no id here and a transport this app does not speak.
        assert_eq!(found.skipped, 2);
        assert!(found.servers.iter().all(|server| !server.enabled));

        let node = &found.servers[0];
        assert_eq!(
            (node.program.as_str(), node.args.as_slice()),
            (
                "node",
                ["repl.js".to_owned(), "--stdio".to_owned()].as_slice()
            )
        );
        assert_eq!(node.credential_env_names, ["API_KEY", "GITHUB_TOKEN"]);
        let remote = &found.servers[1];
        assert_eq!(remote.url.as_deref(), Some("https://mcp.example.com/mcp"));
        assert_eq!(
            remote.http_bearer_token_env_name.as_deref(),
            Some("REMOTO_TOKEN")
        );
        assert_eq!(
            remote.http_env_headers,
            [McpEnvHeader {
                header: "X-Api-Key".into(),
                env_name: "REMOTO_KEY".into()
            }]
        );
        // No value written in either file reaches what is kept.
        let kept = serde_json::to_string(&found.servers).unwrap();
        assert!(!kept.contains("secreto"), "{kept}");
        // Invalid source files are reported without quoting their contents.
        let mut none = Found::default();
        from_codex("esto no es toml = = =", &mut none);
        from_claude("{", Path::new("/proyecto"), &mut none);
        assert!(none.servers.is_empty());
        assert_eq!(none.errors.len(), 2);
    }

    #[test]
    fn discovery_reads_the_agents_files_and_leaves_out_servers_already_known() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex/config.toml"),
            "[mcp_servers.uno]\ncommand = \"uno\"\n",
        )
        .unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers":{"dos":{"command":"dos"}}}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.path().join(".mcp.json"),
            r#"{"mcpServers":{"tres":{"command":"tres"}}}"#,
        )
        .unwrap();
        let all = discover(home.path(), workspace.path(), &[]);
        assert_eq!(
            all.servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["uno", "dos", "tres"]
        );
        let rest = discover(home.path(), workspace.path(), &all.servers[..2]);
        assert_eq!(
            rest.servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["tres"]
        );
        // With none of the files there is nothing to offer.
        assert_eq!(
            discover(workspace.path(), home.path(), &[]),
            Found::default()
        );
    }
}
