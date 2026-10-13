//! MCP servers another agent on this machine already has configured, read
//! from its own files to be offered here. Nothing is written to those files.
//!
//! Only what this app keeps is taken: programs, arguments, addresses and the
//! *names* of environment variables. A value written in the other agent's
//! file (an `env` entry, a literal header) is never copied.

use crate::mcp_settings::{McpEnvHeader, McpServerSettings, McpSettings};
use serde_json::Value;
use std::path::Path;

/// What was found, ready to add, and how many entries could not be used.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub servers: Vec<McpServerSettings>,
    pub skipped: usize,
}

/// A name another agent gave a server, as an id of this app: lowercase
/// letters, digits, `-` and `_`, starting with a letter.
fn id_of(name: &str) -> Option<String> {
    let id: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|letter| if letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '_' { letter } else { '-' })
        .take(32)
        .collect();
    id.starts_with(|letter: char| letter.is_ascii_lowercase()).then_some(id)
}

/// Names of environment variables this app can pass through: the others are
/// left out, for the user to see when they review the server.
fn env_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let valid = |name: &&str| {
        name.starts_with(|letter: char| letter.is_ascii_uppercase() || letter == '_')
            && name.len() <= 64
            && name.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
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
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn keys(value: Option<&Value>) -> Vec<&str> {
    value.and_then(Value::as_object).map(|object| object.keys().map(String::as_str).collect()).unwrap_or_default()
}

/// One entry in the shape both agents share once their files are read as
/// JSON: `command` and `args` for a program, `url` for an endpoint.
fn server(name: &str, entry: &Value) -> Option<McpServerSettings> {
    let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty());
    let url = text("url");
    // The old HTTP+SSE transport is not one this app speaks.
    if text("type") == Some("sse") || (url.is_none() && text("command").is_none()) {
        return None;
    }
    let candidate = McpServerSettings {
        id: id_of(name)?,
        // For the user to review before a turn starts it.
        enabled: false,
        program: if url.is_some() { String::new() } else { text("command")?.to_owned() },
        args: if url.is_some() { Vec::new() } else { strings(entry.get("args")) },
        cwd: None,
        credential_env_names: if url.is_some() {
            Vec::new()
        } else {
            // Codex names passed-through variables in `env_vars`; both keep values in `env`.
            let passed = strings(entry.get("env_vars"));
            env_names(keys(entry.get("env")).into_iter().chain(passed.iter().map(String::as_str)))
        },
        url: url.map(str::to_owned),
        http_bearer_token_env_name: url.and(text("bearer_token_env_var")).map(str::to_owned),
        http_oauth: None,
        http_env_headers: url
            .and(entry.get("env_http_headers"))
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(header, name)| Some(McpEnvHeader { header: header.clone(), env_name: name.as_str()?.to_owned() }))
                    .collect()
            })
            .unwrap_or_default(),
    };
    McpSettings { servers: vec![candidate.clone()] }.validate_structure().is_ok().then_some(candidate)
}

fn collect(found: &mut Found, servers: Option<&Value>) {
    for (name, entry) in servers.and_then(Value::as_object).into_iter().flatten() {
        match server(name, entry) {
            Some(server) if !found.servers.iter().any(|known| known.id == server.id) => found.servers.push(server),
            Some(_) => {}
            None => found.skipped += 1,
        }
    }
}

/// Codex keeps its servers under `[mcp_servers.<name>]` of `config.toml`.
pub(crate) fn from_codex(config: &str, found: &mut Found) {
    let Ok(document) = config.parse::<toml_edit::DocumentMut>() else { return };
    // Read through JSON: one shape for both agents.
    let servers = document.get("mcp_servers").and_then(|item| item.as_table_like()).map(|table| {
        Value::Object(table.iter().map(|(name, item)| (name.to_owned(), toml_json(item))).collect())
    });
    collect(found, servers.as_ref());
}

fn toml_json(item: &toml_edit::Item) -> Value {
    if let Some(table) = item.as_table_like() {
        return Value::Object(table.iter().map(|(key, item)| (key.to_owned(), toml_json(item))).collect());
    }
    match item.as_value() {
        Some(toml_edit::Value::String(text)) => Value::String(text.value().clone()),
        Some(toml_edit::Value::Array(items)) => {
            Value::Array(items.iter().filter_map(|item| item.as_str()).map(|text| Value::String(text.to_owned())).collect())
        }
        _ => Value::Null,
    }
}

/// Claude Code keeps them under `mcpServers`: of its own file, of a project
/// inside it, and of a project's `.mcp.json`.
pub(crate) fn from_claude(config: &str, workspace: &Path, found: &mut Found) {
    let Ok(document) = serde_json::from_str::<Value>(config) else { return };
    collect(found, document.get("mcpServers"));
    let project = document.get("projects").and_then(|projects| projects.get(workspace.to_string_lossy().as_ref()));
    collect(found, project.and_then(|project| project.get("mcpServers")));
}

/// Everything the agents installed for this user have, without the servers
/// already known here by `known` ids.
pub(crate) fn discover(home: &Path, workspace: &Path, known: &[McpServerSettings]) -> Found {
    // A file of settings is small; one that is not is no settings file.
    let read = |path: std::path::PathBuf| {
        std::fs::metadata(&path).ok().filter(|metadata| metadata.len() <= 8 * 1024 * 1024)?;
        std::fs::read_to_string(path).ok()
    };
    let mut found = Found::default();
    if let Some(config) = read(home.join(".codex/config.toml")) {
        from_codex(&config, &mut found);
    }
    if let Some(config) = read(home.join(".claude.json")) {
        from_claude(&config, workspace, &mut found);
    }
    if let Some(config) = read(workspace.join(".mcp.json")) {
        from_claude(&config, workspace, &mut found);
    }
    found.servers.retain(|server| !known.iter().any(|known| known.id == server.id));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ids: Vec<&str> = found.servers.iter().map(|server| server.id.as_str()).collect();
        assert_eq!(ids, ["node_repl", "remoto", "rea", "local"]);
        // A name that is no id here and a transport this app does not speak.
        assert_eq!(found.skipped, 2);
        assert!(found.servers.iter().all(|server| !server.enabled));

        let node = &found.servers[0];
        assert_eq!((node.program.as_str(), node.args.as_slice()), ("node", ["repl.js".to_owned(), "--stdio".to_owned()].as_slice()));
        assert_eq!(node.credential_env_names, ["API_KEY", "GITHUB_TOKEN"]);
        let remote = &found.servers[1];
        assert_eq!(remote.url.as_deref(), Some("https://mcp.example.com/mcp"));
        assert_eq!(remote.http_bearer_token_env_name.as_deref(), Some("REMOTO_TOKEN"));
        assert_eq!(remote.http_env_headers, [McpEnvHeader { header: "X-Api-Key".into(), env_name: "REMOTO_KEY".into() }]);
        // No value written in either file reaches what is kept.
        let kept = serde_json::to_string(&found.servers).unwrap();
        assert!(!kept.contains("secreto"), "{kept}");
        // What cannot be read is nothing found, not an error.
        let mut none = Found::default();
        from_codex("esto no es toml = = =", &mut none);
        from_claude("{", Path::new("/proyecto"), &mut none);
        assert_eq!(none, Found::default());
    }

    #[test]
    fn discovery_reads_the_agents_files_and_leaves_out_servers_already_known() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".codex")).unwrap();
        std::fs::write(home.path().join(".codex/config.toml"), "[mcp_servers.uno]\ncommand = \"uno\"\n").unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{"dos":{"command":"dos"}}}"#).unwrap();
        std::fs::write(workspace.path().join(".mcp.json"), r#"{"mcpServers":{"tres":{"command":"tres"}}}"#).unwrap();
        let all = discover(home.path(), workspace.path(), &[]);
        assert_eq!(all.servers.iter().map(|server| server.id.as_str()).collect::<Vec<_>>(), ["uno", "dos", "tres"]);
        let rest = discover(home.path(), workspace.path(), &all.servers[..2]);
        assert_eq!(rest.servers.iter().map(|server| server.id.as_str()).collect::<Vec<_>>(), ["tres"]);
        // With none of the files there is nothing to offer.
        assert_eq!(discover(workspace.path(), home.path(), &[]), Found::default());
    }
}
