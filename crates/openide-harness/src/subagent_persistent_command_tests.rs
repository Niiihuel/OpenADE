#![cfg(unix)]
use super::*;
use crate::{
    CommandHookConfig, HookPhase, LocalTool, LocalTools, ModelOutput, NativeCommandProfileOptions,
    NativeCommandRegistry, NativeManualCommandPolicy, PermissionMode, ToolContext, ToolDefinition,
};
use async_trait::async_trait;
use openide_core::{
    MessageRole, NativeCommandOperationAction, NativeCommandOperationOutcome,
    NativeCommandOperationState, NativeCommandOutputStore, NativeCommandOutputStream,
};
use serde_json::{json, Value};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn base() -> Arc<dyn ToolExecutor> {
    Arc::new(LocalTools::new().with_tool_permission(LocalTool::RunCommand, true))
}

fn tool(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

fn result<'a>(messages: &'a [AgentMessage], id: &str) -> Option<&'a str> {
    messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some(id))
        .map(|message| message.content.as_str())
}

fn response(call: ToolCall) -> ModelOutput {
    ModelOutput {
        content: String::new(),
        tool_calls: vec![call],
    }
}

async fn approve<T: Send + 'static>(
    future: impl Future<Output = Result<T, SubagentWorkerError>> + Send + 'static,
    bridge: &SubagentEventBridge,
    events: &mpsc::Receiver<SubagentBridgeEvent>,
) -> T {
    let running = tokio::spawn(future);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !running.is_finished() {
        while let Ok(event) = events.try_recv() {
            if let SubagentBridgeEvent::Event {
                child_session_id,
                event,
            } = event
            {
                if let AgentEventKind::PermissionRequested { request_id, .. } = event.kind {
                    assert!(!bridge.resolve_permission(
                        "unrelated-child",
                        &request_id,
                        PermissionChoice::Allow
                    ));
                    assert!(bridge.resolve_permission(
                        &child_session_id,
                        &request_id,
                        PermissionChoice::Allow
                    ));
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child command fixture timed out"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    running.await.unwrap().unwrap()
}

struct ChildCommandsModel {
    parent_command: Mutex<Option<String>>,
    sibling_command: Mutex<Option<String>>,
    own_command: Mutex<Option<String>>,
    polls: AtomicUsize,
    waiting: AtomicBool,
}

#[async_trait]
impl ModelBackend for ChildCommandsModel {
    fn model_id(&self) -> &str {
        "own-child-command-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([74; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        assert!(tools
            .iter()
            .any(|definition| definition.name == "start_command"));
        let prompt = messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .unwrap()
            .content
            .as_str();
        if prompt == "hold-child" {
            self.waiting.store(true, Ordering::SeqCst);
            _cancel.cancelled().await;
            return Err(AgentError::Cancelled);
        }
        if prompt == "start-child" {
            if let Some(output) = result(messages, "child-start") {
                *self.own_command.lock().unwrap() = Some(
                    serde_json::from_str::<Value>(output).unwrap()["session_id"]
                        .as_str()
                        .unwrap()
                        .into(),
                );
                return Ok(ModelOutput {
                    content: "The child command stays alive for follow-up.".into(),
                    tool_calls: vec![],
                });
            }
            return Ok(response(tool(
                "child-start",
                "start_command",
                json!({"command":"/bin/sh","args":["-c","printf 'CHILD-READY\\n'; while IFS= read -r line; do printf 'CHILD:%s\\n' \"$line\"; done; printf 'EOF\\n'"]}),
            )));
        }
        assert_eq!(prompt, "continue-child");
        if result(messages, "foreign-parent").is_none() {
            return Ok(ModelOutput {
                content: String::new(),
                tool_calls: vec![
                    tool(
                        "foreign-parent",
                        "write_stdin",
                        json!({"session_id":self.parent_command.lock().unwrap().as_ref().unwrap(),"input":"must-not-deliver\n"}),
                    ),
                    tool(
                        "foreign-sibling",
                        "write_stdin",
                        json!({"session_id":self.sibling_command.lock().unwrap().as_ref().unwrap(),"input":"must-not-deliver\n"}),
                    ),
                ],
            });
        }
        assert!(result(messages, "foreign-parent")
            .unwrap()
            .contains("not found"));
        assert!(result(messages, "foreign-sibling")
            .unwrap()
            .contains("not found"));
        let own = self.own_command.lock().unwrap().clone().unwrap();
        if result(messages, "own-stdin").is_none() {
            return Ok(response(tool(
                "own-stdin",
                "write_stdin",
                json!({"session_id":own,"input":"café 🌱\n","close_after":true}),
            )));
        }
        if messages
            .iter()
            .filter(|message| {
                message
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("child-poll-"))
            })
            .any(|message| {
                serde_json::from_str::<Value>(&message.content).unwrap()["finished"] == true
            })
        {
            return Ok(ModelOutput {
                content: "The same child command reached EOF.".into(),
                tool_calls: vec![],
            });
        }
        Ok(response(tool(
            &format!("child-poll-{}", self.polls.fetch_add(1, Ordering::SeqCst)),
            "poll_command",
            json!({"session_id":own,"wait_ms":1000}),
        )))
    }
}

#[test]
fn child_commands_use_owned_capture_across_followup_runtimes_and_reject_parent_and_sibling_ids() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let parent = sessions.create(&workspace, "native").unwrap();
    let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
    let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
    let profile = registry
        .profile(NativeCommandProfileOptions::default())
        .unwrap();
    let bridge = Arc::new(SubagentEventBridge::new());
    let events = bridge.subscribe();
    let model = Arc::new(ChildCommandsModel {
        parent_command: Mutex::new(None),
        sibling_command: Mutex::new(None),
        own_command: Mutex::new(None),
        polls: AtomicUsize::new(0),
        waiting: AtomicBool::new(false),
    });
    let hooks = Arc::new(
        CommandHooks::new(vec![
            CommandHookConfig {
                id: "child-before-start".into(),
                phase: HookPhase::PreToolUse,
                tool_name: "start_command".into(),
                executable: std::fs::canonicalize("/bin/sh").unwrap(),
                args: vec!["-c".into(), "cat > child-pre.json; printf pre >> hooks.log; printf '%s' '{\"decision\":\"allow\"}'".into()],
                cwd: workspace.clone(),
                timeout_ms: 2000,
                max_output_bytes: 4096,
            },
            CommandHookConfig {
                id: "child-after-start".into(),
                phase: HookPhase::PostToolUse,
                tool_name: "start_command".into(),
                executable: std::fs::canonicalize("/bin/sh").unwrap(),
                args: vec!["-c".into(), "cat > child-post.json; printf post >> hooks.log; printf '%s' '{\"decision\":\"allow\"}'".into()],
                cwd: workspace.clone(),
                timeout_ms: 2000,
                max_output_bytes: 4096,
            },
        ])
        .unwrap(),
    );
    let worker = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        base(),
        "local",
        "https://child.fixture/v1",
    )
    .with_persistent_commands(profile.clone())
    .with_command_hooks(hooks)
    .with_event_bridge(bridge.clone())
    .with_permission_policy(
        PermissionPolicy::new().with_tool("start_command", PermissionMode::Ask),
    );
    let child = worker
        .prepare_child(
            &parent_lease,
            "child-persistent-start",
            "start-child",
            TurnMode::Build,
        )
        .unwrap();
    let sibling = worker
        .prepare_child(
            &parent_lease,
            "sibling-persistent-reserved",
            "start-child",
            TurnMode::Build,
        )
        .unwrap();
    assert!(
        registry.managers().unwrap().is_empty(),
        "pure reservation/catalog construction created managers"
    );
    let control = coordinator.issue_control(&parent_lease).unwrap();
    let parent_tools = profile
        .overlay(base(), sessions.clone(), &parent_lease)
        .unwrap();
    let sibling_run = coordinator
        .start_child(&parent_lease, &sibling.session.id)
        .unwrap();
    let sibling_tools = profile
        .overlay(base(), sessions.clone(), sibling_run.native_lease())
        .unwrap();
    let context = ToolContext {
        workspace: workspace.clone(),
        cancel: CancellationToken::new(),
    };
    let first = runtime();
    for (tools, name, destination) in [
        (&parent_tools, "parent", &model.parent_command),
        (&sibling_tools, "sibling", &model.sibling_command),
    ] {
        let started = first.block_on(tools.execute(&tool(name,"start_command",json!({"command":"/bin/sh","args":["-c",format!("printf READY; IFS= read -r line; printf '%s' \"$line\" > {name}-injected; IFS= read -r final")]})),&context)).unwrap();
        *destination.lock().unwrap() = Some(
            serde_json::from_str::<Value>(&started.content).unwrap()["session_id"]
                .as_str()
                .unwrap()
                .into(),
        );
    }
    drop(sibling_run);
    drop(parent_lease);
    let initial_worker = worker.clone();
    let initial_control = control.clone();
    let child_id = child.session.id.clone();
    let initial_id = child_id.clone();
    let initial = first.block_on(approve(
        async move {
            initial_worker
                .run_prepared(
                    initial_control,
                    initial_id,
                    "child-persistent-start".into(),
                    "start-child".into(),
                    TurnMode::Build,
                    CancellationToken::new(),
                )
                .await
        },
        &bridge,
        &events,
    ));
    assert_eq!(initial.status, SubagentStatus::Completed, "{initial:?}");
    assert_eq!(
        std::fs::read(workspace.join("hooks.log")).unwrap(),
        b"prepost"
    );
    for file in ["child-pre.json", "child-post.json"] {
        let invocation: Value =
            serde_json::from_slice(&std::fs::read(workspace.join(file)).unwrap()).unwrap();
        assert_eq!(invocation["session_id"], child_id);
        assert_eq!(invocation["tool_call_id"], "child-start");
    }
    let own = model.own_command.lock().unwrap().clone().unwrap();
    let retained = registry.manager(&child_id).unwrap().unwrap();
    assert_eq!(retained.native_session_owner(), Some(child_id.as_str()));
    assert!(retained.control_snapshot(&own).unwrap().stdin_open());
    drop(first);
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    coordinator
        .enqueue_input(
            &lease,
            &child_id,
            "actual-followup",
            "continue-child",
            TurnMode::Build,
        )
        .unwrap();
    drop(lease);
    let next_worker = worker.clone();
    let next_child = child_id.clone();
    let second = runtime();
    second.block_on(approve(
        async move {
            next_worker
                .drain_queued_prepared(control, next_child, CancellationToken::new())
                .await
        },
        &bridge,
        &events,
    ));
    assert!(retained.list_sessions().unwrap().is_empty());
    assert!(
        !workspace.join("parent-injected").exists() && !workspace.join("sibling-injected").exists()
    );
    let child_events = sessions.load(&child_id).unwrap().unwrap().events;
    assert_eq!(
        child_events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
            .count(),
        2
    );
    for id in ["foreign-parent", "foreign-sibling"] {
        assert!(child_events.iter().any(|event| matches!(&event.kind,AgentEventKind::ToolFinished{tool_call_id,is_error:true,..} if tool_call_id==id)));
    }
    let reopened = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let output = NativeCommandOutputStore::new(reopened.clone());
    let captures = output.list(&child_id, &workspace).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].header.binding.command_id, own);
    assert_eq!(
        captures[0].header.binding.manager_generation,
        retained.generation()
    );
    let page = output
        .read(&captures[0].header.binding, 0, 1024, 64)
        .unwrap();
    assert!(page.omitted_ranges.is_empty());
    assert!(page
        .chunks
        .iter()
        .all(|chunk| chunk.stream == NativeCommandOutputStream::Stdout));
    assert_eq!(
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.clone())
            .collect::<Vec<_>>(),
        "CHILD-READY\nCHILD:café 🌱\nEOF\n".as_bytes()
    );
    assert_eq!(page.summary.terminal.unwrap().exit_code, Some(0));
    registry.revoke_all().unwrap();
    let restored_registry = Arc::new(NativeCommandRegistry::new(reopened));
    assert!(restored_registry.managers().unwrap().is_empty());
    assert!(restored_registry.manager(&child_id).unwrap().is_none());
    assert_eq!(output.list(&child_id, &workspace).unwrap(), captures);
}

struct SurfaceModel(AtomicUsize);
#[async_trait]
impl ModelBackend for SurfaceModel {
    fn model_id(&self) -> &str {
        "legacy-child-command-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([75; 32])
    }
    async fn complete(
        &self,
        _messages: &[AgentMessage],
        tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        assert!(!tools
            .iter()
            .any(|tool| crate::tools::persistent_command_tools::is_persistent_tool(&tool.name)));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutput {
            content: "surface preserved".into(),
            tool_calls: vec![],
        })
    }
}

struct PlanTransitionModel {
    own_command: Mutex<Option<String>>,
    surfaces: Mutex<Vec<(String, Vec<String>)>>,
    polls: AtomicUsize,
}

#[async_trait]
impl ModelBackend for PlanTransitionModel {
    fn model_id(&self) -> &str {
        "plan-transition-command-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([77; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        let prompt = messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .unwrap()
            .content
            .as_str();
        self.surfaces.lock().unwrap().push((
            prompt.into(),
            tools.iter().map(|tool| tool.name.clone()).collect(),
        ));
        if prompt != "profile-build" {
            assert!(matches!(
                prompt,
                "legacy-plan" | "profile-plan" | "legacy-build"
            ));
            assert!(!tools.iter().any(|tool| {
                crate::tools::persistent_command_tools::is_persistent_tool(&tool.name)
            }));
            return Ok(ModelOutput {
                content: "The reserved capability choice is preserved.".into(),
                tool_calls: vec![],
            });
        }
        for name in [
            "start_command",
            "poll_command",
            "write_stdin",
            "terminate_command",
        ] {
            assert!(tools.iter().any(|tool| tool.name == name));
        }
        let Some(started) = result(messages, "profile-start") else {
            return Ok(response(tool(
                "profile-start",
                "start_command",
                json!({"command":"/bin/sh","args":["-c","printf 'PROFILE-READY\\n'; IFS= read -r line; printf 'PROFILE:%s\\n' \"$line\"; cat >/dev/null; printf 'EOF\\n'"]}),
            )));
        };
        let own = serde_json::from_str::<Value>(started).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        *self.own_command.lock().unwrap() = Some(own.clone());
        if result(messages, "profile-stdin").is_none() {
            return Ok(response(tool(
                "profile-stdin",
                "write_stdin",
                json!({"session_id":own,"input":"frozen 🌱\n","close_after":true}),
            )));
        }
        if messages.iter().any(|message| {
            message
                .tool_call_id
                .as_deref()
                .is_some_and(|id| id.starts_with("profile-poll-"))
                && serde_json::from_str::<Value>(&message.content).unwrap()["finished"] == true
        }) {
            return Ok(ModelOutput {
                content: "The opted-in Plan child retained its own Build commands.".into(),
                tool_calls: vec![],
            });
        }
        Ok(response(tool(
            &format!("profile-poll-{}", self.polls.fetch_add(1, Ordering::SeqCst)),
            "poll_command",
            json!({"session_id":own,"wait_ms":1000}),
        )))
    }
}

#[test]
fn plan_reservations_freeze_command_profile_across_build_followup_and_host_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let parent = sessions.create(&workspace, "native").unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
    let model = Arc::new(PlanTransitionModel {
        own_command: Mutex::new(None),
        surfaces: Mutex::new(Vec::new()),
        polls: AtomicUsize::new(0),
    });
    let bridge = Arc::new(SubagentEventBridge::new());
    let events = bridge.subscribe();
    let legacy = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        base(),
        "local",
        "https://plan-child.fixture/v1",
    )
    .with_event_bridge(bridge.clone())
    .with_permission_policy(
        PermissionPolicy::new().with_tool("start_command", PermissionMode::Ask),
    );
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let child = legacy
        .prepare_child(&lease, "legacy-plan-child", "legacy-plan", TurnMode::Plan)
        .unwrap();
    let control = coordinator.issue_control(&lease).unwrap();
    drop(lease);
    let first = runtime();
    let initial = first
        .block_on(legacy.run_prepared(
            control.clone(),
            child.session.id.clone(),
            "legacy-plan-child".into(),
            "legacy-plan".into(),
            TurnMode::Plan,
            CancellationToken::new(),
        ))
        .unwrap();
    assert_eq!(initial.status, SubagentStatus::Completed);

    // A newer host opts in after the legacy child's first Plan completed.
    let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
    let profile = registry.profile(Default::default()).unwrap();
    let enabled = legacy.clone().with_persistent_commands(profile);
    assert_ne!(
        legacy
            .spec_for(&workspace, "plan", TurnMode::Plan, 1)
            .unwrap()
            .context_digest,
        enabled
            .spec_for(&workspace, "plan", TurnMode::Plan, 1)
            .unwrap()
            .context_digest
    );
    assert_eq!(
        legacy
            .catalog_for_mode(TurnMode::Plan, 1)
            .unwrap()
            .digest_hex(),
        enabled
            .catalog_for_mode(TurnMode::Plan, 1)
            .unwrap()
            .digest_hex()
    );
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let opted_in = enabled
        .prepare_child(&lease, "profile-plan-child", "profile-plan", TurnMode::Plan)
        .unwrap();
    drop(lease);
    let initial = first
        .block_on(enabled.run_prepared(
            control.clone(),
            opted_in.session.id.clone(),
            "profile-plan-child".into(),
            "profile-plan".into(),
            TurnMode::Plan,
            CancellationToken::new(),
        ))
        .unwrap();
    assert_eq!(initial.status, SubagentStatus::Completed);
    assert!(registry.managers().unwrap().is_empty());
    drop(first);

    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    for (id, child, prompt) in [
        ("legacy-build-input", &child.session.id, "legacy-build"),
        ("profile-build-input", &opted_in.session.id, "profile-build"),
    ] {
        enabled.validate_follow_up_child(child).unwrap();
        coordinator
            .enqueue_input(&lease, child, id, prompt, TurnMode::Build)
            .unwrap();
    }
    drop(lease);
    let second = runtime();
    second
        .block_on(enabled.drain_queued_prepared(
            control.clone(),
            child.session.id.clone(),
            CancellationToken::new(),
        ))
        .unwrap();
    assert!(registry.manager(&child.session.id).unwrap().is_none());
    let output = NativeCommandOutputStore::new(sessions.clone());
    assert!(output
        .list(&child.session.id, &workspace)
        .unwrap()
        .is_empty());
    let next_worker = enabled.clone();
    let opted_in_id = opted_in.session.id.clone();
    second.block_on(approve(
        async move {
            next_worker
                .drain_queued_prepared(control, opted_in_id, CancellationToken::new())
                .await
        },
        &bridge,
        &events,
    ));
    let own = model.own_command.lock().unwrap().clone().unwrap();
    assert_eq!(registry.managers().unwrap().len(), 1);
    assert!(registry.manager(&opted_in.session.id).unwrap().is_some());
    let reopened = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let history = NativeCommandOutputStore::new(reopened);
    assert!(history
        .list(&child.session.id, &workspace)
        .unwrap()
        .is_empty());
    let captures = history.list(&opted_in.session.id, &workspace).unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].header.binding.command_id, own);
    let page = history
        .read(&captures[0].header.binding, 0, 4096, 64)
        .unwrap();
    assert!(page.omitted_ranges.is_empty());
    assert_eq!(
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.clone())
            .collect::<Vec<_>>(),
        "PROFILE-READY\nPROFILE:frozen 🌱\nEOF\n".as_bytes()
    );
    assert_eq!(page.summary.terminal.unwrap().exit_code, Some(0));
    let surfaces = model.surfaces.lock().unwrap();
    assert!(surfaces.iter().any(|(prompt, _)| prompt == "legacy-build"));
    assert!(surfaces.iter().any(|(prompt, _)| prompt == "profile-build"));
    registry.revoke_all().unwrap();
}

struct PlanControlsModel {
    waiting: AtomicBool,
    release: tokio::sync::Notify,
}

#[async_trait]
impl ModelBackend for PlanControlsModel {
    fn model_id(&self) -> &str {
        "plan-existing-command-controls-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([78; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        let prompt = messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .unwrap()
            .content
            .as_str();
        assert_eq!(prompt, "plan-manual");
        assert!(!tools.iter().any(|tool| {
            crate::tools::persistent_command_tools::is_persistent_tool(&tool.name)
        }));
        self.waiting.store(true, Ordering::SeqCst);
        tokio::select! {
            _ = self.release.notified() => {}
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
        }
        Ok(ModelOutput {
            content: "Plan completed after a separately confirmed manual control.".into(),
            tool_calls: vec![],
        })
    }
}

#[test]
fn plan_worker_processes_confirmed_host_fixture_controls_without_expanding_model_tools() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let parent = sessions.create(&workspace, "native").unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
    let policy = NativeManualCommandPolicy {
        permissions: PermissionPolicy::new(),
        run_command_enabled: true,
        isolation: crate::CommandIsolation::HostUnrestricted,
        hooks: Arc::new(CommandHooks::default()),
    };
    let registry = Arc::new(
        NativeCommandRegistry::new(sessions.clone())
            .with_manual_policy_source(Arc::new(move || Ok(policy.clone()))),
    );
    let profile = registry.profile(Default::default()).unwrap();
    let bridge = Arc::new(SubagentEventBridge::new());
    let model = Arc::new(PlanControlsModel {
        waiting: AtomicBool::new(false),
        release: tokio::sync::Notify::new(),
    });
    let worker = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        base(),
        "local",
        "https://plan-controls.fixture/v1",
    )
    .with_persistent_commands(profile.clone())
    .with_event_bridge(bridge.clone())
    .with_permission_policy(
        PermissionPolicy::new().with_tool("start_command", PermissionMode::Ask),
    );
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let child = worker
        .prepare_child(&lease, "host-before-plan", "plan-manual", TurnMode::Plan)
        .unwrap();
    let control = coordinator.issue_control(&lease).unwrap();
    drop(lease);
    let child_id = child.session.id.clone();
    // Explicit trusted-host fixture, not a model tool or mailbox Plan input.
    // The mailbox admits only Build follow-ups. Here a native child Plan sees
    // an existing scoped process, without gaining any model execution surface.
    let host_lease = sessions.try_acquire_native_lease(&child_id).unwrap();
    let host_tools = profile
        .overlay(base(), sessions.clone(), &host_lease)
        .unwrap();
    let first = runtime();
    let started = first.block_on(host_tools.execute(
        &tool("host-fixture-start", "start_command", json!({"command":"/bin/sh","args":["-c","printf 'HOST-READY\\n'; while IFS= read -r line; do printf 'HUMAN:%s\\n' \"$line\"; done; printf 'EOF\\n'"]})),
        &ToolContext {workspace: workspace.clone(), cancel:CancellationToken::new()},
    )).unwrap();
    let command = serde_json::from_str::<Value>(&started.content).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let actor = profile
        .actor(sessions.clone(), &host_lease)
        .unwrap()
        .unwrap();
    let manager = registry.manager(&child_id).unwrap().unwrap();
    assert!(manager.control_snapshot(&command).unwrap().stdin_open());
    assert!(sessions.load(&child_id).unwrap().unwrap().events.is_empty());
    drop(host_lease);
    drop(host_tools);
    drop(first);
    let next_worker = worker.clone();
    let next_child = child_id.clone();
    let second = runtime();
    let operation_id = second.block_on(async {
        let running = tokio::spawn(async move {
            next_worker
                .run_prepared(
                    control,
                    next_child,
                    "host-before-plan".into(),
                    "plan-manual".into(),
                    TurnMode::Plan,
                    CancellationToken::new(),
                )
                .await
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !model.waiting.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "Plan model did not start"
            );
            assert!(!running.is_finished(), "Plan ended before manual control");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(sessions.try_acquire_native_lease(&child_id).is_err());
        assert!(Arc::ptr_eq(
            &actor,
            &registry.manual_actor(&child_id).unwrap().unwrap()
        ));
        let request = actor
            .prepare_request(
                &command,
                NativeCommandOperationAction::Stdin {
                    input: "café 🌱\n".into(),
                    close_after: true,
                },
            )
            .unwrap();
        let operation_id = request.operation_id.clone();
        assert_eq!(
            actor.enqueue_confirmed(request).unwrap().state,
            NativeCommandOperationState::Requested
        );
        loop {
            let saved = actor
                .operations()
                .snapshot(&child_id, &operation_id)
                .unwrap()
                .unwrap();
            if let NativeCommandOperationState::Finished { outcome } = saved.state {
                assert!(matches!(
                    outcome,
                    NativeCommandOperationOutcome::Succeeded { .. }
                ));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Plan failed to ACK manual command"
            );
            assert!(
                !running.is_finished(),
                "Plan ended without a manual receipt"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!manager.control_snapshot(&command).unwrap().stdin_open());
        model.release.notify_one();
        assert_eq!(
            running.await.unwrap().unwrap().status,
            SubagentStatus::Completed
        );
        operation_id
    });
    let child_events = sessions.load(&child_id).unwrap().unwrap().events;
    assert_eq!(
        child_events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
            .count(),
        1
    );
    assert!(child_events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::TurnAdmitted {
            mode: TurnMode::Plan,
            ..
        }
    )));
    assert!(!child_events.iter().any(|event| matches!(&event.kind,AgentEventKind::ToolFinished{tool_call_id,..} if tool_call_id==&operation_id)));
    let history = NativeCommandOutputStore::new(sessions.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let capture = loop {
        let capture = history.list(&child_id, &workspace).unwrap().remove(0);
        if capture.terminal.is_some() {
            break capture;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "command EOF not captured"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let page = history.read(&capture.header.binding, 0, 4096, 64).unwrap();
    assert_eq!(
        page.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.clone())
            .collect::<Vec<_>>(),
        "HOST-READY\nHUMAN:café 🌱\nEOF\n".as_bytes()
    );
    assert_eq!(page.summary.terminal.unwrap().exit_code, Some(0));
    registry.revoke_all().unwrap();
}

#[test]
fn legacy_ready_recovery_and_plan_keep_original_catalog_while_revoked_profiles_fail_before_claim() {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let parent = sessions.create(root.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
    let model = Arc::new(SurfaceModel(AtomicUsize::new(0)));
    let legacy = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        base(),
        "local",
        "https://legacy.child/v1",
    );
    let ready = legacy
        .prepare_child(&lease, "legacy-ready", "legacy", TurnMode::Build)
        .unwrap();
    let interrupted = legacy
        .prepare_child(&lease, "legacy-recovery", "legacy", TurnMode::Build)
        .unwrap();
    drop(
        coordinator
            .start_child(&lease, &interrupted.session.id)
            .unwrap(),
    );
    let control = coordinator.issue_control(&lease).unwrap();
    let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
    let profile = registry.profile(Default::default()).unwrap();
    let enabled = legacy
        .clone()
        .with_persistent_commands(profile.clone())
        .with_native_goals()
        .with_nested_subagents(Default::default());
    enabled
        .validate_initial_child(&ready.session.id, "legacy-ready", "legacy", TurnMode::Build)
        .unwrap();
    let plan = enabled
        .prepare_child(&lease, "new-plan", "plan", TurnMode::Plan)
        .unwrap();
    let current = enabled
        .prepare_child(&lease, "new-persistent", "current", TurnMode::Build)
        .unwrap();
    let changed = legacy
        .clone()
        .with_persistent_commands(
            registry
                .profile(NativeCommandProfileOptions {
                    limits: crate::PersistentCommandLimits {
                        max_lifetime: Duration::from_secs(12),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .unwrap(),
        )
        .with_native_goals()
        .with_nested_subagents(Default::default());
    assert!(changed
        .validate_initial_child(
            &current.session.id,
            "new-persistent",
            "current",
            TurnMode::Build
        )
        .is_err());
    assert!(registry.managers().unwrap().is_empty());
    drop(lease);
    let runtime = runtime();
    runtime.block_on(async {
        assert_eq!(
            enabled
                .run_prepared(
                    control.clone(),
                    ready.session.id.clone(),
                    "legacy-ready".into(),
                    "legacy".into(),
                    TurnMode::Build,
                    CancellationToken::new()
                )
                .await
                .unwrap()
                .status,
            SubagentStatus::Completed
        );
        assert!(matches!(
            enabled
                .resume_prepared(
                    control.clone(),
                    interrupted.session.id.clone(),
                    "legacy-recovery".into(),
                    "legacy".into(),
                    TurnMode::Build,
                    CancellationToken::new()
                )
                .await
                .unwrap(),
            SubagentRecoveryState::Terminal(SubagentTurnResult {
                status: SubagentStatus::Completed,
                ..
            })
        ));
        assert_eq!(
            enabled
                .run_prepared(
                    control.clone(),
                    plan.session.id,
                    "new-plan".into(),
                    "plan".into(),
                    TurnMode::Plan,
                    CancellationToken::new()
                )
                .await
                .unwrap()
                .status,
            SubagentStatus::Completed
        );
        assert!(registry.managers().unwrap().is_empty());
        registry.revoke_all().unwrap();
        let before = model.0.load(Ordering::SeqCst);
        let error = enabled
            .run_prepared(
                control,
                current.session.id.clone(),
                "new-persistent".into(),
                "current".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("revoked"));
        assert_eq!(model.0.load(Ordering::SeqCst), before);
        assert!(sessions
            .load(&current.session.id)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    });
    let mut original_definitions = legacy
        .catalog_for_mode(TurnMode::Build, 1)
        .unwrap()
        .definitions()
        .to_vec();
    original_definitions.push(crate::plan_tool::definition());
    let original_catalog_digest = ToolCatalog::new(original_definitions).unwrap().digest_hex();
    for child in [ready, interrupted] {
        let events = sessions.load(&child.session.id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(&event.kind,AgentEventKind::ToolCatalogCommitted{digest,..} if digest == &original_catalog_digest)));
    }
}

#[test]
fn bridge_cancellation_commits_before_revoking_the_retained_child_process_and_notifying_model() {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let parent = sessions.create(root.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
    let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
    let bridge = Arc::new(SubagentEventBridge::new());
    let events = bridge.subscribe();
    let model = Arc::new(ChildCommandsModel {
        parent_command: Mutex::new(None),
        sibling_command: Mutex::new(None),
        own_command: Mutex::new(None),
        polls: AtomicUsize::new(0),
        waiting: AtomicBool::new(false),
    });
    let worker = SubagentWorker::new(
        coordinator.clone(),
        sessions.clone(),
        model.clone(),
        base(),
        "local",
        "https://cancel.child/v1",
    )
    .with_persistent_commands(registry.profile(Default::default()).unwrap())
    .with_event_bridge(bridge.clone())
    .with_permission_policy(
        PermissionPolicy::new().with_tool("start_command", PermissionMode::Ask),
    );
    let child = worker
        .prepare_child(
            &lease,
            "cancel-retained-child",
            "start-child",
            TurnMode::Build,
        )
        .unwrap();
    let control = coordinator.issue_control(&lease).unwrap();
    drop(lease);
    let runtime = runtime();
    runtime.block_on(async {
        let first_worker = worker.clone();
        let first_child = child.session.id.clone();
        let first_control = control.clone();
        let result = approve(
            async move {
                first_worker
                    .run_prepared(
                        first_control,
                        first_child,
                        "cancel-retained-child".into(),
                        "start-child".into(),
                        TurnMode::Build,
                        CancellationToken::new(),
                    )
                    .await
            },
            &bridge,
            &events,
        )
        .await;
        assert_eq!(result.status, SubagentStatus::Completed);
        let command = model.own_command.lock().unwrap().clone().unwrap();
        let manager = registry.manager(&child.session.id).unwrap().unwrap();
        assert!(manager.control_snapshot(&command).unwrap().stdin_open());
        while events.try_recv().is_ok() {}
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        coordinator
            .enqueue_input(
                &lease,
                &child.session.id,
                "hold-input",
                "hold-child",
                TurnMode::Build,
            )
            .unwrap();
        drop(lease);
        let running_worker = worker.clone();
        let running_child = child.session.id.clone();
        let running = tokio::spawn(async move {
            running_worker
                .drain_queued_prepared(control, running_child, CancellationToken::new())
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !model.waiting.load(Ordering::SeqCst)
                || !bridge.active_child_ids().contains(&child.session.id)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(bridge.cancel_child(&child.session.id).unwrap());
        // cancel_child returns only after its durable affected-ID transaction
        // and synchronous process revocation, before the model can complete.
        assert!(coordinator.list_children_for_ui(&parent.id).unwrap()[0].cancellation_requested);
        assert!(manager.control_snapshot(&command).unwrap().revoked);
        assert!(registry.manager(&child.session.id).unwrap().is_none());
        assert!(manager
            .write_stdin(
                &command,
                b"must-not-deliver\n",
                false,
                &CancellationToken::new()
            )
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(10), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while manager
                .inspect_pending_output(&command)
                .unwrap()
                .exit
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let capture = NativeCommandOutputStore::new(sessions.clone())
            .list(&child.session.id, root.path())
            .unwrap()
            .remove(0);
        assert_eq!(
            capture.terminal.unwrap().exit_kind,
            Some(openide_core::NativeCommandOutputExit::Terminated)
        );
        let events = sessions.load(&child.session.id).unwrap().unwrap().events;
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::TurnCancelled)));
    });
}

struct ApprovalModel;
#[async_trait]
impl ModelBackend for ApprovalModel {
    fn model_id(&self) -> &str {
        "child-command-approval"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([76; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        if result(messages, "approved-start").is_some() {
            return Ok(ModelOutput {
                content: "No payload is permitted by this fixture.".into(),
                tool_calls: vec![],
            });
        }
        Ok(response(tool(
            "approved-start",
            "start_command",
            json!({"command":"/bin/sh","args":["-c","printf effect > forbidden-payload"]}),
        )))
    }
}

#[test]
fn child_start_approval_denial_and_revocation_after_request_prevent_payload_and_capture_registration(
) {
    for revoke in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator =
            Arc::new(SubagentCoordinator::new(sessions.clone(), Default::default()).unwrap());
        let registry = Arc::new(NativeCommandRegistry::new(sessions.clone()));
        let bridge = Arc::new(SubagentEventBridge::new());
        let events = bridge.subscribe();
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            Arc::new(ApprovalModel),
            base(),
            "local",
            "https://approval.child/v1",
        )
        .with_persistent_commands(registry.profile(Default::default()).unwrap())
        .with_event_bridge(bridge.clone())
        .with_permission_policy(
            PermissionPolicy::new().with_tool("start_command", PermissionMode::Ask),
        );
        let child = worker
            .prepare_child(&lease, "approval-child", "approval", TurnMode::Build)
            .unwrap();
        let control = coordinator.issue_control(&lease).unwrap();
        drop(lease);
        let runtime = runtime();
        runtime.block_on(async {
            let child_id=child.session.id.clone();
            let running=tokio::spawn(async move { worker.run_prepared(control,child_id,"approval-child".into(),"approval".into(),TurnMode::Build,CancellationToken::new()).await });
            let request_id=tokio::time::timeout(Duration::from_secs(10),async {
                loop {
                    if let Ok(SubagentBridgeEvent::Event{child_session_id,event})=events.try_recv() {
                        if let AgentEventKind::PermissionRequested{request_id,..}=event.kind {
                            assert_eq!(child_session_id,child.session.id);
                            break request_id;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.unwrap();
            assert!(!root.path().join("forbidden-payload").exists());
            if revoke { registry.revoke_all().unwrap(); }
            assert!(bridge.resolve_permission(&child.session.id,&request_id,if revoke {PermissionChoice::Allow} else {PermissionChoice::Deny}));
            tokio::time::timeout(Duration::from_secs(10),running).await.unwrap().unwrap().unwrap();
            assert!(!root.path().join("forbidden-payload").exists());
            assert!(NativeCommandOutputStore::new(sessions.clone()).list(&child.session.id,root.path()).unwrap().is_empty());
            let events=sessions.load(&child.session.id).unwrap().unwrap().events;
            assert!(events.iter().any(|event| matches!(&event.kind,AgentEventKind::ToolFinished{tool_call_id,is_error:true,..} if tool_call_id=="approved-start")));
        });
    }
}
