//! Real processes/hooks and durable human-command recovery. These controls
//! never manufacture model/tool journal events.
#![cfg(unix)]

use openide_core::{
    NativeCommandOperationAction as Action, NativeCommandOperationOutcome as Outcome,
    NativeCommandOperationPhase as Phase, NativeCommandOperationSnapshot,
    NativeCommandOperationState as State, NativeCommandOperationStore, Session, SessionStore,
};
use openide_harness::{
    AgentError, CommandHookConfig, CommandHooks, CommandIsolation, HookPhase,
    NativeManualCommandActor, NativeManualCommandObserver, NativeManualCommandPolicy,
    PermissionMode, PermissionPolicy, PersistentCommandEvent, PersistentCommandLimits,
    PersistentCommandManager,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const EXACT_INPUT: &str = "entrada 🦀\r\nliteral\0final";

fn policy(hooks: Vec<CommandHookConfig>) -> NativeManualCommandPolicy {
    NativeManualCommandPolicy {
        permissions: PermissionPolicy::new().with_default_mode(PermissionMode::Ask),
        run_command_enabled: true,
        isolation: CommandIsolation::HostUnrestricted,
        hooks: Arc::new(CommandHooks::new(hooks).unwrap()),
    }
}

fn hook(workspace: &Path, id: &str, phase: HookPhase, script: &str) -> CommandHookConfig {
    CommandHookConfig {
        id: id.into(),
        phase,
        tool_name: "write_stdin".into(),
        executable: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: workspace.into(),
        timeout_ms: 30_000,
        max_output_bytes: 64 * 1024,
    }
}

fn manager(workspace: &Path, session: &Session) -> Arc<PersistentCommandManager> {
    Arc::new(
        PersistentCommandManager::new(
            workspace.into(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap()
        .with_native_session_owner(&session.id)
        .unwrap(),
    )
}

fn actor(
    sessions: Arc<SessionStore>,
    session: &Session,
    manager: Arc<PersistentCommandManager>,
    policy: Arc<Mutex<NativeManualCommandPolicy>>,
) -> NativeManualCommandActor {
    NativeManualCommandActor::new(
        sessions,
        session.id.clone(),
        Some(manager),
        Arc::new(move || Ok(policy.lock().unwrap().clone())),
    )
    .unwrap()
}

fn fixture() -> (
    tempfile::TempDir,
    Arc<SessionStore>,
    Session,
    Arc<PersistentCommandManager>,
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    let manager = manager(&workspace, &session);
    (root, sessions, session, manager)
}

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process did not publish {}", path.display()));
}

async fn wait_bytes(path: &Path, expected: &[u8]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fs::read(path).ok().as_deref() == Some(expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process did not save exact bytes in {}", path.display()));
}

async fn file_command(manager: &PersistentCommandManager, workspace: &Path) -> String {
    let started = manager
        .start(
            "/bin/sh",
            &[
                "-c".into(),
                "printf ready > command.started; exec cat > command.input".into(),
            ],
            &CancellationToken::new(),
        )
        .unwrap();
    wait_file(&workspace.join("command.started")).await;
    started.id
}

fn output_bytes(manager: &PersistentCommandManager, id: &str) -> Vec<u8> {
    manager
        .inspect_pending_output(id)
        .unwrap()
        .events
        .iter()
        .flat_map(|event| match event {
            PersistentCommandEvent::Output { bytes, .. } => bytes.clone(),
            PersistentCommandEvent::Omitted { .. } => panic!("unexpected probe output omission"),
        })
        .collect()
}

#[tokio::test]
async fn confirmed_stdin_eof_and_terminate_touch_real_processes_without_fake_journal_events() {
    let (_root, sessions, session, manager) = fixture();
    let actor = actor(
        sessions.clone(),
        &session,
        manager.clone(),
        Arc::new(Mutex::new(policy(vec![]))),
    );
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let command = manager
        .start(
            "/bin/sh",
            &["-c".into(), "exec cat".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let input = actor
        .prepare_request(
            &command.id,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(input.clone()).unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while output_bytes(&manager, &command.id) != EXACT_INPUT.as_bytes() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(manager.control_snapshot(&command.id).unwrap().stdin_open());
    let eof = actor
        .prepare_request(
            &command.id,
            Action::Stdin {
                input: String::new(),
                close_after: true,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(eof.clone()).unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while manager
            .control_snapshot(&command.id)
            .unwrap()
            .exit
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(output_bytes(&manager, &command.id), EXACT_INPUT.as_bytes());
    let saved_eof = actor
        .operations()
        .snapshot(&session.id, &eof.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(actor.enqueue_confirmed(eof.clone()).unwrap(), saved_eof);
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output_bytes(&manager, &command.id), EXACT_INPUT.as_bytes());
    let mut conflicting_eof = eof.clone();
    conflicting_eof.action = Action::Stdin {
        input: "must not resend after EOF".into(),
        close_after: true,
    };
    assert!(actor.enqueue_confirmed(conflicting_eof).is_err());
    let second = manager
        .start(
            "/bin/sh",
            &["-c".into(), "exec cat".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let termination = actor
        .prepare_request(&second.id, Action::Terminate)
        .unwrap();
    actor.enqueue_confirmed(termination.clone()).unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    let saved_termination = actor
        .operations()
        .snapshot(&session.id, &termination.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        actor.enqueue_confirmed(termination.clone()).unwrap(),
        saved_termination
    );
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    let snapshots = actor.operations().list(&session.id).unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots.iter().all(|op| matches!(
        op.state,
        State::Finished {
            outcome: Outcome::Succeeded { .. }
        }
    ) && matches!(
        op.effect_result,
        Some(Outcome::Succeeded { .. })
    )));
    assert!(manager.control_snapshot(&second.id).unwrap().stop_requested);
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
}

#[tokio::test]
async fn fresh_deny_changed_hook_bytes_cancellation_and_capacity_rejection_precede_all_effects() {
    let (_root, sessions, session, manager) = fixture();
    let workspace = &session.workspace;
    let script = workspace.join("guard.sh");
    fs::write(
        &script,
        "printf hook > hook.effect; cat > invocation.json; printf '%s' '{\"decision\":\"allow\"}'",
    )
    .unwrap();
    let mut config = hook(workspace, "file-guard", HookPhase::PreToolUse, "");
    config.args = vec![script.to_string_lossy().into_owned()];
    let source = Arc::new(Mutex::new(policy(vec![config.clone()])));
    let actor = actor(sessions.clone(), &session, manager.clone(), source.clone());
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let command = file_command(&manager, workspace).await;
    let denial = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(denial.clone()).unwrap();
    source.lock().unwrap().permissions =
        PermissionPolicy::new().with_tool("write_stdin", PermissionMode::Deny);
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        actor
            .operations()
            .snapshot(&session.id, &denial.operation_id)
            .unwrap()
            .unwrap()
            .state,
        State::Rejected { .. }
    ));
    assert!(actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: "denied".into(),
                close_after: false
            }
        )
        .is_err());
    source.lock().unwrap().permissions = PermissionPolicy::new();
    let changed = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(changed.clone()).unwrap();
    fs::write(
        &script,
        "printf modified > hook.effect; printf '%s' '{\"decision\":\"allow\"}'",
    )
    .unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        actor
            .operations()
            .snapshot(&session.id, &changed.operation_id)
            .unwrap()
            .unwrap()
            .state,
        State::Rejected { .. }
    ));
    let cancelled = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(cancelled.clone()).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    actor.process_pending(&lease, &cancel).await.unwrap();
    assert!(matches!(
        actor
            .operations()
            .snapshot(&session.id, &cancelled.operation_id)
            .unwrap()
            .unwrap()
            .state,
        State::Cancelled { .. }
    ));
    let mut many = Vec::new();
    for index in 0..6 {
        let mut next = config.clone();
        next.id = format!("large-{index}");
        many.push(next);
    }
    source.lock().unwrap().hooks = Arc::new(CommandHooks::new(many).unwrap());
    let capacity = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(capacity.clone()).unwrap();
    assert!(matches!(
        actor
            .process_pending(&lease, &CancellationToken::new())
            .await,
        Err(AgentError::Limit(_))
    ));
    assert!(matches!(
        actor
            .operations()
            .snapshot(&session.id, &capacity.operation_id)
            .unwrap()
            .unwrap()
            .state,
        State::Rejected { .. }
    ));
    assert!(!workspace.join("hook.effect").exists());
    assert!(!workspace.join("invocation.json").exists());
    assert_eq!(fs::read(workspace.join("command.input")).unwrap(), b"");
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    manager.terminate(&command).await.unwrap();
}

#[tokio::test]
async fn queued_withdrawal_wins_while_active_actor_is_revalidating_policy_before_start() {
    let (_root, sessions, session, manager) = fixture();
    let workspace = &session.workspace;
    let trusted = policy(vec![hook(
        workspace,
        "guard",
        HookPhase::PreToolUse,
        "cat > hook.invocation; printf hook > hook.effect; printf '%s' '{\"decision\":\"allow\"}'",
    )]);
    let source_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = source_calls.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let actor = Arc::new(
        NativeManualCommandActor::new(
            sessions.clone(),
            session.id.clone(),
            Some(manager.clone()),
            Arc::new(move || {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
                    entered_tx
                        .send(())
                        .map_err(|_| "test barrier receiver disappeared".to_owned())?;
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))
                        .map_err(|_| "test source barrier timed out".to_owned())?;
                }
                Ok(trusted.clone())
            }),
        )
        .unwrap(),
    );
    let command = file_command(&manager, workspace).await;
    let request = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(request.clone()).unwrap();
    assert_eq!(source_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let worker_actor = actor.clone();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(worker_actor.process_pending(&lease, &CancellationToken::new()))
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("actor did not reach fresh policy barrier");
    assert!(matches!(
        sessions.try_acquire_native_lease(&session.id),
        Err(openide_core::StoreError::NativeSessionBusy(_))
    ));
    let cancelled = actor
        .operations()
        .cancel_queued_requested(
            &session.id,
            &request.operation_id,
            0,
            "Human withdrew exact input while active worker revalidated policy",
        )
        .unwrap();
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(matches!(cancelled.state, State::Cancelled { .. }));
    assert!(cancelled.effect_result.is_none());
    assert_eq!(
        actor
            .operations()
            .snapshot(&session.id, &request.operation_id)
            .unwrap(),
        Some(cancelled)
    );
    assert!(!workspace.join("hook.effect").exists());
    assert!(!workspace.join("hook.invocation").exists());
    assert_eq!(fs::read(workspace.join("command.input")).unwrap(), b"");
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    manager.terminate(&command).await.unwrap();
}

#[tokio::test]
async fn known_stdin_result_and_full_hook_output_survive_uncertain_post_hook_and_reopen() {
    let (root, sessions, session, manager) = fixture();
    let workspace = &session.workspace;
    let pre = hook(workspace, "pre", HookPhase::PreToolUse,
        "cat > pre.invocation.json; printf pre > pre.effect; printf '%s' '{\"decision\":\"allow\"}'; printf '%032768d' 7 >&2");
    let post = hook(workspace, "post", HookPhase::PostToolUse,
        "cat > post.invocation.json; printf post > post.effect; printf invalid-json; printf full-post-error >&2; exit 7");
    let actor = actor(
        sessions.clone(),
        &session,
        manager.clone(),
        Arc::new(Mutex::new(policy(vec![pre, post]))),
    );
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let command = file_command(&manager, workspace).await;
    let request = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(request.clone()).unwrap();
    assert!(matches!(
        actor
            .process_pending(&lease, &CancellationToken::new())
            .await,
        Err(AgentError::EffectUncertain(_))
    ));
    wait_bytes(&workspace.join("command.input"), EXACT_INPUT.as_bytes()).await;
    let saved = actor
        .operations()
        .snapshot(&session.id, &request.operation_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.effect_result,
        Some(Outcome::Succeeded { .. })
    ));
    assert!(matches!(
        saved.state,
        State::Finished {
            outcome: Outcome::EffectUncertain { .. }
        }
    ));
    let pre_observation = saved
        .phase_diagnostics()
        .into_iter()
        .filter_map(|entry| serde_json::from_str::<serde_json::Value>(&entry.diagnostic).ok())
        .find(|value| value["hook_id"] == "pre" && value.get("stderr").is_some())
        .unwrap();
    assert_eq!(pre_observation["stderr"].as_str().unwrap().len(), 32_768);
    assert!(pre_observation["stderr"].as_str().unwrap().ends_with('7'));
    let post_observation = saved
        .phase_diagnostics()
        .into_iter()
        .filter_map(|entry| serde_json::from_str::<serde_json::Value>(&entry.diagnostic).ok())
        .find(|value| value["hook_id"] == "post" && value.get("stderr").is_some())
        .unwrap();
    assert_eq!(post_observation["stdout"], "invalid-json");
    assert_eq!(post_observation["stderr"], "full-post-error");
    let invocation: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join("pre.invocation.json")).unwrap()).unwrap();
    assert_eq!(invocation["source"], "manual_command");
    assert_eq!(invocation["tool_input"]["input"], EXACT_INPUT);
    assert!(invocation.get("turn_id").is_none());
    manager.terminate(&command).await.unwrap();
    drop(lease);
    let reopened_sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
    let reopened = NativeCommandOperationStore::new(reopened_sessions.clone());
    assert_eq!(
        reopened
            .snapshot(&session.id, &request.operation_id)
            .unwrap(),
        Some(saved)
    );
    assert!(reopened_sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
}

struct ProcessBarrier {
    workspace: PathBuf,
    case: String,
}

struct LostHostAfterKnownEffect;
impl NativeManualCommandObserver for LostHostAfterKnownEffect {
    fn on_persisted_operation(
        &self,
        snapshot: &NativeCommandOperationSnapshot,
    ) -> Result<(), AgentError> {
        if snapshot.effect_result.is_some() {
            return Err(AgentError::Runtime(
                "host delivery failed after synced effect receipt, before post-hook".into(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn host_observer_failure_after_known_effect_stops_before_post_hook_and_never_repeats_stdin() {
    let (root, sessions, session, manager) = fixture();
    let workspace = &session.workspace;
    let source = Arc::new(Mutex::new(policy(vec![
        hook(workspace, "pre", HookPhase::PreToolUse, "cat > pre.invocation.json; printf pre >> pre.count; printf '%s' '{\"decision\":\"allow\"}'"),
        hook(workspace, "post", HookPhase::PostToolUse, "cat > post.invocation.json; printf post >> post.count; printf '%s' '{\"decision\":\"allow\"}'"),
    ])));
    let actor = actor(sessions.clone(), &session, manager.clone(), source)
        .with_operation_observer(Arc::new(LostHostAfterKnownEffect));
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let command = file_command(&manager, workspace).await;
    let request = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    actor.enqueue_confirmed(request.clone()).unwrap();
    assert!(matches!(
        actor
            .process_pending(&lease, &CancellationToken::new())
            .await,
        Err(AgentError::Runtime(_))
    ));
    wait_bytes(&workspace.join("command.input"), EXACT_INPUT.as_bytes()).await;
    let saved = actor
        .operations()
        .snapshot(&session.id, &request.operation_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.state,
        State::Started {
            phase: Phase::BeforePostHooks,
            ..
        }
    ));
    assert!(matches!(
        saved.effect_result,
        Some(Outcome::Succeeded { .. })
    ));
    let path = root
        .path()
        .join("state")
        .join(format!("{}.native-command-operations.json", session.id));
    let bytes = fs::read(&path).unwrap();
    assert!(matches!(
        actor
            .process_pending(&lease, &CancellationToken::new())
            .await,
        Err(AgentError::EffectUncertain(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    actor
        .operations()
        .resolve_started(
            &lease,
            &request.operation_id,
            saved.revision,
            "Verified exact command file and one pre-hook; post-hook never spawned",
        )
        .unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fs::read(workspace.join("pre.count")).unwrap(), b"pre");
    assert!(!workspace.join("post.count").exists());
    assert_eq!(
        fs::read(workspace.join("command.input")).unwrap(),
        EXACT_INPUT.as_bytes()
    );
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    manager.terminate(&command).await.unwrap();
}
impl NativeManualCommandObserver for ProcessBarrier {
    fn on_persisted_operation(
        &self,
        snapshot: &NativeCommandOperationSnapshot,
    ) -> Result<(), AgentError> {
        let matches = match (&self.case[..], &snapshot.state) {
            (
                "before-pre",
                State::Started {
                    phase: Phase::BeforePreHooks,
                    ..
                },
            ) => snapshot.revision == 1,
            (
                "before-command",
                State::Started {
                    phase: Phase::BeforeCommand,
                    ..
                },
            ) => true,
            (
                "known-effect",
                State::Started {
                    phase: Phase::BeforePostHooks,
                    ..
                },
            ) => snapshot.effect_result.is_some(),
            _ => false,
        };
        if matches {
            fs::write(
                self.workspace.join("cut.barrier"),
                serde_json::to_vec(snapshot).unwrap(),
            )
            .unwrap();
            loop {
                std::thread::park_timeout(Duration::from_secs(60));
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn killed_real_actors_reopen_each_effect_boundary_without_restoring_or_repeating_processes() {
    for case in [
        "requested",
        "before-pre",
        "before-command",
        "known-effect",
        "hook-unknown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "manual_command_process_cut_child",
                "--nocapture",
            ])
            .env("OPENIDE_MANUAL_CUT_ROOT", root.path())
            .env("OPENIDE_MANUAL_CUT_CASE", case)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let marker = workspace.join(if case == "hook-unknown" {
            "hook.started"
        } else {
            "cut.barrier"
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "child {case} ended {status}: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(
                Instant::now() < deadline,
                "{case} did not reach durable/process cut boundary"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if case == "known-effect" {
            wait_bytes(&workspace.join("command.input"), EXACT_INPUT.as_bytes()).await;
        }
        child.kill().unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        // Let a hook already launched by the killed owner finish before human
        // inspection. This does not use the reopened actor or restore it.
        if case == "hook-unknown" {
            fs::write(workspace.join("hook.release"), b"release old owned hook").unwrap();
            wait_file(&workspace.join("hook.done")).await;
        }
        let sessions = Arc::new(SessionStore::open(root.path().join("state")).unwrap());
        let session = sessions.list_with_diagnostics().unwrap().sessions[0]
            .session
            .clone();
        let store = NativeCommandOperationStore::new(sessions.clone());
        let original = store.list(&session.id).unwrap().remove(0);
        let new_manager = manager(&workspace, &session);
        assert_ne!(
            new_manager.generation(),
            original.request.manager_generation
        );
        assert!(new_manager
            .control_snapshot(&original.request.command_id)
            .is_err());
        let source = Arc::new(Mutex::new(policy(vec![])));
        let new_actor = actor(sessions.clone(), &session, new_manager, source);
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        if case == "requested" {
            new_actor
                .process_pending(&lease, &CancellationToken::new())
                .await
                .unwrap();
            assert!(matches!(
                store
                    .snapshot(&session.id, &original.request.operation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                State::Rejected { .. }
            ));
        } else {
            assert!(original.state.requires_verification());
            assert!(matches!(
                new_actor
                    .process_pending(&lease, &CancellationToken::new())
                    .await,
                Err(AgentError::EffectUncertain(_))
            ));
            assert_eq!(
                store
                    .snapshot(&session.id, &original.request.operation_id)
                    .unwrap(),
                Some(original.clone())
            );
            let resolved = store
                .resolve_started(
                    &lease,
                    &original.request.operation_id,
                    original.revision,
                    "Human verified old command bytes and old hook completion; no process restored",
                )
                .unwrap();
            assert!(matches!(resolved.state, State::Resolved { .. }));
            assert_eq!(resolved.effect_result, original.effect_result);
            new_actor
                .process_pending(&lease, &CancellationToken::new())
                .await
                .unwrap();
        }
        let expected = if case == "known-effect" {
            EXACT_INPUT.as_bytes()
        } else {
            b""
        };
        wait_bytes(&workspace.join("command.input"), expected).await;
        assert_eq!(
            fs::read(workspace.join("pre.count"))
                .ok()
                .unwrap_or_default(),
            if matches!(case, "before-command" | "known-effect" | "hook-unknown") {
                b"pre".to_vec()
            } else {
                vec![]
            }
        );
        assert!(!workspace.join("post.count").exists());
        assert!(sessions
            .load(&session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    }
}

#[tokio::test]
#[ignore = "subprocess helper killed at real durable/effect boundaries"]
async fn manual_command_process_cut_child() {
    let Some(root) = std::env::var_os("OPENIDE_MANUAL_CUT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.join("state")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    let manager = manager(&workspace, &session);
    let case = std::env::var("OPENIDE_MANUAL_CUT_CASE").unwrap();
    let pre_script = if case == "hook-unknown" {
        "cat > pre.invocation.json; printf pre >> pre.count; printf started > hook.started; while [ ! -f hook.release ]; do sleep 0.02; done; printf done > hook.done; printf '%s' '{\"decision\":\"allow\"}'"
    } else {
        "cat > pre.invocation.json; printf pre >> pre.count; printf '%s' '{\"decision\":\"allow\"}'"
    };
    let source = Arc::new(Mutex::new(policy(vec![
        hook(&workspace, "pre", HookPhase::PreToolUse, pre_script),
        hook(&workspace, "post", HookPhase::PostToolUse, "cat > post.invocation.json; printf post >> post.count; printf '%s' '{\"decision\":\"allow\"}'"),
    ])));
    let actor = actor(sessions.clone(), &session, manager.clone(), source).with_operation_observer(
        Arc::new(ProcessBarrier {
            workspace: workspace.clone(),
            case: case.clone(),
        }),
    );
    let command = file_command(&manager, &workspace).await;
    let request = actor
        .prepare_request(
            &command,
            Action::Stdin {
                input: EXACT_INPUT.into(),
                close_after: false,
            },
        )
        .unwrap();
    let snapshot = actor.enqueue_confirmed(request).unwrap();
    if case == "requested" {
        fs::write(
            workspace.join("cut.barrier"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        loop {
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    actor
        .process_pending(&lease, &CancellationToken::new())
        .await
        .unwrap();
    panic!("cut not reached");
}
