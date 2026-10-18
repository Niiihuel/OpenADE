//! How OpenIDE points a known agent CLI at the bridge when it starts one.
//!
//! The token never goes on the command line, where other local users could
//! read it: Claude Code gets a private config file, Codex an environment
//! variable.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

const SERVER_NAME: &str = "openide";
const TOKEN_ENV: &str = "OPENIDE_BRIDGE_TOKEN";
/// A delegated task answers when its turn ends, which takes minutes; the
/// CLIs' own limits (Codex: 60 s) would drop the answer.
const TOOL_TIMEOUT_SECONDS: u64 = 3600;

/// How a CLI is told about the bridge. Which CLI uses which is host data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeKind {
    /// A private JSON file passed with `--mcp-config`.
    ClaudeMcpConfig,
    /// `-c mcp_servers...` overrides with the token in an environment variable.
    CodexConfig,
}

/// Extra arguments and environment for one CLI session.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, String)>,
    /// Holds the token; delete it when the session's terminal closes.
    pub config_file: Option<PathBuf>,
}

impl BridgeKind {
    /// `config_dir` must be private to the user; it receives the Claude Code
    /// config file. `url` is the bridge's `/mcp` endpoint.
    pub fn bridge_launch(self, url: &str, token: &str, config_dir: &Path) -> io::Result<Launch> {
        match self {
            Self::ClaudeMcpConfig => {
                let path = config_dir.join(format!("claude-mcp-{}.json", uuid::Uuid::new_v4().simple()));
                let config = json!({
                    "mcpServers": {
                        SERVER_NAME: {
                            "type": "http",
                            "url": url,
                            "headers": { "Authorization": format!("Bearer {token}") }
                        }
                    }
                });
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                options.open(&path)?.write_all(config.to_string().as_bytes())?;
                Ok(Launch {
                    args: vec!["--mcp-config".into(), path.clone().into()],
                    env: vec![("MCP_TOOL_TIMEOUT", (TOOL_TIMEOUT_SECONDS * 1000).to_string())],
                    config_file: Some(path),
                })
            }
            Self::CodexConfig => Ok(Launch {
                args: vec![
                    "-c".into(),
                    // `-c` values are TOML, and a JSON string is a valid TOML string here.
                    format!("mcp_servers.{SERVER_NAME}.url={}", json!(url)).into(),
                    "-c".into(),
                    format!("mcp_servers.{SERVER_NAME}.bearer_token_env_var=\"{TOKEN_ENV}\"").into(),
                    "-c".into(),
                    format!("mcp_servers.{SERVER_NAME}.tool_timeout_sec={TOOL_TIMEOUT_SECONDS}").into(),
                ],
                env: vec![(TOKEN_ENV, token.to_owned())],
                config_file: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_stays_out_of_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1:4100/mcp";
        for kind in [BridgeKind::ClaudeMcpConfig, BridgeKind::CodexConfig] {
            let launch = kind.bridge_launch(url, "s3cret", dir.path()).unwrap();
            assert!(launch
                .args
                .iter()
                .all(|arg| !arg.to_string_lossy().contains("s3cret")));
        }

        let claude = BridgeKind::ClaudeMcpConfig.bridge_launch(url, "s3cret", dir.path()).unwrap();
        let path = claude.config_file.unwrap();
        let config: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["openide"]["url"], url);
        assert_eq!(config["mcpServers"]["openide"]["headers"]["Authorization"], "Bearer s3cret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }

        let codex = BridgeKind::CodexConfig.bridge_launch(url, "s3cret", dir.path()).unwrap();
        assert_eq!(codex.env, vec![("OPENIDE_BRIDGE_TOKEN", "s3cret".to_owned())]);
        assert_eq!(
            codex.args[1].to_string_lossy(),
            r#"mcp_servers.openide.url="http://127.0.0.1:4100/mcp""#
        );
    }

    #[test]
    fn kinds_are_named_in_host_data_by_snake_case() {
        let kinds: Vec<BridgeKind> =
            serde_json::from_str(r#"["claude_mcp_config","codex_config"]"#).unwrap();
        assert_eq!(kinds, vec![BridgeKind::ClaudeMcpConfig, BridgeKind::CodexConfig]);
        assert!(serde_json::from_str::<BridgeKind>(r#""shell""#).is_err());
    }
}
