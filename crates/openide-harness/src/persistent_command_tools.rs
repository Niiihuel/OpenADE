//! Model-facing adapter for sessions already scoped to a trusted workspace.

use super::{ToolContext, ToolDefinition, ToolOutput};
use crate::{
    AgentError, PersistentCommandEvent, PersistentCommandExit, PersistentCommandManager,
    PersistentCommandSnapshot, PersistentCommandStream,
};
use base64::Engine;
use openide_core::ToolCall;
use serde_json::{json, Map, Value};
use std::time::Duration;
use uuid::Uuid;

const MAX_ARGUMENT_JSON_BYTES: usize = 64 * 1024;
const MAX_RESULT_JSON_BYTES: usize = 128 * 1024;
const TOOL_POLL_OUTPUT_BYTES: usize = 8 * 1024;
const MAX_POLL_WAIT_MS: u64 = 30_000;
const WAIT_FIELD: &str = "timeout_ms";

pub(crate) fn is_persistent_tool(name: &str) -> bool {
    matches!(
        name,
        "start_command" | "poll_command" | "write_stdin" | "terminate_command" | "resize_command"
    )
}

pub(crate) fn definitions(manager: &PersistentCommandManager) -> Vec<ToolDefinition> {
    definitions_for_profile(
        manager.isolation(),
        manager.pty_tools_enabled(),
        manager.pty_resize_tools_enabled(),
        manager.start_wait_tools_enabled(),
    )
}

/// Pure catalog construction; its flags are the host's effective capabilities.
/// No manager, process, output registration or runtime is created here.
pub(crate) fn definitions_for_profile(
    isolation: crate::CommandIsolation,
    pty_enabled: bool,
    resize_enabled: bool,
    wait_fields: bool,
) -> Vec<ToolDefinition> {
    let mut definitions = vec![
        legacy_start_command_definition(),
        ToolDefinition {
            name: "poll_command".into(),
            description: "Read bounded output and process status from a persistent command. Repeat until finished is true; omission markers mean some output was not retained.".into(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "session_id":{"type":"string","format":"uuid","maxLength":36},
                    "wait_ms":{"type":"integer","minimum":0,"maximum":30000}
                },
                "required":["session_id"],
                "additionalProperties":false
            }),
        },
        ToolDefinition {
            name: "write_stdin".into(),
            description: "Write UTF-8 text to a persistent command's stdin, optionally closing stdin afterward. Input may have effects and requires approval. Do not enter secrets.".into(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "session_id":{"type":"string","format":"uuid","maxLength":36},
                    "input":{"type":"string","maxLength":8192},
                    "close_after":{"type":"boolean"}
                },
                "required":["session_id","input"],
                "additionalProperties":false
            }),
        },
        ToolDefinition {
            name: "terminate_command".into(),
            description: "Terminate a persistent command and its descendants, preserving remaining output for poll_command. Requires approval.".into(),
            parameters: json!({
                "type":"object",
                "properties":{"session_id":{"type":"string","format":"uuid","maxLength":36}},
                "required":["session_id"],
                "additionalProperties":false
            }),
        },
    ];
    if isolation == crate::CommandIsolation::WorkspaceWriteOffline {
        definitions[0].description = if cfg!(windows) {
            "Start a persistent command inside the Windows workspace-write/offline OS sandbox. Uses ordinary stdin, stdout, and stderr pipes; no PTY or implicit shell. Requires approval."
        } else {
            "Start a persistent command inside the workspace-write/offline OS sandbox. Requires Bubblewrap and working user namespaces on Linux. Only the workspace, mounted system runtime and private temporary files are visible; network sockets are blocked. Uses ordinary pipes; no PTY, implicit shell or unrestricted fallback. Requires approval."
        }.into();
    } else if pty_enabled {
        definitions[0] = ToolDefinition {
            name: "start_command".into(),
            description: "Start a persistent command in this workspace with host filesystem and network rights. No implicit shell or inherited secret environment. Set pty=true for a Windows terminal with merged output. Requires approval.".into(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string","minLength":1,"maxLength":32768},
                    "args":{"type":"array","items":{"type":"string"},"maxItems":256},
                    "pty":{"type":"boolean"},
                    "columns":{"type":"integer","minimum":20,"maximum":500},
                    "rows":{"type":"integer","minimum":5,"maximum":200}
                },
                "required":["command"],
                "additionalProperties":false
            }),
        };
    }
    if resize_enabled {
        definitions.push(ToolDefinition {
            name: "resize_command".into(),
            description: "Resize a running Windows PTY to the specified columns and rows. Pipe and sandboxed commands cannot be resized. Requires approval.".into(),
            parameters: json!({
                "type":"object",
                "properties":{
                    "session_id":{"type":"string","format":"uuid","maxLength":36},
                    "columns":{"type":"integer","minimum":20,"maximum":500},
                    "rows":{"type":"integer","minimum":5,"maximum":200}
                },
                "required":["session_id","columns","rows"],
                "additionalProperties":false
            }),
        });
    }
    if wait_fields {
        // Added last and only as a property and a description suffix:
        // removing them restores, byte for byte, the catalog an interrupted
        // turn or reserved child recorded.
        definitions[0]
            .description
            .push_str(crate::command_line::COMMAND_RULE);
        for definition in &mut definitions {
            let description = match definition.name.as_str() {
                "start_command" => "How long to wait for initial output before returning, in milliseconds. This is not a lifetime: the command keeps running after the call returns. Default 0 returns the session immediately; read later output with poll_command.",
                "poll_command" => "Alias of wait_ms, accepted because run_command names it this way. Send only one of them.",
                _ => continue,
            };
            definition.parameters["properties"][WAIT_FIELD] = json!({
                "type":"integer","minimum":0,"maximum":MAX_POLL_WAIT_MS,"description":description
            });
        }
    }
    definitions
}

#[cfg(test)]
use crate::command_line::definition_before_command_revision;

pub(crate) fn legacy_start_command_definition() -> ToolDefinition {
    ToolDefinition {
        name: "start_command".into(),
        description: "Start a persistent command in this workspace with host filesystem and network rights. No implicit shell or inherited secret environment. Requires approval.".into(),
        parameters: json!({
            "type":"object",
            "properties":{
                "command":{"type":"string","minLength":1,"maxLength":32768},
                "args":{"type":"array","items":{"type":"string"},"maxItems":256}
            },
            "required":["command"],
            "additionalProperties":false
        }),
    }
}

pub(crate) async fn execute(
    manager: &PersistentCommandManager,
    call: &ToolCall,
    context: &ToolContext,
) -> Result<ToolOutput, AgentError> {
    let workspace = context.workspace.canonicalize().map_err(|error| {
        AgentError::Tool(format!("cannot resolve active command workspace: {error}"))
    })?;
    if workspace != manager.workspace() {
        return Err(AgentError::Tool(
            "persistent command manager belongs to another workspace".into(),
        ));
    }
    let mut allowed: Vec<&str> = match call.name.as_str() {
        "start_command" if manager.pty_tools_enabled() => {
            vec!["command", "args", "pty", "columns", "rows"]
        }
        "start_command" => vec!["command", "args"],
        "poll_command" => vec!["session_id", "wait_ms"],
        "write_stdin" => vec!["session_id", "input", "close_after"],
        "terminate_command" => vec!["session_id"],
        "resize_command" => vec!["session_id", "columns", "rows"],
        _ => unreachable!("only known persistent tools are routed here"),
    };
    if manager.start_wait_tools_enabled()
        && matches!(call.name.as_str(), "start_command" | "poll_command")
    {
        allowed.push(WAIT_FIELD);
    }
    // Run what the approval was computed on: plain words split, no shell.
    let split = match call.name.as_str() {
        "start_command" => crate::command_line::executed_call(call)?,
        _ => None,
    };
    let call = split.as_ref().unwrap_or(call);
    let arguments = arguments(&call.arguments, &allowed)?;
    match call.name.as_str() {
        "start_command" => {
            let program = required_string(arguments, "command")?;
            let args = match arguments.get("args") {
                None => Vec::new(),
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| AgentError::Tool("command args must be strings".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(AgentError::Tool("command args must be an array".into())),
            };
            let pty = match arguments.get("pty") {
                None | Some(Value::Bool(false)) => false,
                Some(Value::Bool(true)) => true,
                Some(_) => return Err(AgentError::Tool("pty must be boolean".into())),
            };
            let wait_ms = wait_argument(arguments, WAIT_FIELD)?.unwrap_or(0);
            let started = if pty {
                let columns = terminal_dimension(arguments, "columns", Some(80), 20, 500)?;
                let rows = terminal_dimension(arguments, "rows", Some(24), 5, 200)?;
                manager.start_pty(program, &args, columns, rows, &context.cancel)?
            } else {
                if arguments.contains_key("columns") || arguments.contains_key("rows") {
                    return Err(AgentError::Tool("PTY dimensions require pty=true".into()));
                }
                manager.start(program, &args, &context.cancel)?
            };
            // Say what ran when the line was split, in every result shape.
            let executed = |mut output: ToolOutput| {
                if split.is_some() {
                    if let Ok(Value::Object(mut content)) = serde_json::from_str(&output.content) {
                        content.insert(
                            "executed".into(),
                            crate::command_line::executed_words(call).into(),
                        );
                        output.content = Value::Object(content).to_string();
                    }
                }
                output
            };
            if wait_ms > 0 {
                // The command already runs: a failed or cancelled wait must
                // still hand its session to the model, so fall through.
                if let Ok(snapshot) = manager
                    .poll_with_output_limit(
                        &started.id,
                        Duration::from_millis(wait_ms),
                        TOOL_POLL_OUTPUT_BYTES.min(manager.poll_output_limit()),
                        &context.cancel,
                    )
                    .await
                {
                    return snapshot_output(snapshot).map(executed);
                }
            }
            Ok(executed(ToolOutput::success(if pty {
                json!({"session_id":started.id,"pty":true}).to_string()
            } else {
                json!({"session_id":started.id}).to_string()
            })))
        }
        "poll_command" => {
            let id = session_id(arguments)?;
            let wait_ms = match (
                wait_argument(arguments, "wait_ms")?,
                wait_argument(arguments, WAIT_FIELD)?,
            ) {
                (Some(_), Some(_)) => {
                    return Err(AgentError::Tool(
                        "send wait_ms or timeout_ms, not both".into(),
                    ))
                }
                (wait, alias) => wait.or(alias).unwrap_or(1000),
            };
            let snapshot = manager
                .poll_with_output_limit(
                    &id,
                    Duration::from_millis(wait_ms),
                    TOOL_POLL_OUTPUT_BYTES.min(manager.poll_output_limit()),
                    &context.cancel,
                )
                .await?;
            snapshot_output(snapshot)
        }
        "write_stdin" => {
            let id = session_id(arguments)?;
            let input = required_string(arguments, "input")?;
            let close_after = match arguments.get("close_after") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Err(AgentError::Tool("close_after must be boolean".into())),
            };
            manager
                .write_stdin(&id, input.as_bytes(), close_after, &context.cancel)
                .await?;
            Ok(ToolOutput::success(
                json!({"session_id":id,"stdin_closed":close_after}).to_string(),
            ))
        }
        "terminate_command" => {
            let id = session_id(arguments)?;
            manager.terminate(&id).await?;
            Ok(ToolOutput::success(
                json!({"session_id":id,"terminated":true,"poll_remaining_output":true}).to_string(),
            ))
        }
        "resize_command" => {
            let id = session_id(arguments)?;
            let columns = terminal_dimension(arguments, "columns", None, 20, 500)?;
            let rows = terminal_dimension(arguments, "rows", None, 5, 200)?;
            manager
                .resize_pty(&id, columns, rows, &context.cancel)
                .await?;
            Ok(ToolOutput::success(
                json!({"session_id":id,"columns":columns,"rows":rows,"resized":true}).to_string(),
            ))
        }
        _ => unreachable!("only known persistent tools are routed here"),
    }
}

fn arguments<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, AgentError> {
    if serde_json::to_vec(value)
        .map_err(|_| AgentError::Tool("cannot encode tool arguments".into()))?
        .len()
        > MAX_ARGUMENT_JSON_BYTES
    {
        return Err(AgentError::Tool(
            "persistent command arguments exceed JSON limit".into(),
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| AgentError::Tool("tool arguments must be an object".into()))?;
    if let Some(unknown) = object
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        // Name the field so the model can correct the call instead of guessing.
        let unknown: String = unknown.chars().take(64).collect();
        return Err(AgentError::Tool(format!(
            "persistent command arguments contain an unknown field: {unknown}; allowed fields: {}",
            allowed.join(", ")
        )));
    }
    Ok(object)
}

fn required_string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, AgentError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Tool(format!("missing string argument: {key}")))
}

fn wait_argument(arguments: &Map<String, Value>, name: &str) -> Result<Option<u64>, AgentError> {
    arguments
        .get(name)
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value <= MAX_POLL_WAIT_MS)
                .ok_or_else(|| AgentError::Tool(format!("{name} must be between 0 and 30000")))
        })
        .transpose()
}

fn terminal_dimension(
    arguments: &Map<String, Value>,
    name: &str,
    default: Option<u16>,
    min: u16,
    max: u16,
) -> Result<u16, AgentError> {
    match arguments.get(name) {
        None => default.ok_or_else(|| AgentError::Tool(format!("missing PTY {name}"))),
        Some(value) => value
            .as_u64()
            .and_then(|number| u16::try_from(number).ok())
            .filter(|number| (min..=max).contains(number))
            .ok_or_else(|| AgentError::Tool(format!("invalid PTY {name}"))),
    }
}

fn session_id(arguments: &Map<String, Value>) -> Result<String, AgentError> {
    let raw = required_string(arguments, "session_id")?;
    Uuid::parse_str(raw)
        .map(|id| id.to_string())
        .map_err(|_| AgentError::Tool("invalid persistent command session ID".into()))
}

fn snapshot_output(snapshot: PersistentCommandSnapshot) -> Result<ToolOutput, AgentError> {
    let has_omission = snapshot
        .events
        .iter()
        .any(|event| matches!(event, PersistentCommandEvent::Omitted { .. }));
    let failed_exit = match &snapshot.exit {
        Some(PersistentCommandExit::Exited { code: Some(0) }) | None => false,
        Some(_) => true,
    };
    let is_error = has_omission || failed_exit || snapshot.reader_error.is_some();
    let events: Vec<Value> = snapshot
        .events
        .iter()
        .map(|event| match event {
            PersistentCommandEvent::Output { stream, bytes } => {
                let stream = match stream {
                    PersistentCommandStream::Stdout => "stdout",
                    PersistentCommandStream::Stderr => "stderr",
                    PersistentCommandStream::Terminal => "terminal",
                };
                match std::str::from_utf8(bytes) {
                    Ok(text) => json!({"stream":stream,"text":text}),
                    Err(_) => json!({
                        "stream":stream,
                        "base64":base64::engine::general_purpose::STANDARD.encode(bytes),
                        "encoding":"base64"
                    }),
                }
            }
            PersistentCommandEvent::Omitted { bytes } => json!({"omitted_bytes":bytes}),
        })
        .collect();
    let exit = match snapshot.exit {
        None => Value::Null,
        Some(PersistentCommandExit::Exited { code }) => json!({"kind":"exited","code":code}),
        Some(PersistentCommandExit::Terminated) => json!({"kind":"terminated"}),
        Some(PersistentCommandExit::TimedOut) => json!({"kind":"timed_out"}),
        Some(PersistentCommandExit::WaitFailed(error)) => {
            json!({"kind":"wait_failed","error":error})
        }
    };
    let content = json!({
        "session_id":snapshot.id,
        "events":events,
        "exit":exit,
        "reader_error":snapshot.reader_error,
        "has_more_output":snapshot.has_more_output,
        "finished":snapshot.finished,
    })
    .to_string();
    if content.len() > MAX_RESULT_JSON_BYTES {
        // The manager delivered at most 8 KiB and 128 events, so reaching this
        // branch would indicate a broken bound. Report the lost result rather
        // than silently returning incomplete JSON.
        return Err(AgentError::EffectUncertain(
            "persistent command output exceeded the JSON result limit after polling".into(),
        ));
    }
    Ok(ToolOutput { content, is_error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandIsolation, LocalTool, LocalTools, PersistentCommandLimits, ToolExecutor};
    #[cfg(windows)]
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn advertises_only_with_run_permission_and_host_isolation() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let disabled = LocalTools::new().with_persistent_commands(Arc::clone(&manager));
        assert!(!disabled
            .definitions()
            .iter()
            .any(|tool| is_persistent_tool(&tool.name)));
        let tools = disabled.with_tool_permission(LocalTool::RunCommand, true);
        for name in [
            "start_command",
            "poll_command",
            "write_stdin",
            "terminate_command",
        ] {
            assert!(tools.definitions().iter().any(|tool| tool.name == name));
            assert!(!tools.certifies_parallel_read(name));
        }
        crate::ToolCatalog::from_executor(&tools).unwrap();
        let offline = tools.with_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        assert!(!offline
            .definitions()
            .iter()
            .any(|tool| is_persistent_tool(&tool.name)));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn resize_catalog_is_additive_and_tool_request_is_bounded() {
        let workspace = tempfile::tempdir().unwrap();
        let base = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools();
        let previous = definitions(&base);
        assert_eq!(previous.len(), 4);
        assert!(!previous.iter().any(|tool| tool.name == "resize_command"));
        let manager = Arc::new(base.with_pty_resize_tools());
        let current = definitions(&manager);
        assert_eq!(&current[..4], previous.as_slice());
        assert_eq!(current[4].name, "resize_command");
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_command_isolation(CommandIsolation::HostUnrestricted)
            .with_persistent_commands(Arc::clone(&manager));
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let id = Uuid::new_v4().to_string();
        for arguments in [
            json!({"session_id":id,"columns":19,"rows":24}),
            json!({"session_id":id,"columns":80,"rows":201}),
            json!({"session_id":id,"columns":80}),
        ] {
            let error = tools
                .execute(&call("resize_command", arguments), &context)
                .await
                .unwrap_err();
            assert!(matches!(error, AgentError::Tool(_)));
        }
        assert!(manager.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn offline_manager_advertises_pipes_without_host_or_pty_claims() {
        if !CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::WorkspaceWriteOffline,
                PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_pty_tools(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_command_isolation(CommandIsolation::WorkspaceWriteOffline)
            .with_persistent_commands(manager);
        let definitions = tools.definitions();
        assert_eq!(
            definitions
                .iter()
                .filter(|tool| is_persistent_tool(&tool.name))
                .count(),
            4
        );
        let start = definitions
            .iter()
            .find(|definition| definition.name == "start_command")
            .unwrap();
        assert!(start.description.contains("workspace-write/offline"));
        assert!(start.parameters["properties"].get("pty").is_none());
        assert_ne!(
            start,
            &PersistentCommandManager::legacy_start_command_definition()
        );
        crate::ToolCatalog::from_executor(&tools).unwrap();
    }

    #[test]
    fn pty_catalog_is_explicit_and_legacy_definition_is_stable() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_tools_before_command_revision();
        let legacy = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(Arc::new(manager));
        let legacy_start = legacy
            .definitions()
            .into_iter()
            .find(|definition| definition.name == "start_command")
            .unwrap();
        assert_eq!(
            legacy_start,
            PersistentCommandManager::legacy_start_command_definition()
        );
        assert!(legacy_start.parameters["properties"].get("pty").is_none());

        let manager = PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_pty_tools();
        let enabled = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(Arc::new(manager));
        let enabled_start = enabled
            .definitions()
            .into_iter()
            .find(|definition| definition.name == "start_command")
            .unwrap();
        assert_eq!(
            enabled_start.parameters["properties"].get("pty").is_some(),
            cfg!(windows),
            "PTY capability must match the host implementation"
        );
        if cfg!(windows) {
            assert_ne!(legacy_start, enabled_start);
        } else {
            assert_eq!(legacy_start, definition_before_command_revision(enabled_start));
        }
        crate::ToolCatalog::from_executor(&enabled).unwrap();
    }

    #[tokio::test]
    async fn rejects_cross_workspace_and_cancelled_calls_before_start() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(manager);
        let start = call(
            "start_command",
            json!({"command":"definitely-not-a-program"}),
        );
        let wrong_workspace = ToolContext {
            workspace: other.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        assert!(matches!(
            tools.execute(&start, &wrong_workspace).await,
            Err(AgentError::Tool(message)) if message.contains("another workspace")
        ));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: cancelled,
        };
        assert!(matches!(
            tools.execute(&start, &context).await,
            Err(AgentError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_ms_waits_for_initial_output_and_is_never_a_lifetime() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(Arc::clone(&manager));
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        for name in ["start_command", "poll_command"] {
            let definition = tools
                .definitions()
                .into_iter()
                .find(|definition| definition.name == name)
                .unwrap();
            assert_eq!(
                definition.parameters["properties"]["timeout_ms"]["maximum"],
                30000
            );
            assert!(definition_before_command_revision(definition).parameters["properties"]
                .get("timeout_ms")
                .is_none());
        }
        let started = tools
            .execute(
                &call(
                    "start_command",
                    json!({"command":"/bin/sh","args":["-c","echo READY; sleep 30"],"timeout_ms":5000}),
                ),
                &context,
            )
            .await
            .unwrap();
        assert!(!started.is_error, "{}", started.content);
        let value: Value = serde_json::from_str(&started.content).unwrap();
        assert_eq!(value["events"][0]["text"], "READY\n");
        assert_eq!(value["finished"], false);
        let id = value["session_id"].as_str().unwrap().to_owned();
        // The wait elapsed without output: the command is still running.
        let polled = tools
            .execute(
                &call("poll_command", json!({"session_id":id,"timeout_ms":50})),
                &context,
            )
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&polled.content).unwrap();
        assert_eq!(value["finished"], false);
        for arguments in [
            json!({"session_id":id,"timeout_ms":50,"wait_ms":50}),
            json!({"session_id":id,"timeout_ms":30001}),
        ] {
            assert!(tools
                .execute(&call("poll_command", arguments), &context)
                .await
                .is_err());
        }
        let unknown = tools
            .execute(
                &call("start_command", json!({"command":"/bin/sh","elevated":true})),
                &context,
            )
            .await;
        assert!(
            matches!(&unknown, Err(AgentError::Tool(message)) if message.contains("unknown field: elevated") && message.contains("timeout_ms")),
            "{unknown:?}"
        );
        // A plain line is split, runs, and the result says what ran.
        let split = tools
            .execute(
                &call("start_command", json!({"command":"sh -c true","timeout_ms":5000})),
                &context,
            )
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&split.content).unwrap();
        assert_eq!(value["executed"], json!(["sh", "-c", "true"]));
        assert_eq!(value["exit"]["code"], 0);
        for (arguments, expected) in [
            (json!({"command":"sh -c 'sleep 30'"}), "pass arguments in `args`"),
            (
                json!({"command":"openide-no-such-program"}),
                "executable not found on PATH: openide-no-such-program",
            ),
        ] {
            let error = tools
                .execute(&call("start_command", arguments), &context)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
        for name in ["run_command", "start_command"] {
            let definition = tools
                .definitions()
                .into_iter()
                .find(|definition| definition.name == name)
                .unwrap();
            assert!(definition.description.contains("every argument goes in `args`"));
            assert!(!definition_before_command_revision(definition)
                .description
                .contains("`args`"));
        }
        manager.stop_all();
    }

    #[tokio::test]
    async fn legacy_catalog_rejects_pty_field_before_spawn() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_persistent_commands(manager);
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let result = tools
            .execute(
                &call(
                    "start_command",
                    json!({"command":"definitely-not-a-program","pty":true}),
                ),
                &context,
            )
            .await;
        assert!(
            matches!(result, Err(AgentError::Tool(message)) if message.contains("unknown field"))
        );
    }

    #[cfg(windows)]
    fn compile_probe() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tool_probe.rs");
        let binary = dir.path().join("tool_probe.exe");
        std::fs::write(
            &source,
            r#"
use std::io::{IsTerminal, Read};
use std::{env, thread, time::Duration};
fn main() {
    match env::args().nth(1).as_deref().unwrap() {
        "echo" => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            print!("OUT:{input}");
            eprint!("ERR:{input}");
        }
        "sleep" => thread::sleep(Duration::from_secs(60)),
        "terminal" => print!("PTY:{}", std::io::stdout().is_terminal()),
        _ => panic!("invalid mode"),
    }
}
"#,
        )
        .unwrap();
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let result = std::process::Command::new(compiler)
            .arg("--edition=2021")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        (dir, binary)
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn interactive_flow_through_tool_executor_and_termination() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_command_isolation(CommandIsolation::HostUnrestricted)
            .with_persistent_commands(manager);
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let start = tools
            .execute(
                &call("start_command", json!({"command":binary,"args":["echo"]})),
                &context,
            )
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&start.content).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(Uuid::parse_str(&id).is_ok());
        tools
            .execute(
                &call(
                    "write_stdin",
                    json!({"session_id":id,"input":"hello\n","close_after":true}),
                ),
                &context,
            )
            .await
            .unwrap();
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut finished = false;
        for _ in 0..100 {
            let output = tools
                .execute(
                    &call("poll_command", json!({"session_id":id,"wait_ms":100})),
                    &context,
                )
                .await
                .unwrap();
            let result: Value = serde_json::from_str(&output.content).unwrap();
            for event in result["events"].as_array().unwrap() {
                match event["stream"].as_str() {
                    Some("stdout") => stdout.push_str(event["text"].as_str().unwrap()),
                    Some("stderr") => stderr.push_str(event["text"].as_str().unwrap()),
                    _ => panic!("unexpected event: {event}"),
                }
            }
            if result["finished"] == true {
                assert_eq!(result["exit"]["code"], 0);
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert_eq!(stdout, "OUT:hello\n");
        assert_eq!(stderr, "ERR:hello\n");

        let sleeper = tools
            .execute(
                &call("start_command", json!({"command":binary,"args":["sleep"]})),
                &context,
            )
            .await
            .unwrap();
        let sleeper_id = serde_json::from_str::<Value>(&sleeper.content).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let terminated = tools
            .execute(
                &call("terminate_command", json!({"session_id":sleeper_id})),
                &context,
            )
            .await
            .unwrap();
        assert!(!terminated.is_error);
        let final_poll = tools
            .execute(
                &call("poll_command", json!({"session_id":sleeper_id,"wait_ms":0})),
                &context,
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&final_poll.content).unwrap();
        assert_eq!(result["exit"]["kind"], "terminated");
        assert_eq!(result["finished"], true);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pty_opt_in_flows_through_tool_adapter() {
        let (_probe_dir, binary) = compile_probe();
        let workspace = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.path().to_path_buf(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_pty_tools(),
        );
        let tools = LocalTools::new()
            .with_tool_permission(LocalTool::RunCommand, true)
            .with_command_isolation(CommandIsolation::HostUnrestricted)
            .with_persistent_commands(manager);
        let context = ToolContext {
            workspace: workspace.path().to_path_buf(),
            cancel: CancellationToken::new(),
        };
        let start = tools
            .execute(
                &call(
                    "start_command",
                    json!({"command":binary,"args":["terminal"],"pty":true,"columns":90,"rows":25}),
                ),
                &context,
            )
            .await
            .unwrap();
        let started: Value = serde_json::from_str(&start.content).unwrap();
        assert_eq!(started["pty"], true);
        let id = started["session_id"].as_str().unwrap();
        let mut terminal = String::new();
        let mut finished = false;
        for _ in 0..100 {
            let output = tools
                .execute(
                    &call("poll_command", json!({"session_id":id,"wait_ms":100})),
                    &context,
                )
                .await
                .unwrap();
            let result: Value = serde_json::from_str(&output.content).unwrap();
            for event in result["events"].as_array().unwrap() {
                if event["stream"] == "terminal" {
                    terminal.push_str(event["text"].as_str().unwrap());
                }
            }
            if result["finished"] == true {
                assert_eq!(result["exit"]["code"], 0);
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert!(terminal.contains("PTY:true"), "{terminal}");
    }
}
