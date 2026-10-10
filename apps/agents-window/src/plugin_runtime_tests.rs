//! Offline acceptance of installed packages through the existing MCP runtime.

use super::{McpServerSettings, McpSettings, MAX_SERVERS, SETTINGS_FILE};
use crate::plugins::PluginStore;
use serde_json::json;
use std::{fs, path::Path};

fn write_package(source: &Path, arguments: Vec<String>) {
    fs::write(
        source.join("openide-plugin.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "id": "echo",
            "name": "Echo local",
            "version": "1.0.0",
            "description": "Herramienta local para verificar el ciclo del plugin",
            "publisher": "OpenIDE tests",
            "program": "sh",
            "entrypoint": "server.sh",
            "args": arguments,
            "credential_env_names": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(source.join("server.sh"), "exit 0\n").unwrap();
}

fn install(root: &Path, workspace: &Path, source: &Path) {
    let preview = PluginStore::inspect(source).unwrap();
    PluginStore::install(root, workspace, &preview).unwrap();
}

fn manual_server(id: &str) -> McpServerSettings {
    McpServerSettings {
        id: id.into(),
        enabled: true,
        program: "sh".into(),
        args: Vec::new(),
        cwd: None,
        credential_env_names: Vec::new(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
    }
}

#[test]
fn runtime_plugins_follow_workspace_activation_without_rewriting_mcp_preferences() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    write_package(source.path(), Vec::new());
    let configured = McpSettings {
        servers: vec![manual_server("manual")],
    };
    configured.save(root.path(), workspace.path()).unwrap();
    let saved = fs::read(root.path().join(SETTINGS_FILE)).unwrap();
    install(root.path(), workspace.path(), source.path());

    assert_eq!(
        McpSettings::load_runtime(root.path(), workspace.path()).unwrap(),
        configured,
        "installation alone must not activate a process"
    );
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).unwrap();
    let frozen = McpSettings::load_runtime(root.path(), workspace.path()).unwrap();
    assert_eq!(
        frozen
            .enabled_servers()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["manual", "plugin_echo"]
    );
    assert_eq!(
        McpSettings::load_runtime(root.path(), other_workspace.path()).unwrap(),
        configured
    );
    assert_eq!(
        McpSettings::load(root.path(), workspace.path()).unwrap(),
        configured,
        "the MCP editor must continue to own only manually configured servers"
    );
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", false).unwrap();
    assert_eq!(
        McpSettings::load_runtime(root.path(), workspace.path()).unwrap(),
        configured
    );
    assert_eq!(frozen.enabled_servers().count(), 2);
    assert_eq!(fs::read(root.path().join(SETTINGS_FILE)).unwrap(), saved);
}

#[test]
fn runtime_merge_rejects_plugin_namespace_collisions_and_combined_server_limit() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    write_package(source.path(), Vec::new());
    install(root.path(), workspace.path(), source.path());
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).unwrap();
    let conflict = McpSettings {
        servers: vec![manual_server("plugin_echo")],
    };
    conflict.save(root.path(), workspace.path()).unwrap();
    let error = McpSettings::load_runtime(root.path(), workspace.path()).unwrap_err();
    assert!(error.contains("duplicado"), "{error}");
    assert_eq!(
        McpSettings::load(root.path(), workspace.path()).unwrap(),
        conflict
    );
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", false).unwrap();
    assert!(PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).is_err());
    assert!(!PluginStore::list(root.path(), workspace.path()).unwrap()[0].enabled);

    let mut configured = McpSettings {
        servers: (0..MAX_SERVERS - 1)
            .map(|index| manual_server(&format!("manual_{index}")))
            .collect(),
    };
    configured.save(root.path(), workspace.path()).unwrap();
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).unwrap();
    assert_eq!(
        McpSettings::load_runtime(root.path(), workspace.path())
            .unwrap()
            .servers
            .len(),
        MAX_SERVERS
    );
    configured.servers.push(manual_server("last_manual"));
    configured.save(root.path(), workspace.path()).unwrap();
    let error = McpSettings::load_runtime(root.path(), workspace.path()).unwrap_err();
    assert!(error.contains("servidores MCP"), "{error}");
    assert_eq!(
        McpSettings::load(root.path(), workspace.path()).unwrap(),
        configured
    );
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", false).unwrap();
    assert!(PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).is_err());
    assert!(!PluginStore::list(root.path(), workspace.path()).unwrap()[0].enabled);
}

#[cfg(unix)]
#[test]
fn installed_plugin_is_not_started_in_plan_and_build_asks_before_call_then_shuts_down() {
    use crate::environment_runtime::EnvironmentToolPreparation;
    use async_trait::async_trait;
    use openide_core::{AgentEventKind, AgentMessage, AgentRunRequest, ToolCall, TurnMode};
    use openide_harness::{
        AgentError, AgentRunner, LocalTools, ModelBackend, ModelOutput, PermissionChoice,
        ToolDefinition,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    struct FixtureModel {
        build: bool,
        step: AtomicUsize,
    }

    #[async_trait]
    impl ModelBackend for FixtureModel {
        fn model_id(&self) -> &str {
            "plugin-offline-fixture"
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            assert_eq!(
                tools
                    .iter()
                    .any(|tool| tool.name == "mcp__plugin_echo__echo"),
                self.build
            );
            if self.build && self.step.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(ModelOutput {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "plugin-call".into(),
                        name: "mcp__plugin_echo__echo".into(),
                        arguments: json!({}),
                    }],
                });
            }
            if self.build {
                assert_eq!(messages.last().unwrap().content, "plugin-ok");
            }
            Ok(ModelOutput {
                content: "done".into(),
                tool_calls: Vec::new(),
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let log = workspace.path().join("plugin-process.log");
    write_package(source.path(), vec![log.to_string_lossy().into_owned()]);
    let initialize = json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","serverInfo":{"name":"plugin fixture","version":"1"},"capabilities":{"tools":{}}}});
    let list = json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object","additionalProperties":false}}]}});
    let call = json!({"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"plugin-ok"}],"isError":false}});
    fs::write(
        source.path().join("server.sh"),
        format!(
            r#"log="$1"
printf 'started\n' >> "$log"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{initialize}' ;;
    *'"method":"tools/list"'*) printf '%s\n' '{list}' ;;
    *'"method":"tools/call"'*) printf 'called\n' >> "$log"; printf '%s\n' '{call}' ;;
  esac
done
printf 'closed\n' >> "$log"
"#
        ),
    )
    .unwrap();
    install(root.path(), workspace.path(), source.path());
    PluginStore::set_enabled(root.path(), workspace.path(), "echo", true).unwrap();
    assert!(
        !log.exists(),
        "installing and enabling must never execute the package"
    );

    for mode in [TurnMode::Plan, TurnMode::Build] {
        let settings = McpSettings::load(root.path(), workspace.path()).unwrap();
        assert!(
            !log.exists(),
            "reading runtime configuration must be passive"
        );
        let runner = AgentRunner::new(
            Arc::new(FixtureModel {
                build: mode == TurnMode::Build,
                step: AtomicUsize::new(0),
            }),
            Arc::new(LocalTools::new()),
        )
        .with_tool_preparation(Arc::new(
            EnvironmentToolPreparation::new(
                root.path().to_path_buf(),
                "plugin-fixture".into(),
                Arc::new(Vec::new()),
                settings.servers,
                None,
            )
            .with_plugins(root.path().to_path_buf()),
        ));
        let handle = runner.start(AgentRunRequest {
            session_id: Some("plugin-fixture".into()),
            sequence_start: 0,
            workspace: workspace.path().to_path_buf(),
            history: Vec::new(),
            prompt: "Verify installed plugin".into(),
            mode,
        });
        let mut approvals = 0;
        loop {
            let event = handle
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("plugin turn terminal event");
            match event.kind {
                AgentEventKind::PermissionRequested { request_id, .. } => {
                    approvals += 1;
                    assert_eq!(fs::read_to_string(&log).unwrap(), "started\n");
                    assert!(handle.resolve_permission(&request_id, PermissionChoice::Allow));
                }
                AgentEventKind::TurnCompleted { response, .. } => {
                    assert_eq!(response, "done");
                    break;
                }
                AgentEventKind::TurnFailed { .. } | AgentEventKind::TurnCancelled => {
                    panic!("plugin fixture did not complete: {:?}", event.kind);
                }
                _ => {}
            }
        }
        if mode == TurnMode::Plan {
            assert_eq!(approvals, 0);
            assert!(!log.exists(), "Plan must not initialize external plugins");
        } else {
            assert_eq!(approvals, 1);
            assert_eq!(
                fs::read_to_string(&log).unwrap(),
                "started\ncalled\nclosed\n"
            );
        }
    }
}
