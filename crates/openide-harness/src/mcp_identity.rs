//! Conservative runtime fingerprints for MCP calls which have not started.
//! A fingerprint detects changes in the declared launch or HTTPS authority,
//! handshake, and advertised catalog. It cannot prove that an external
//! service or binary will behave identically when undeclared data changes.

use crate::mcp_http::is_loopback_host;
use crate::{McpServerInfo, McpStdioConfig, MCP_HTTP_PROTOCOL_VERSION};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_CODE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARGS: usize = 32;
const MAX_ARG_BYTES: usize = 2048;
const MAX_SERVERS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpStdioLaunchIdentity {
    pub canonical_command: PathBuf,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub struct McpStdioServerIdentity {
    pub id: String,
    pub launch_digest: String,
    pub server_info: McpServerInfo,
}

/// A modern, stateless HTTPS server observed with a verified authorization
/// binding when authentication is present.
/// Construction is restricted to `McpHttpClient` after successful discovery.
#[derive(Clone, Debug)]
pub struct McpHttpServerIdentity {
    id: String,
    endpoint: String,
    client_name: String,
    client_version: String,
    request_timeout_ms: u128,
    max_message_bytes: usize,
    authorization: Option<McpHttpAuthIdentity>,
    server_info: McpServerInfo,
}

#[derive(Clone, Debug)]
pub(crate) enum McpHttpAuthIdentity {
    Bearer(String),
    OAuth(String),
}

impl McpHttpServerIdentity {
    pub(crate) fn new(
        id: String,
        endpoint: String,
        client: (String, String),
        limits: (u128, usize),
        authorization: Option<McpHttpAuthIdentity>,
        server_info: McpServerInfo,
    ) -> Self {
        Self {
            id,
            endpoint,
            client_name: client.0,
            client_version: client.1,
            request_timeout_ms: limits.0,
            max_message_bytes: limits.1,
            authorization,
            server_info,
        }
    }
}

#[derive(Clone, Debug)]
pub enum McpServerRuntimeIdentity {
    Stdio(McpStdioServerIdentity),
    Http(McpHttpServerIdentity),
}

/// Inspect an absolute program and the exact argv/cwd/bootstrap environment.
/// File arguments are content-hashed; interpreters and indirect code flags
/// fail closed. The caller must only use this for a server with no credentials
/// and launch the returned canonical command path, then recheck after spawn.
pub fn inspect_mcp_stdio_launch(config: &McpStdioConfig) -> Option<McpStdioLaunchIdentity> {
    if config.inherit_env || config.args.len() > MAX_ARGS {
        return None;
    }
    let command = Path::new(&config.command);
    if !command.is_absolute() {
        return None;
    }
    let canonical_command = command.canonicalize().ok()?;
    if !canonical_command.is_file()
        || indirect_launcher(&canonical_command)
        || !direct_binary_format(&canonical_command)
    {
        return None;
    }
    let cwd = config.cwd.as_ref()?.canonicalize().ok()?;
    if !cwd.is_dir() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"openide-mcp-stdio-launch-v1\0");
    hash_field(&mut hasher, canonical_command.to_str()?.as_bytes());
    hash_file(&mut hasher, &canonical_command)?;
    hash_field(&mut hasher, cwd.to_str()?.as_bytes());
    hash_field(
        &mut hasher,
        &config.request_timeout.as_millis().to_be_bytes(),
    );
    hash_field(
        &mut hasher,
        &config.shutdown_timeout.as_millis().to_be_bytes(),
    );
    hash_field(
        &mut hasher,
        &(config.max_message_bytes as u64).to_be_bytes(),
    );
    hash_field(&mut hasher, config.client_name.as_bytes());
    hash_field(&mut hasher, config.client_version.as_bytes());
    for argument in &config.args {
        let argument = argument.to_str()?;
        if argument.len() > MAX_ARG_BYTES
            || argument.bytes().any(|byte| byte == b'"' || byte == b'\'')
            || indirect_code_flag(argument)
            || secret_bearing_argument(argument)
        {
            return None;
        }
        if argument.starts_with('-') && !argument.contains('=') {
            hash_field(&mut hasher, argument.as_bytes());
            continue;
        }
        let candidate = argument
            .split_once('=')
            .map_or(argument, |(_, value)| value);
        let path = Path::new(candidate);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if resolved.is_file() {
            let canonical = resolved.canonicalize().ok()?;
            hash_field(&mut hasher, argument.as_bytes());
            hash_field(&mut hasher, canonical.to_str()?.as_bytes());
            hash_file(&mut hasher, &canonical)?;
        } else if resolved.is_dir() {
            hash_field(&mut hasher, argument.as_bytes());
            hash_field(
                &mut hasher,
                resolved.canonicalize().ok()?.to_str()?.as_bytes(),
            );
        } else {
            return None;
        }
    }
    for (name, value) in &config.env {
        let name = name.to_str()?;
        let normalized = name.to_ascii_uppercase();
        if !matches!(
            normalized.as_str(),
            "PATH"
                | "SYSTEMROOT"
                | "WINDIR"
                | "TEMP"
                | "TMP"
                | "USERPROFILE"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "HOME"
                | "TMPDIR"
                | "XDG_CACHE_HOME"
                | "XDG_CONFIG_HOME"
        ) {
            return None;
        }
        hash_field(&mut hasher, normalized.as_bytes());
        hash_field(&mut hasher, value.to_str()?.as_bytes());
    }
    Some(McpStdioLaunchIdentity {
        canonical_command,
        digest: format!("{:x}", hasher.finalize()),
    })
}

/// Bind all connected stdio servers to the exact frozen tool catalog. A
/// caller must omit this marker if any enabled server was not attestable.
pub fn mcp_stdio_runtime_digest(
    servers: &[McpStdioServerIdentity],
    catalog_digest: &str,
) -> Option<String> {
    if servers.is_empty() || servers.len() > MAX_SERVERS || !valid_digest(catalog_digest) {
        return None;
    }
    let mut servers = servers.to_vec();
    servers.sort_by(|a, b| a.id.cmp(&b.id));
    let mut ids = BTreeSet::new();
    let mut hasher = Sha256::new();
    hasher.update(b"openide-mcp-runtime-v1\0");
    hash_field(&mut hasher, catalog_digest.as_bytes());
    for server in servers {
        if server.id.is_empty()
            || server.id.len() > 32
            || !ids.insert(server.id.clone())
            || !valid_digest(&server.launch_digest)
        {
            return None;
        }
        hash_field(&mut hasher, server.id.as_bytes());
        hash_field(&mut hasher, server.launch_digest.as_bytes());
        hash_field(&mut hasher, server.server_info.protocol_version.as_bytes());
        hash_field(&mut hasher, server.server_info.name.as_bytes());
        hash_field(&mut hasher, server.server_info.version.as_bytes());
        hash_field(
            &mut hasher,
            &serde_json::to_vec(&server.server_info.capabilities).ok()?,
        );
        hash_field(
            &mut hasher,
            server
                .server_info
                .instructions
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Bind all verified stdio and modern HTTPS servers to the frozen catalog.
/// Retains the original digest for stdio-only turns so prior safe markers can
/// still be checked after an upgrade. Returns None if any identity is malformed.
pub fn mcp_runtime_digest(
    servers: &[McpServerRuntimeIdentity],
    catalog_digest: &str,
) -> Option<String> {
    if servers.is_empty() || servers.len() > MAX_SERVERS || !valid_digest(catalog_digest) {
        return None;
    }
    if servers
        .iter()
        .all(|identity| matches!(identity, McpServerRuntimeIdentity::Stdio(_)))
    {
        let stdio = servers
            .iter()
            .map(|identity| match identity {
                McpServerRuntimeIdentity::Stdio(identity) => identity.clone(),
                McpServerRuntimeIdentity::Http(_) => unreachable!(),
            })
            .collect::<Vec<_>>();
        return mcp_stdio_runtime_digest(&stdio, catalog_digest);
    }
    let mut ordered = servers.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| server_id(a).cmp(server_id(b)));
    let mut ids = BTreeSet::new();
    let mut hasher = Sha256::new();
    hasher.update(b"openide-mcp-runtime-mixed-v1\0");
    hash_field(&mut hasher, catalog_digest.as_bytes());
    for identity in ordered {
        let id = server_id(identity);
        if id.is_empty() || id.len() > 32 || !ids.insert(id) {
            return None;
        }
        hash_field(&mut hasher, id.as_bytes());
        match identity {
            McpServerRuntimeIdentity::Stdio(stdio) => {
                if !valid_digest(&stdio.launch_digest) {
                    return None;
                }
                hash_field(&mut hasher, b"stdio");
                hash_field(&mut hasher, stdio.launch_digest.as_bytes());
                hash_server_info(&mut hasher, &stdio.server_info)?;
            }
            McpServerRuntimeIdentity::Http(http) => {
                let endpoint = Url::parse(&http.endpoint).ok()?;
                if endpoint.scheme() != "https"
                    || is_loopback_host(&endpoint)
                    || endpoint.as_str() != http.endpoint
                    || endpoint.host_str().is_none()
                    || endpoint.query().is_some()
                    || endpoint.fragment().is_some()
                    || !endpoint.username().is_empty()
                    || endpoint.password().is_some()
                    || http.client_name.is_empty()
                    || http.client_version.is_empty()
                    || http.request_timeout_ms == 0
                    || http.max_message_bytes == 0
                    || http.server_info.protocol_version != MCP_HTTP_PROTOCOL_VERSION
                    || http.server_info.name.is_empty()
                    || http.server_info.version.is_empty()
                {
                    return None;
                }
                match &http.authorization {
                    None => hash_field(&mut hasher, b"https-stateless-unauthenticated"),
                    Some(McpHttpAuthIdentity::Bearer(digest)) if valid_digest(digest) => {
                        hash_field(&mut hasher, b"https-stateless-bearer-v1");
                        hash_field(&mut hasher, digest.as_bytes());
                    }
                    Some(McpHttpAuthIdentity::OAuth(digest)) if valid_digest(digest) => {
                        hash_field(&mut hasher, b"https-stateless-oauth-v1");
                        hash_field(&mut hasher, digest.as_bytes());
                    }
                    Some(_) => return None,
                }
                hash_field(&mut hasher, http.endpoint.as_bytes());
                hash_field(&mut hasher, http.client_name.as_bytes());
                hash_field(&mut hasher, http.client_version.as_bytes());
                hash_field(&mut hasher, &http.request_timeout_ms.to_be_bytes());
                hash_field(&mut hasher, &(http.max_message_bytes as u64).to_be_bytes());
                hash_server_info(&mut hasher, &http.server_info)?;
            }
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn server_id(identity: &McpServerRuntimeIdentity) -> &str {
    match identity {
        McpServerRuntimeIdentity::Stdio(stdio) => &stdio.id,
        McpServerRuntimeIdentity::Http(http) => &http.id,
    }
}

fn hash_server_info(hasher: &mut Sha256, info: &McpServerInfo) -> Option<()> {
    hash_field(hasher, info.protocol_version.as_bytes());
    hash_field(hasher, info.name.as_bytes());
    hash_field(hasher, info.version.as_bytes());
    hash_field(hasher, &serde_json::to_vec(&info.capabilities).ok()?);
    hash_field(
        hasher,
        info.instructions.as_deref().unwrap_or("").as_bytes(),
    );
    Some(())
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn hash_file(hasher: &mut Sha256, path: &Path) -> Option<()> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CODE_FILE_BYTES {
        return None;
    }
    hash_field(hasher, &metadata.len().to_be_bytes());
    let mut bytes = [0u8; 8192];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut bytes).ok()?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64)?;
        if total > MAX_CODE_FILE_BYTES {
            return None;
        }
        hasher.update(&bytes[..read]);
    }
    (total == metadata.len()).then_some(())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn indirect_launcher(path: &Path) -> bool {
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.starts_with("python") || name.starts_with("node") {
        return true;
    }
    matches!(
        name.as_str(),
        "python"
            | "python3"
            | "py"
            | "node"
            | "npx"
            | "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "deno"
            | "uv"
            | "uvx"
            | "powershell"
            | "pwsh"
            | "cmd"
            | "bash"
            | "sh"
            | "zsh"
            | "ruby"
            | "perl"
            | "java"
            | "dotnet"
            | "cargo"
            | "go"
    )
}

fn direct_binary_format(path: &Path) -> bool {
    #[cfg(windows)]
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic.starts_with(b"MZ")
        || magic == *b"\x7fELF"
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
        )
}

fn indirect_code_flag(argument: &str) -> bool {
    let lower = argument.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "-c" | "-m"
            | "-e"
            | "-file"
            | "-command"
            | "--eval"
            | "--require"
            | "--loader"
            | "--import"
            | "-jar"
            | "--package"
            | "--exec"
            | "--script"
            | "--module"
            | "--plugin"
    ) || [
        "-c ",
        "-m ",
        "-e ",
        "--eval=",
        "--require=",
        "--loader=",
        "--import=",
        "--package=",
        "--exec=",
        "--script=",
        "--module=",
        "--plugin=",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn secret_bearing_argument(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "api-key",
        "apikey",
        "bearer",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn launch_fingerprint_tracks_executable_and_file_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join(if cfg!(windows) {
            "server.exe"
        } else {
            "server"
        });
        let script = temp.path().join("config.js");
        fs::write(&executable, b"MZbinary-one").unwrap();
        fs::write(&script, b"module-one").unwrap();
        let mut config = McpStdioConfig::new(&executable);
        config.cwd = Some(temp.path().into());
        config.args.push("config.js".into());
        let first = inspect_mcp_stdio_launch(&config).unwrap();
        assert_eq!(first.canonical_command, executable.canonicalize().unwrap());
        fs::write(&script, b"module-two").unwrap();
        assert_ne!(
            first.digest,
            inspect_mcp_stdio_launch(&config).unwrap().digest
        );
        fs::write(&executable, b"MZbinary-two").unwrap();
        assert_ne!(
            first.digest,
            inspect_mcp_stdio_launch(&config).unwrap().digest
        );
    }

    #[test]
    fn indirect_or_credentialed_launch_has_no_identity() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join(if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        });
        fs::write(&executable, b"MZinterpreter").unwrap();
        let mut config = McpStdioConfig::new(&executable);
        config.cwd = Some(temp.path().into());
        assert!(inspect_mcp_stdio_launch(&config).is_none());
        let direct = temp.path().join(if cfg!(windows) {
            "server.exe"
        } else {
            "server"
        });
        fs::write(&direct, b"MZdirect").unwrap();
        config.command = direct.into_os_string();
        config.env.insert("API_TOKEN".into(), "secret".into());
        assert!(inspect_mcp_stdio_launch(&config).is_none());
        config.env.clear();
        config.args.push("-m".into());
        assert!(inspect_mcp_stdio_launch(&config).is_none());
        config.args.clear();
        config.args.push("--api-key=guessable".into());
        assert!(inspect_mcp_stdio_launch(&config).is_none());
        config.args.clear();
        config.args.push("guessable".into());
        assert!(inspect_mcp_stdio_launch(&config).is_none());
    }

    #[test]
    fn runtime_digest_binds_handshake_catalog_and_all_server_ids() {
        let server = McpStdioServerIdentity {
            id: "tools".into(),
            launch_digest: "a".repeat(64),
            server_info: McpServerInfo {
                protocol_version: "2025-11-25".into(),
                name: "fixture".into(),
                version: "1".into(),
                capabilities: json!({"tools":{}}),
                instructions: None,
            },
        };
        let digest =
            mcp_stdio_runtime_digest(std::slice::from_ref(&server), &"b".repeat(64)).unwrap();
        let mut changed = server.clone();
        changed.server_info.version = "2".into();
        assert_ne!(
            digest,
            mcp_stdio_runtime_digest(&[changed], &"b".repeat(64)).unwrap()
        );
        assert_ne!(
            digest,
            mcp_stdio_runtime_digest(std::slice::from_ref(&server), &"c".repeat(64)).unwrap()
        );
        assert!(mcp_stdio_runtime_digest(&[server.clone(), server], &"b".repeat(64)).is_none());
    }

    #[test]
    fn mixed_runtime_binds_https_authority_handshake_and_catalog() {
        let info = McpServerInfo {
            protocol_version: "2026-07-28".into(),
            name: "remote tools".into(),
            version: "1".into(),
            capabilities: json!({"tools": {}}),
            instructions: None,
        };
        let http = McpHttpServerIdentity::new(
            "remote".into(),
            "https://example.com/mcp".into(),
            ("openide".into(), "1".into()),
            (30_000, 8 * 1024 * 1024),
            None,
            info.clone(),
        );
        let catalog = "a".repeat(64);
        let digest =
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(http.clone())], &catalog).unwrap();
        let mut bearer = http.clone();
        bearer.authorization = Some(McpHttpAuthIdentity::Bearer("1".repeat(64)));
        let bearer_digest =
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(bearer.clone())], &catalog)
                .unwrap();
        assert_ne!(digest, bearer_digest);
        bearer.authorization = Some(McpHttpAuthIdentity::Bearer("2".repeat(64)));
        assert_ne!(
            bearer_digest,
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(bearer.clone())], &catalog)
                .unwrap()
        );
        bearer.authorization = Some(McpHttpAuthIdentity::OAuth("1".repeat(64)));
        assert_ne!(
            bearer_digest,
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(bearer.clone())], &catalog)
                .unwrap()
        );
        bearer.authorization = Some(McpHttpAuthIdentity::OAuth("invalid".into()));
        assert!(mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(bearer)], &catalog).is_none());
        let mut changed = http.clone();
        changed.endpoint = "https://elsewhere.example/mcp".into();
        assert_ne!(
            digest,
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(changed)], &catalog).unwrap()
        );
        let mut changed = http.clone();
        changed.server_info.version = "2".into();
        assert_ne!(
            digest,
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(changed)], &catalog).unwrap()
        );
        assert_ne!(
            digest,
            mcp_runtime_digest(
                &[McpServerRuntimeIdentity::Http(http.clone())],
                &"b".repeat(64)
            )
            .unwrap()
        );
        let mut changed = http.clone();
        changed.endpoint = "http://127.0.0.1/mcp".into();
        assert!(mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(changed)], &catalog).is_none());
        let mut changed = http.clone();
        changed.server_info.protocol_version = "2025-11-25".into();
        assert!(mcp_runtime_digest(&[McpServerRuntimeIdentity::Http(changed)], &catalog).is_none());
        assert!(mcp_runtime_digest(
            &[
                McpServerRuntimeIdentity::Http(http.clone()),
                McpServerRuntimeIdentity::Http(http),
            ],
            &catalog
        )
        .is_none());
        let stdio = McpServerRuntimeIdentity::Stdio(McpStdioServerIdentity {
            id: "local".into(),
            launch_digest: "c".repeat(64),
            server_info: info,
        });
        let http = McpServerRuntimeIdentity::Http(McpHttpServerIdentity::new(
            "remote".into(),
            "https://example.com/mcp".into(),
            ("openide".into(), "1".into()),
            (30_000, 8 * 1024 * 1024),
            None,
            McpServerInfo {
                protocol_version: "2026-07-28".into(),
                name: "remote tools".into(),
                version: "1".into(),
                capabilities: json!({"tools": {}}),
                instructions: None,
            },
        ));
        assert_eq!(
            mcp_runtime_digest(&[stdio.clone(), http.clone()], &catalog),
            mcp_runtime_digest(&[http, stdio], &catalog)
        );
    }

    #[test]
    fn stdio_only_runtime_retains_its_previous_digest() {
        let stdio = McpStdioServerIdentity {
            id: "local".into(),
            launch_digest: "a".repeat(64),
            server_info: McpServerInfo {
                protocol_version: "2025-11-25".into(),
                name: "local tools".into(),
                version: "1".into(),
                capabilities: json!({"tools": {}}),
                instructions: None,
            },
        };
        let catalog = "b".repeat(64);
        assert_eq!(
            mcp_runtime_digest(&[McpServerRuntimeIdentity::Stdio(stdio.clone())], &catalog),
            mcp_stdio_runtime_digest(&[stdio], &catalog)
        );
    }

    #[test]
    fn authenticated_http_digest_revalidates_across_reconstructed_client() {
        let info = McpServerInfo {
            protocol_version: MCP_HTTP_PROTOCOL_VERSION.into(),
            name: "authenticated service".into(),
            version: "1".into(),
            capabilities: json!({"tools": {}}),
            instructions: None,
        };
        let identity = |authorization| {
            McpServerRuntimeIdentity::Http(McpHttpServerIdentity::new(
                "remote".into(),
                "https://example.test/mcp".into(),
                ("openide".into(), "1".into()),
                (30_000, 8 * 1024 * 1024),
                Some(authorization),
                info.clone(),
            ))
        };
        let catalog = "c".repeat(64);
        let original = mcp_runtime_digest(
            &[identity(McpHttpAuthIdentity::OAuth("a".repeat(64)))],
            &catalog,
        )
        .unwrap();
        let after_restart = mcp_runtime_digest(
            &[identity(McpHttpAuthIdentity::OAuth("a".repeat(64)))],
            &catalog,
        )
        .unwrap();
        assert_eq!(original, after_restart);
        assert_ne!(
            original,
            mcp_runtime_digest(
                &[identity(McpHttpAuthIdentity::OAuth("b".repeat(64)))],
                &catalog
            )
            .unwrap()
        );
    }
}
