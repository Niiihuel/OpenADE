//! Fresh policy/registry acceptance at each real manual hook spawn boundary.
#![cfg(unix)]

use openide_core::{
    NativeCommandOperationAction as Action, NativeCommandOperationOutcome as Outcome,
    NativeCommandOperationState as State, SessionStore,
};
use openide_harness::{
    CommandHookConfig, CommandHooks, CommandIsolation, HookPhase, NativeManualCommandActor,
    NativeManualCommandPolicy, PermissionMode, PermissionPolicy, PersistentCommandLimits,
    PersistentCommandManager,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn hook(
    workspace: &Path,
    id: &str,
    phase: HookPhase,
    barrier: bool,
    block: bool,
) -> CommandHookConfig {
    let wait = if barrier {
        format!("while [ ! -f {id}.release ]; do sleep 0.01; done;")
    } else {
        String::new()
    };
    let decision = if block { "block" } else { "allow" };
    let message = if block {
        ",\"message\":\"manual hook block requested\""
    } else {
        ""
    };
    CommandHookConfig {
        id: id.into(), phase, tool_name: "write_stdin".into(), executable: "/bin/sh".into(),
        args: vec!["-c".into(), format!("cat > {id}.invocation.json; printf started > {id}.started; {wait} printf '%s' '{{\"decision\":\"{decision}\"{message}}}'")],
        cwd: workspace.into(), timeout_ms: 10_000, max_output_bytes: 4_096,
    }
}

fn policy(hooks: Vec<CommandHookConfig>) -> NativeManualCommandPolicy {
    NativeManualCommandPolicy {
        permissions: PermissionPolicy::new(),
        run_command_enabled: true,
        isolation: CommandIsolation::HostUnrestricted,
        hooks: Arc::new(CommandHooks::new(hooks).unwrap()),
    }
}

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing real hook/process observation {}", path.display()));
}

async fn wait_bytes(path: &Path, expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read(path).ok().as_deref() != Some(expected) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process did not persist exact bytes in {}", path.display()));
}

#[tokio::test]
async fn changed_permission_or_registry_between_pre_hooks_stops_remaining_spawns_and_stdin() {
    for changed_registry in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.clone(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_native_session_owner(&session.id)
            .unwrap(),
        );
        let command = manager
            .start(
                "/bin/sh",
                &["-c".into(), "exec cat > command.input".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        wait_file(&workspace.join("command.input")).await;
        let first = hook(&workspace, "pre-first", HookPhase::PreToolUse, true, false);
        let second = hook(
            &workspace,
            "pre-second",
            HookPhase::PreToolUse,
            false,
            false,
        );
        let source = Arc::new(Mutex::new(policy(vec![first.clone(), second])));
        let captured_source = source.clone();
        let actor = NativeManualCommandActor::new(
            sessions.clone(),
            session.id.clone(),
            Some(manager.clone()),
            Arc::new(move || Ok(captured_source.lock().unwrap().clone())),
        )
        .unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let request = actor
            .prepare_request(
                &command.id,
                Action::Stdin {
                    input: "never dispatched 🌱\n".into(),
                    close_after: true,
                },
            )
            .unwrap();
        actor.enqueue_confirmed(request.clone()).unwrap();
        let mutation = async {
            wait_file(&workspace.join("pre-first.started")).await;
            if changed_registry {
                source.lock().unwrap().hooks = Arc::new(
                    CommandHooks::new(vec![
                        first,
                        hook(
                            &workspace,
                            "replacement-pre",
                            HookPhase::PreToolUse,
                            false,
                            false,
                        ),
                    ])
                    .unwrap(),
                );
            } else {
                source.lock().unwrap().permissions =
                    PermissionPolicy::new().with_tool("write_stdin", PermissionMode::Deny);
            }
            std::fs::write(
                workspace.join("pre-first.release"),
                b"release already started original hook",
            )
            .unwrap();
        };
        let cancel = CancellationToken::new();
        let (result, ()) = tokio::join!(actor.process_pending(&lease, &cancel), mutation);
        result.unwrap();
        let receipt = actor
            .operations()
            .snapshot(&session.id, &request.operation_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            receipt.state,
            State::Finished {
                outcome: Outcome::Failed { .. }
            }
        ));
        assert!(receipt.effect_result.is_none());
        assert!(!workspace.join("pre-second.started").exists());
        assert!(!workspace.join("replacement-pre.started").exists());
        assert_eq!(std::fs::read(workspace.join("command.input")).unwrap(), b"");
        assert!(manager.control_snapshot(&command.id).unwrap().stdin_open());
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
        manager.terminate(&command.id).await.unwrap();
    }
}

#[tokio::test]
async fn changed_permission_or_registry_between_post_hooks_preserves_known_eof_effect() {
    for changed_registry in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.clone(),
                CommandIsolation::HostUnrestricted,
                PersistentCommandLimits::default(),
            )
            .unwrap()
            .with_native_session_owner(&session.id)
            .unwrap(),
        );
        let command = manager
            .start(
                "/bin/sh",
                &["-c".into(), "exec cat > command.input".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        wait_file(&workspace.join("command.input")).await;
        let first = hook(
            &workspace,
            "post-first",
            HookPhase::PostToolUse,
            true,
            false,
        );
        let second = hook(
            &workspace,
            "post-second",
            HookPhase::PostToolUse,
            false,
            false,
        );
        let source = Arc::new(Mutex::new(policy(vec![first.clone(), second])));
        let captured_source = source.clone();
        let actor = NativeManualCommandActor::new(
            sessions.clone(),
            session.id.clone(),
            Some(manager.clone()),
            Arc::new(move || Ok(captured_source.lock().unwrap().clone())),
        )
        .unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let request = actor
            .prepare_request(
                &command.id,
                Action::Stdin {
                    input: "known exact EOF effect 🌱\n".into(),
                    close_after: true,
                },
            )
            .unwrap();
        actor.enqueue_confirmed(request.clone()).unwrap();
        let mutation = async {
            wait_file(&workspace.join("post-first.started")).await;
            if changed_registry {
                source.lock().unwrap().hooks = Arc::new(
                    CommandHooks::new(vec![
                        first,
                        hook(
                            &workspace,
                            "replacement-post",
                            HookPhase::PostToolUse,
                            false,
                            false,
                        ),
                    ])
                    .unwrap(),
                );
            } else {
                source.lock().unwrap().permissions =
                    PermissionPolicy::new().with_tool("write_stdin", PermissionMode::Deny);
            }
            std::fs::write(
                workspace.join("post-first.release"),
                b"release original hook after confirmed command effect",
            )
            .unwrap();
        };
        let cancel = CancellationToken::new();
        let (result, ()) = tokio::join!(actor.process_pending(&lease, &cancel), mutation);
        result.unwrap();
        let receipt = actor
            .operations()
            .snapshot(&session.id, &request.operation_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            receipt.state,
            State::Finished {
                outcome: Outcome::Failed { .. }
            }
        ));
        assert!(matches!(
            receipt.effect_result,
            Some(Outcome::Succeeded { .. })
        ));
        assert!(!workspace.join("post-second.started").exists());
        assert!(!workspace.join("replacement-post.started").exists());
        assert_eq!(
            std::fs::read(workspace.join("command.input")).unwrap(),
            "known exact EOF effect 🌱\n".as_bytes()
        );
        let invocation: serde_json::Value = serde_json::from_slice(
            &std::fs::read(workspace.join("post-first.invocation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(invocation["source"], "manual_command");
        assert_eq!(invocation["tool_output"]["type"], "succeeded");
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
        drop(lease);
        let reopened = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
        assert_eq!(
            openide_core::NativeCommandOperationStore::new(reopened)
                .snapshot(&session.id, &request.operation_id)
                .unwrap(),
            Some(receipt)
        );
    }
}

#[tokio::test]
async fn invalid_post_hook_block_preserves_phase_diagnostics_and_known_input_receipt() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    let manager = Arc::new(
        PersistentCommandManager::new(
            workspace.clone(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_native_session_owner(&session.id)
        .unwrap(),
    );
    let command = manager
        .start(
            "/bin/sh",
            &["-c".into(), "exec cat > command.input".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let configuration = policy(vec![hook(
        &workspace,
        "post-block",
        HookPhase::PostToolUse,
        false,
        true,
    )]);
    let actor = NativeManualCommandActor::new(
        sessions.clone(),
        session.id.clone(),
        Some(manager.clone()),
        Arc::new(move || Ok(configuration.clone())),
    )
    .unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let request = actor
        .prepare_request(
            &command.id,
            Action::Stdin {
                input: "acknowledged EOF\n".into(),
                close_after: true,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(request.clone()).unwrap();
    assert!(matches!(
        actor
            .process_pending(&lease, &CancellationToken::new())
            .await,
        Err(openide_harness::AgentError::EffectUncertain(_))
    ));
    let receipt = actor
        .operations()
        .snapshot(&session.id, &request.operation_id)
        .unwrap()
        .unwrap();
    let State::Finished {
        outcome: Outcome::EffectUncertain { diagnostic },
    } = &receipt.state
    else {
        panic!("invalid post-hook decision did not preserve an uncertain observation");
    };
    assert!(diagnostic.contains("requires verification"));
    let phase = receipt
        .phase_diagnostics()
        .into_iter()
        .filter(|entry| entry.phase == openide_core::NativeCommandOperationPhase::BeforePostHooks)
        .find_map(|entry| {
            let value: serde_json::Value = serde_json::from_str(&entry.diagnostic).ok()?;
            value.get("stdout").map(|_| value.clone())
        })
        .unwrap();
    assert_eq!(phase["hook_id"], "post-block");
    assert_eq!(
        phase["stdout"],
        "{\"decision\":\"block\",\"message\":\"manual hook block requested\"}"
    );
    assert!(!diagnostic.contains("not executed"));
    assert!(matches!(
        receipt.effect_result,
        Some(Outcome::Succeeded { .. })
    ));
    wait_bytes(&workspace.join("command.input"), b"acknowledged EOF\n").await;
    assert_eq!(
        std::fs::read(workspace.join("command.input")).unwrap(),
        b"acknowledged EOF\n"
    );
    assert!(workspace.join("post-block.started").exists());
}
