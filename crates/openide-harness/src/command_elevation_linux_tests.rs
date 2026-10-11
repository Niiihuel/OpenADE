use super::*;
use crate::{LocalTool, LocalTools, ToolExecutor};

fn context(path: &Path) -> ToolContext {
    ToolContext {
        workspace: path.to_path_buf(),
        cancel: CancellationToken::new(),
    }
}

fn call(arguments: Value) -> ToolCall {
    ToolCall {
        id: "elevated-test".into(),
        name: "run_command".into(),
        arguments,
    }
}

fn shell() -> PathBuf {
    resolve_program("sh", Path::new("/")).unwrap()
}

fn script(path: &Path, body: &str) {
    let source = format!("#!{}\n{}\n", shell().display(), body);
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn fixture_host(path: &Path, body: &str) -> ElevationHost {
    let pkexec = path.join("fake-pkexec");
    script(&pkexec, body);
    ElevationHost {
        pkexec,
        helper: path.join("trusted-supervisor"),
    }
}

#[tokio::test]
async fn administrator_flag_requires_separate_host_opt_in_and_command_permission() {
    let dir = tempfile::tempdir().unwrap();
    let call = call(json!({"command":"sh","elevated":true}));
    let disabled = LocalTools::new().with_elevated_commands(true);
    assert!(
        disabled
            .execute(&call, &context(dir.path()))
            .await
            .unwrap()
            .is_error
    );
    let ordinary = LocalTools::new().with_tool_permission(LocalTool::RunCommand, true);
    let error = ordinary
        .execute(&call, &context(dir.path()))
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("administrator command access is disabled"));
    let malformed = call_with_value(json!({"command":"sh","elevated":"true"}));
    assert!(ordinary
        .execute(&malformed, &context(dir.path()))
        .await
        .unwrap_err()
        .to_string()
        .contains("elevated must be boolean"));
    assert!(ordinary
        .definitions()
        .iter()
        .find(|definition| definition.name == "run_command")
        .unwrap()
        .parameters["properties"]
        .get("elevated")
        .is_none());
    let enabled = ordinary.with_elevated_commands(true);
    assert_eq!(
        enabled
            .definitions()
            .iter()
            .find(|definition| definition.name == "run_command")
            .unwrap()
            .parameters["properties"]["elevated"]["type"],
        "boolean"
    );
}

fn call_with_value(arguments: Value) -> ToolCall {
    call(arguments)
}

#[tokio::test]
async fn administrator_flag_cannot_escape_a_workspace_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let tool = LocalTools::new()
        .with_tool_permission(LocalTool::RunCommand, true)
        .with_command_isolation(crate::CommandIsolation::WorkspaceWriteOffline)
        .with_elevated_commands(true);
    let error = tool
        .execute(
            &call(json!({"command":"sh","elevated":true})),
            &context(dir.path()),
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("cannot run inside the workspace sandbox"));
    assert!(tool
        .definitions()
        .iter()
        .find(|definition| definition.name == "run_command")
        .unwrap()
        .parameters["properties"]
        .get("elevated")
        .is_none());
}

#[tokio::test]
async fn elevation_uses_exact_argv_absolute_program_and_no_shell_interpolation() {
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("arguments");
    let body = format!("printf '%s\\n' \"$@\" > '{}'\nprintf '%s' '{{\"content\":\"fake authenticated output\",\"is_error\":false,\"uncertain\":false}}'", args_file.display());
    let host = fixture_host(dir.path(), &body);
    let invocation = invocation(&call(json!({"command":shell(),"args":["literal $(touch do-not-create)","space arg"],"timeout_ms":1000})), &context(dir.path())).unwrap();
    let program = invocation.program.clone();
    let output = run_with_host(invocation, &context(dir.path()), host)
        .await
        .unwrap();
    assert_eq!(output.content, "fake authenticated output");
    let arguments = std::fs::read_to_string(args_file).unwrap();
    let lines = arguments.lines().collect::<Vec<_>>();
    assert_eq!(
        &lines[..4],
        &[
            "--disable-internal-agent",
            "--user",
            "root",
            dir.path().join("trusted-supervisor").to_str().unwrap()
        ]
    );
    assert_eq!(lines[4], HELPER_FLAG);
    assert_eq!(lines[5], "1000");
    assert_eq!(lines[6], dir.path().to_str().unwrap());
    assert_eq!(lines[7], program.to_str().unwrap());
    assert_eq!(
        &lines[8..],
        &["literal $(touch do-not-create)", "space arg"]
    );
    assert!(!dir.path().join("do-not-create").exists());
}

#[tokio::test]
async fn authentication_rejection_has_no_user_command_fallback() {
    for code in [126, 127] {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("must-not-exist");
        let host = fixture_host(dir.path(), &format!("exit {code}"));
        let call =
            call(json!({"command":shell(),"args":["-c", format!("touch '{}'", marker.display())]}));
        let invocation = invocation(&call, &context(dir.path())).unwrap();
        let output = run_with_host(invocation, &context(dir.path()), host)
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("not executed"));
        assert!(!marker.exists());
    }
}

#[tokio::test]
async fn cancellation_closes_the_supervisor_lifetime_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let eof = dir.path().join("eof");
    let host = fixture_host(dir.path(), &format!("printf ready > '{}'\nread ignored\nprintf closed > '{}'\nprintf '%s' '{{\"content\":\"cancelled\",\"is_error\":true,\"uncertain\":false}}'", ready.display(), eof.display()));
    let context = context(dir.path());
    let cancellation = context.cancel.clone();
    let invocation = invocation(
        &call(json!({"command":shell(),"timeout_ms":1000})),
        &context,
    )
    .unwrap();
    let task = tokio::spawn(async move { run_with_host(invocation, &context, host).await });
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(ready.exists());
    cancellation.cancel();
    assert!(matches!(task.await.unwrap(), Err(AgentError::Cancelled)));
    assert!(eof.exists());
}

#[tokio::test]
async fn timeout_closes_supervisor_lease_and_marks_effect_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let eof = dir.path().join("eof");
    let host = fixture_host(
        dir.path(),
        &format!("read ignored\nprintf closed > '{}'", eof.display()),
    );
    let context = context(dir.path());
    let invocation = invocation(
        &call(json!({"command":shell(),"timeout_ms":1000})),
        &context,
    )
    .unwrap();
    let result = run_with_host(invocation, &context, host).await;
    assert!(matches!(result, Err(AgentError::EffectUncertain(_))));
    assert!(eof.exists());
}

#[tokio::test]
async fn missing_authentication_executable_is_an_explicit_error() {
    let dir = tempfile::tempdir().unwrap();
    let context = context(dir.path());
    let invocation = invocation(&call(json!({"command":shell()})), &context).unwrap();
    let host = ElevationHost {
        pkexec: dir.path().join("missing-pkexec"),
        helper: dir.path().join("helper"),
    };
    let result = run_with_host(invocation, &context, host).await;
    assert!(result.unwrap_err().to_string().contains("not executed"));
}

#[tokio::test]
async fn invalid_or_uncertain_supervisor_result_never_becomes_success() {
    for body in [
        "printf not-json",
        "printf '%s' '{\"content\":\"effects uncertain\",\"is_error\":true,\"uncertain\":true}'",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path());
        let invocation = invocation(&call(json!({"command":shell()})), &context).unwrap();
        let result = run_with_host(invocation, &context, fixture_host(dir.path(), body)).await;
        assert!(matches!(result, Err(AgentError::EffectUncertain(_))));
    }
}

#[test]
fn privileged_supervisor_rejects_unbounded_or_relative_argv() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "1".into(),
            dir.path().display().to_string(),
            shell().display().to_string(),
        ],
        vec![
            "120001".into(),
            dir.path().display().to_string(),
            shell().display().to_string(),
        ],
        vec![
            "1000".into(),
            "relative".into(),
            shell().display().to_string(),
        ],
        vec!["1000".into(), dir.path().display().to_string(), "sh".into()],
    ] {
        assert!(helper_invocation(&args).is_err());
    }
    let mut args = vec![
        "1000".into(),
        dir.path().display().to_string(),
        shell().display().to_string(),
    ];
    args.extend((0..MAX_ARGUMENTS + 1).map(|_| "arg".to_owned()));
    assert!(helper_invocation(&args).is_err());
}

#[test]
fn elevated_catalog_is_validated_before_permission_or_process_execution() {
    let enabled = LocalTools::new()
        .with_tool_permission(LocalTool::RunCommand, true)
        .with_elevated_commands(true);
    let catalog = crate::ToolCatalog::from_executor(&enabled).unwrap();
    let accepted = call(json!({"command":"git","args":["status"],"elevated":true}));
    assert!(catalog.require_valid_call(&accepted).is_ok());
    for arguments in [
        json!({"command":"git","elevated":"true"}),
        json!({"command":"git","args":[false],"elevated":true}),
        json!({"command":"git","elevated":true,"password":"never accepted"}),
        json!({"command":"git","elevated":true,"timeout_ms":120001}),
    ] {
        assert!(catalog.require_valid_call(&call(arguments)).is_err());
    }
    let ordinary = LocalTools::new().with_tool_permission(LocalTool::RunCommand, true);
    assert!(crate::ToolCatalog::from_executor(&ordinary)
        .unwrap()
        .require_valid_call(&accepted)
        .is_err());
    let persistent = call_with_value(json!({"command":"git","elevated":true}));
    let persistent = ToolCall {
        name: "start_command".into(),
        ..persistent
    };
    let definitions = crate::tools::persistent_command_tools::definitions_for_profile(
        crate::CommandIsolation::HostUnrestricted,
        false,
        false,
        true,
    );
    assert!(crate::ToolCatalog::new(definitions)
        .unwrap()
        .require_valid_call(&persistent)
        .is_err());
}

#[test]
fn elevated_calls_keep_exec_rules_for_the_actual_command() {
    let call = call(json!({"command":"git","args":["status"],"elevated":true}));
    for decision in [crate::ExecDecision::Ask, crate::ExecDecision::Deny] {
        let rule = crate::ExecRule::new(
            vec![crate::ExecPatternToken::Literal("git".into())],
            decision,
            None,
        )
        .unwrap();
        let policy = crate::PermissionPolicy::new()
            .with_exec_policy(crate::ExecPolicy::new(vec![rule]).unwrap());
        let expected = if decision == crate::ExecDecision::Ask {
            crate::PermissionMode::Ask
        } else {
            crate::PermissionMode::Deny
        };
        assert_eq!(policy.mode_for_call(&call), expected);
    }
    let host_deny =
        crate::PermissionPolicy::new().with_tool("run_command", crate::PermissionMode::Deny);
    assert_eq!(host_deny.mode_for_call(&call), crate::PermissionMode::Deny);
}

#[test]
fn lost_or_written_lifetime_lease_is_rejected_before_privileged_spawn() {
    use std::os::fd::AsRawFd;
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(lease_is_live(reader.as_raw_fd()).unwrap());
    writer.write_all(b"no command input is allowed").unwrap();
    assert!(!lease_is_live(reader.as_raw_fd()).unwrap());
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(writer);
    assert!(!lease_is_live(reader.as_raw_fd()).unwrap());
}

#[tokio::test]
async fn privileged_supervision_runs_harmless_command_with_closed_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let call = call(
        json!({"command":shell(),"args":["-c","if read input; then exit 1; else printf stdin-closed; fi"],"timeout_ms":1000}),
    );
    let output = supervise(&call, &context(dir.path())).await.unwrap();
    assert!(!output.is_error);
    assert!(output.content.contains("stdin-closed"));
}

#[tokio::test]
async fn supervisor_timeout_contains_the_actual_command_group_without_root() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("child-pid");
    let call = call(
        json!({"command":shell(),"args":["-c",format!("printf '%s' \"$$\" > '{}'; while :; do :; done",pid_file.display())],"timeout_ms":1000}),
    );
    let output = supervise(&call, &context(dir.path())).await;
    assert!(matches!(output, Err(AgentError::EffectUncertain(_))));
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    for _ in 0..100 {
        let status = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        if status.is_none()
            || status.as_deref().is_some_and(|value| {
                value
                    .rsplit_once(')')
                    .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
            })
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("command group survived supervisor timeout");
}
