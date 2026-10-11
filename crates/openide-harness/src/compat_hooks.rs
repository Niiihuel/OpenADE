//! Hooks that speak the Claude Code hook protocol, so tools written for it
//! (output proxies, rule injectors) run unchanged.
//!
//! They differ from native command hooks in one deliberate way: a hook that
//! fails, times out or exits with an unexpected status has no opinion and the
//! turn continues. Only exit status 2 or an explicit deny blocks.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use crate::hooks::{capture_hook_process, HookProcessOutput};
use crate::{
    CommandHookConfig, HookDecision, HookFailure, HookInvocation, HookPhase, HookResponse,
    HookRunResult, MAX_HOOK_OUTPUT_BYTES,
};

const MAX_TEXT_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    SubagentStart,
    Stop,
}

impl CompatEvent {
    pub const fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::SubagentStart => "SubagentStart",
            Self::Stop => "Stop",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::SessionStart,
            Self::UserPromptSubmit,
            Self::PreToolUse,
            Self::PostToolUse,
            Self::SubagentStart,
            Self::Stop,
        ]
        .into_iter()
        .find(|event| event.name() == name)
    }

    /// Raw standard output is context for these events; for the others only a
    /// JSON object means anything.
    const fn plain_output_is_context(self) -> bool {
        matches!(self, Self::SessionStart | Self::UserPromptSubmit)
    }
}

/// One hook entry. `matcher` is a regular expression that must match the whole
/// subject (a tool name, or the session source); empty or `*` matches all.
#[derive(Clone, Debug)]
pub struct CompatHookConfig {
    pub event: CompatEvent,
    pub matcher: String,
    /// Process settings. Its `tool_name` is a placeholder and its `phase` is
    /// `PostToolUse` only for that event.
    pub command: CommandHookConfig,
    /// Extra variables for this hook only, such as a plugin's own root
    /// directory. They are part of the hook's identity.
    pub env: Vec<(String, String)>,
}

impl CompatHookConfig {
    pub fn new(
        id: impl Into<String>,
        event: CompatEvent,
        matcher: impl Into<String>,
        executable: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
        timeout_ms: u64,
    ) -> Self {
        Self {
            event,
            matcher: matcher.into(),
            command: CommandHookConfig {
                id: id.into(),
                phase: if event == CompatEvent::PostToolUse {
                    HookPhase::PostToolUse
                } else {
                    HookPhase::PreToolUse
                },
                tool_name: "*".into(),
                executable,
                args,
                cwd,
                timeout_ms,
                max_output_bytes: MAX_HOOK_OUTPUT_BYTES,
            },
            env: Vec::new(),
        }
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    pub fn matches(&self, subject: &str) -> bool {
        if self.matcher.is_empty() || self.matcher == "*" {
            return true;
        }
        // An unparsable matcher matches nothing rather than everything.
        Regex::new(&format!("^(?:{})$", self.matcher)).is_ok_and(|regex| regex.is_match(subject))
    }
}

/// The name and input a Claude Code hook expects for one of our tool calls.
/// `run_command` becomes `Bash` with a single shell-quoted command line.
pub fn claude_tool(name: &str, arguments: &Value) -> (String, Value) {
    let claude_name = match name {
        "run_command" => "Bash",
        "read_file" => "Read",
        "write_file" => "Write",
        "edit_file" | "apply_patch" => "Edit",
        "search_files" => "Grep",
        "list_files" => "Glob",
        other => other,
    };
    let mut input = arguments.clone();
    if name == "run_command" {
        let split = crate::split_command_arguments(arguments);
        let arguments = split.as_ref().unwrap_or(arguments);
        let words = arguments["command"].as_str().into_iter().chain(
            arguments["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
        input = json!({ "command": shell_words::join(words) });
    } else if let (Some(object), Some(path)) = (input.as_object_mut(), arguments.get("path")) {
        object.insert("file_path".into(), path.clone());
    }
    (claude_name.to_owned(), input)
}

/// Turns a hook's rewritten `Bash` input back into `run_command` arguments,
/// keeping the original call's other fields. Rewrites of other tools are not
/// applied: their Claude Code input shapes do not map onto ours.
// shortcut: a rewrite that needs a shell (pipes, `&&`) is ignored, add a shell-backed command tool before honouring those.
pub fn native_input(name: &str, original: &Value, updated: &Value) -> Option<Value> {
    if name != "run_command" {
        return None;
    }
    let line = updated.get("command")?.as_str()?;
    if line.chars().any(|ch| "|&;<>()$`\n".contains(ch)) {
        return None;
    }
    let mut words = shell_words::split(line).ok()?.into_iter();
    let command = words.next()?;
    let mut arguments: Map<String, Value> = original.as_object().cloned().unwrap_or_default();
    arguments.insert("command".into(), command.into());
    arguments.insert("args".into(), words.map(Value::from).collect());
    Some(arguments.into())
}

fn tool_payload(config: &CompatHookConfig, invocation: &HookInvocation) -> Value {
    let (tool_name, tool_input) = claude_tool(&invocation.tool_name, &invocation.tool_input);
    let mut payload = json!({
        "hook_event_name": config.event.name(),
        "session_id": invocation.session_id,
        "cwd": config.command.cwd,
        "tool_name": tool_name,
        "tool_input": tool_input,
        "tool_use_id": invocation.tool_call_id,
    });
    if let Some(output) = &invocation.tool_output {
        payload["tool_response"] = output.clone();
    }
    payload
}

/// Runs a tool-event hook with the invocation the native protocol would get.
pub(crate) async fn run_compat_tool_hook(
    config: &CompatHookConfig,
    invocation: &HookInvocation,
    cancel: &CancellationToken,
) -> HookRunResult {
    run_compat_hook(config, tool_payload(config, invocation), cancel).await
}

fn bounded(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_owned())
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).and_then(bounded)
}

/// Runs one hook with an already built payload. The response's
/// `updated_input` is in the hook's own (Claude Code) shape.
pub async fn run_compat_hook(
    config: &CompatHookConfig,
    payload: Value,
    cancel: &CancellationToken,
) -> HookRunResult {
    let mut response = HookResponse {
        decision: HookDecision::Allow,
        reason: None,
        additional_context: None,
        updated_input: None,
    };
    let project_dir = config.command.cwd.as_os_str();
    let mut env: Vec<(&str, &std::ffi::OsStr)> = vec![("CLAUDE_PROJECT_DIR", project_dir)];
    env.extend(
        config
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), std::ffi::OsStr::new(value))),
    );
    let captured = capture_hook_process(
        &config.command,
        payload.to_string().into_bytes(),
        cancel,
        &env,
    )
    .await;
    let HookProcessOutput {
        status,
        stdout,
        stderr,
    } = match captured {
        Ok(output) => output,
        Err(
            HookRunResult::NotStarted(HookFailure::Cancelled)
            | HookRunResult::OutcomeUncertain(HookFailure::Cancelled)
            | HookRunResult::OutcomeUncertainCaptured {
                failure: HookFailure::Cancelled,
                ..
            },
        ) => return HookRunResult::NotStarted(HookFailure::Cancelled),
        Err(result) => {
            response.reason = Some(format!("hook did not complete: {result:?}"))
                .as_deref()
                .and_then(bounded);
            return HookRunResult::Completed {
                response,
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let post = config.event == CompatEvent::PostToolUse;
    match status.code() {
        Some(2) if post => response.additional_context = bounded(&stderr),
        Some(2) => {
            response.decision = HookDecision::Block;
            response.reason = bounded(&stderr).or_else(|| Some("blocked by hook".into()));
        }
        Some(0) => match serde_json::from_str::<Value>(&stdout) {
            Ok(output) if output.is_object() => {
                let specific = output.get("hookSpecificOutput").unwrap_or(&Value::Null);
                let denied = specific.get("permissionDecision").and_then(Value::as_str)
                    == Some("deny")
                    || output.get("decision").and_then(Value::as_str) == Some("block");
                let reason = text_field(specific, "permissionDecisionReason")
                    .or_else(|| text_field(&output, "reason"));
                if denied && post {
                    response.additional_context = reason;
                } else if denied {
                    response.decision = HookDecision::Block;
                    response.reason = reason.or_else(|| Some("blocked by hook".into()));
                } else {
                    response.additional_context = text_field(specific, "additionalContext");
                    response.updated_input = specific
                        .get("updatedInput")
                        .filter(|input| input.is_object() && !post)
                        .cloned();
                }
            }
            _ if config.event.plain_output_is_context() => {
                response.additional_context = bounded(&stdout);
            }
            _ => {}
        },
        // Any other status is a hook with nothing to say, not a failed turn.
        _ => {}
    }
    HookRunResult::Completed {
        response,
        stdout,
        stderr,
    }
}

/// What the hooks for a non-tool event said, in configuration order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventOutcome {
    /// Text to add to the model's context.
    pub context: Vec<String>,
    /// Set by the first hook that blocked: the prompt is refused, or for
    /// `Stop` the turn should not be treated as finished.
    pub blocked: Option<String>,
}

impl crate::CommandHooks {
    /// Runs every hook for a session, prompt, subagent or stop event. The
    /// host decides what a block means; nothing here changes a journal.
    /// `payload` carries the event's own fields (`prompt`, `source`,
    /// `agent_type`, `stop_hook_active`).
    pub async fn run_event(
        &self,
        event: CompatEvent,
        subject: &str,
        mut payload: Value,
        cancel: &CancellationToken,
    ) -> EventOutcome {
        let mut outcome = EventOutcome::default();
        if !payload.is_object() {
            payload = json!({});
        }
        payload["hook_event_name"] = event.name().into();
        for hook in self.compat_for_event(event, subject) {
            payload["cwd"] = json!(hook.command.cwd);
            let HookRunResult::Completed { response, .. } =
                run_compat_hook(hook, payload.clone(), cancel).await
            else {
                break;
            };
            outcome.context.extend(response.additional_context);
            if response.decision == HookDecision::Block {
                outcome.blocked = response.reason;
                break;
            }
        }
        outcome
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_gather_context_and_stop_at_the_first_block() {
        let dir = tempfile::tempdir().unwrap();
        let make = |id: &str, event, matcher: &str, script: &str| {
            CompatHookConfig::new(
                id,
                event,
                matcher,
                "/bin/sh".into(),
                vec!["-c".into(), script.into()],
                dir.path().into(),
                5_000,
            )
        };
        let hooks = crate::CommandHooks::new(vec![])
            .unwrap()
            .with_compat(vec![
                make("rules", CompatEvent::SessionStart, "startup|resume", "cat >/dev/null; printf 'RULES'"),
                make("map", CompatEvent::SessionStart, "", "cat >/dev/null; printf 'MAP'"),
                make(
                    "check",
                    CompatEvent::Stop,
                    "",
                    "case \"$(cat)\" in *'\"stop_hook_active\":true'*) exit 0;; esac; echo 'tests failed: 2' >&2; exit 2",
                ),
                make("after", CompatEvent::Stop, "", "cat >/dev/null; printf '{\"hookSpecificOutput\":{\"additionalContext\":\"never\"}}'"),
            ])
            .unwrap();
        let cancel = CancellationToken::new();

        let started = hooks.run_event(CompatEvent::SessionStart, "startup", json!({"source":"startup"}), &cancel).await;
        assert_eq!(started, EventOutcome { context: vec!["RULES".into(), "MAP".into()], blocked: None });
        let cleared = hooks.run_event(CompatEvent::SessionStart, "clear", Value::Null, &cancel).await;
        assert_eq!(cleared.context, vec!["MAP".to_owned()]);

        let stop = hooks.run_event(CompatEvent::Stop, "", json!({"stop_hook_active":false}), &cancel).await;
        assert_eq!(stop, EventOutcome { context: vec![], blocked: Some("tests failed: 2".into()) });
        // The retry guard a host passes on its follow-up lets the turn end.
        let retry = hooks.run_event(CompatEvent::Stop, "", json!({"stop_hook_active":true}), &cancel).await;
        assert_eq!(retry, EventOutcome { context: vec!["never".into()], blocked: None });

        let none = hooks.run_event(CompatEvent::UserPromptSubmit, "", json!({"prompt":"hi"}), &cancel).await;
        assert_eq!(none, EventOutcome::default());
    }

    fn hook(dir: &std::path::Path, event: CompatEvent, script: &str) -> CompatHookConfig {
        CompatHookConfig::new(
            "compat-1",
            event,
            "",
            "/bin/sh".into(),
            vec!["-c".into(), format!("cat >/dev/null; {script}")],
            dir.into(),
            5_000,
        )
    }

    async fn run(event: CompatEvent, script: &str) -> HookResponse {
        let dir = tempfile::tempdir().unwrap();
        let config = hook(dir.path(), event, script);
        match run_compat_hook(&config, json!({}), &CancellationToken::new()).await {
            HookRunResult::Completed { response, .. } => response,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn matcher_must_match_the_whole_subject() {
        let dir = std::env::temp_dir();
        let with = |matcher: &str| {
            CompatHookConfig::new("m", CompatEvent::PreToolUse, matcher, "/bin/sh".into(), vec![], dir.clone(), 1)
        };
        assert!(with("").matches("Bash") && with("*").matches("Read"));
        assert!(with("Bash").matches("Bash") && !with("Bash").matches("BashOutput"));
        assert!(with("Edit|Write").matches("Write") && !with("Edit|Write").matches("Read"));
        assert!(!with("(").matches("Bash"));
    }

    #[test]
    fn run_command_round_trips_through_a_bash_command_line() {
        let original = json!({"command":"git","args":["commit","-m","two words"],"timeout_ms":5000});
        let (name, input) = claude_tool("run_command", &original);
        assert_eq!(name, "Bash");
        assert_eq!(input, json!({"command":"git commit -m 'two words'"}));
        assert_eq!(native_input("run_command", &original, &input), Some(original.clone()));

        let wrapped = json!({"command":"rtk git commit -m 'two words'"});
        assert_eq!(
            native_input("run_command", &original, &wrapped),
            Some(json!({"command":"rtk","args":["git","commit","-m","two words"],"timeout_ms":5000}))
        );
        // Nothing that needs a shell, and nothing for other tools.
        for line in ["rtk a && rtk b", "cat x | wc", "echo $(id)", "", "'unterminated"] {
            assert_eq!(native_input("run_command", &original, &json!({"command": line})), None, "{line}");
        }
        assert_eq!(native_input("read_file", &json!({}), &json!({"command":"ls"})), None);
        assert_eq!(claude_tool("read_file", &json!({"path":"a.rs"})).1["file_path"], "a.rs");
    }

    #[tokio::test]
    async fn exit_status_two_blocks_and_any_other_failure_has_no_opinion() {
        let blocked = run(CompatEvent::PreToolUse, "echo 'not allowed' >&2; exit 2").await;
        assert_eq!(blocked.decision, HookDecision::Block);
        assert_eq!(blocked.reason.as_deref(), Some("not allowed"));

        for script in ["exit 1", "exit 3", "printf 'not json'", "printf '[1]'"] {
            let response = run(CompatEvent::PreToolUse, script).await;
            assert_eq!(response.decision, HookDecision::Allow, "{script}");
            assert!(response.updated_input.is_none() && response.additional_context.is_none());
        }
        // After the tool ran there is nothing left to block: the text is context.
        let after = run(CompatEvent::PostToolUse, "echo 'lint failed' >&2; exit 2").await;
        assert_eq!(after.decision, HookDecision::Allow);
        assert_eq!(after.additional_context.as_deref(), Some("lint failed"));
    }

    #[tokio::test]
    async fn json_output_can_deny_rewrite_or_add_context() {
        let denied = run(
            CompatEvent::PreToolUse,
            r#"printf '%s' '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"policy"}}'"#,
        )
        .await;
        assert_eq!((denied.decision, denied.reason.as_deref()), (HookDecision::Block, Some("policy")));

        let rewritten = run(
            CompatEvent::PreToolUse,
            r#"printf '%s' '{"hookSpecificOutput":{"updatedInput":{"command":"rtk ls"},"additionalContext":"note"}}'"#,
        )
        .await;
        assert_eq!(rewritten.updated_input, Some(json!({"command":"rtk ls"})));
        assert_eq!(rewritten.additional_context.as_deref(), Some("note"));

        let late = run(
            CompatEvent::PostToolUse,
            r#"printf '%s' '{"hookSpecificOutput":{"updatedInput":{"command":"x"}}}'"#,
        )
        .await;
        assert!(late.updated_input.is_none());
    }

    #[tokio::test]
    async fn a_hooks_own_variables_reach_its_process_and_change_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        let script = "cat >/dev/null; printf '%s' \"root=$CLAUDE_PLUGIN_ROOT\"";
        let plain = hook(dir.path(), CompatEvent::SessionStart, "");
        let mut with_root = plain.clone().with_env(vec![("CLAUDE_PLUGIN_ROOT".into(), "/plugins/demo".into())]);
        with_root.command.args = vec!["-c".into(), script.into()];
        let HookRunResult::Completed { response, .. } =
            run_compat_hook(&with_root, json!({}), &CancellationToken::new()).await
        else {
            panic!("hook did not complete");
        };
        assert_eq!(response.additional_context.as_deref(), Some("root=/plugins/demo"));

        let digest = |hook: CompatHookConfig| {
            crate::CommandHooks::new(vec![]).unwrap().with_compat(vec![hook]).unwrap().digest_hex().unwrap()
        };
        let other_root = plain.clone().with_env(vec![("CLAUDE_PLUGIN_ROOT".into(), "/plugins/other".into())]);
        assert_ne!(digest(plain.clone()), digest(other_root.clone()));
        assert_ne!(
            digest(other_root),
            digest(plain.with_env(vec![("CLAUDE_PLUGIN_ROOT".into(), "/plugins/demo".into())]))
        );
    }

    #[tokio::test]
    async fn plain_output_is_context_only_for_session_and_prompt_events() {
        let session = run(CompatEvent::SessionStart, "printf 'RULES ACTIVE'").await;
        assert_eq!(session.additional_context.as_deref(), Some("RULES ACTIVE"));
        let tool = run(CompatEvent::PreToolUse, "printf 'RULES ACTIVE'").await;
        assert!(tool.additional_context.is_none());
    }

    #[tokio::test]
    async fn payload_carries_the_claude_shape_and_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = hook(dir.path(), CompatEvent::PreToolUse, "");
        config.command.args = vec![
            "-c".into(),
            r#"input=$(cat); printf '{"hookSpecificOutput":{"additionalContext":"%s|%s"}}' "$CLAUDE_PROJECT_DIR" "$(printf '%s' "$input" | tr -d '"{}')" "#.into(),
        ];
        let invocation = HookInvocation {
            phase: HookPhase::PreToolUse,
            session_id: "s1".into(),
            turn_id: "t1".into(),
            tool_call_id: "c1".into(),
            tool_name: "run_command".into(),
            tool_input: json!({"command":"git","args":["status"]}),
            tool_output: None,
        };
        let HookRunResult::Completed { response, .. } =
            run_compat_tool_hook(&config, &invocation, &CancellationToken::new()).await
        else {
            panic!("hook did not complete");
        };
        let context = response.additional_context.unwrap();
        assert!(context.starts_with(&dir.path().display().to_string()), "{context}");
        for part in ["hook_event_name:PreToolUse", "tool_name:Bash", "command:git status", "session_id:s1"] {
            assert!(context.contains(part), "{part} missing in {context}");
        }
    }

    /// The contract this module exists for, checked against the real tool
    /// when it is installed. Skipped silently otherwise.
    #[tokio::test]
    async fn installed_rtk_hook_rewrites_a_command_unchanged() {
        let Some(rtk) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("rtk"))
                .find(|candidate| candidate.is_file())
        }) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let config = CompatHookConfig::new(
            "rtk",
            CompatEvent::PreToolUse,
            "Bash",
            rtk,
            vec!["hook".into(), "claude".into()],
            dir.path().into(),
            10_000,
        );
        let original = json!({"command":"git","args":["status"]});
        let (name, input) = claude_tool("run_command", &original);
        assert!(config.matches(&name));
        let payload = json!({"hook_event_name":"PreToolUse","tool_name":name,"tool_input":input});
        let HookRunResult::Completed { response, stdout, stderr } =
            run_compat_hook(&config, payload, &CancellationToken::new()).await
        else {
            panic!("rtk hook did not complete");
        };
        let updated = response.updated_input.unwrap_or_else(|| panic!("no rewrite: {stdout} {stderr}"));
        assert_eq!(
            native_input("run_command", &original, &updated),
            Some(json!({"command":"rtk","args":["git","status"]}))
        );
    }
}
