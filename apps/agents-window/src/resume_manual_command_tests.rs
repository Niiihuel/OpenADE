use super::*;
use openide_core::{
    NativeCommandOperationAction, NativeCommandOperationRequest, NativeCommandOperationState,
    NativeCommandOperationStore,
};
use openide_harness::{
    NativeManualCommandActor, NativeManualCommandPolicy, PersistentCommandLimits,
};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

struct ManualGateModel(Arc<AtomicUsize>);

#[cfg(unix)]
#[tokio::test]
async fn recovery_actor_from_another_store_handle_stops_before_idle_or_mcp_effects() {
    for active in [false, true] {
        let fixture = ManualGateFixture::new(active, false);
        let request = fixture.request();
        let other = Arc::new(SessionStore::open(fixture._root.path().join("sessions")).unwrap());
        let actor = Arc::new(
            NativeManualCommandActor::new(
                other,
                fixture.owner.clone(),
                None,
                Arc::new(|| Ok(manual_gate_policy())),
            )
            .unwrap(),
        );
        let (sender, _receiver) = mpsc::sync_channel(64);
        let (_commands, commands) = tokio::sync::mpsc::unbounded_channel();
        let result = run_recovery_with_command_actor(
            request,
            fixture.lease,
            commands,
            &sender,
            CancellationToken::new(),
            Some(actor),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(fixture.preparation_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.model_calls.load(Ordering::SeqCst), 0);
        assert!(!fixture.workspace.join("prepare.effect").exists());
        let saved = NativeCommandOperationStore::new(fixture.sessions.clone())
            .snapshot(&fixture.owner, &fixture.operation.operation_id)
            .unwrap()
            .unwrap();
        assert!(matches!(
            saved.state,
            NativeCommandOperationState::Requested
        ));
    }
}

#[async_trait]
impl ModelBackend for ManualGateModel {
    fn model_id(&self) -> &str {
        "test-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([7; 32])
    }
    async fn complete(
        &self,
        _: &[AgentMessage],
        _: &[ToolDefinition],
        _: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelOutput {
            content: "Recovered after stale requested control was rejected".into(),
            tool_calls: Vec::new(),
        })
    }
}

struct ManualGatePreparation {
    calls: Arc<AtomicUsize>,
    workspace: PathBuf,
}

#[async_trait]
impl ToolPreparation for ManualGatePreparation {
    async fn prepare(
        &self,
        _: &Path,
        base_tools: Arc<dyn ToolExecutor>,
        _: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::write(
            self.workspace.join("prepare.effect"),
            b"external service preparation was reached",
        )
        .unwrap();
        Ok(Box::new(TrackingSession {
            tools: base_tools,
            shutdown: Arc::new(AtomicBool::new(false)),
        }))
    }
}

fn manual_gate_policy() -> NativeManualCommandPolicy {
    NativeManualCommandPolicy {
        permissions: PermissionPolicy::new(),
        run_command_enabled: true,
        isolation: CommandIsolation::HostUnrestricted,
        hooks: Arc::new(CommandHooks::default()),
    }
}

struct ManualGateFixture {
    _root: tempfile::TempDir,
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    owner: String,
    workspace: PathBuf,
    operation: NativeCommandOperationRequest,
    lease: NativeSessionLease,
    preparation_calls: Arc<AtomicUsize>,
    model_calls: Arc<AtomicUsize>,
    local: Arc<LocalTools>,
    original_manager: Arc<PersistentCommandManager>,
}

impl ManualGateFixture {
    fn new(active: bool, started: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let checkpoints =
            Arc::new(CheckpointFileStore::new(root.path().join("checkpoints")).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        let local = Arc::new(LocalTools::new());
        if active {
            sessions
                .admit_native_turn(
                    &session.id,
                    "manual-gate-turn",
                    "Recover this",
                    "test-model",
                    TurnMode::Build,
                )
                .unwrap();
            append(
                &sessions,
                &session.id,
                AgentEventKind::TurnStarted {
                    prompt: "Recover this".into(),
                },
            );
            append(
                &sessions,
                &session.id,
                AgentEventKind::ToolCatalogCommitted {
                    digest: ToolCatalog::from_executor(local.as_ref())
                        .unwrap()
                        .digest_hex(),
                },
            );
            append_context(&sessions, &session.id, &local);
            append(
                &sessions,
                &session.id,
                AgentEventKind::ModelRequestStarted {
                    step: 1,
                    model: "test-model".into(),
                },
            );
        }
        // Capture an actual previous process identity, then revoke it. Resume
        // must inspect its durable intention and must never recreate this pipe.
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
                &["-c".into(), "exec cat".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        let source = manual_gate_policy();
        let original_actor = NativeManualCommandActor::new(
            sessions.clone(),
            session.id.clone(),
            Some(manager.clone()),
            Arc::new(move || Ok(source.clone())),
        )
        .unwrap();
        let operation = original_actor
            .prepare_request(
                &command.id,
                NativeCommandOperationAction::Stdin {
                    input: "must not be replayed".into(),
                    close_after: false,
                },
            )
            .unwrap();
        original_actor.enqueue_confirmed(operation.clone()).unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        if started {
            let store = NativeCommandOperationStore::new(sessions.clone());
            store
                .preflight_capacity(&lease, &operation.operation_id, 512 * 1024)
                .unwrap();
            store
                .begin_started(&lease, &operation.operation_id, 0)
                .unwrap();
        }
        manager.stop_all();
        Self {
            _root: root,
            sessions,
            checkpoints,
            owner: session.id,
            workspace,
            operation,
            lease,
            preparation_calls: Arc::new(AtomicUsize::new(0)),
            model_calls: Arc::new(AtomicUsize::new(0)),
            local,
            original_manager: manager,
        }
    }

    fn request(&self) -> ResumeWorkerRequest {
        ResumeWorkerRequest {
            native_automation: None,
            session_id: self.owner.clone(),
            sessions: self.sessions.clone(),
            checkpoints: self.checkpoints.clone(),
            compatibility: compatibility(),
            model: Arc::new(ManualGateModel(self.model_calls.clone())),
            provider: "test".into(),
            endpoint: "https://test.example/v1".into(),
            base_tools: self.local.clone(),
            child_command_profile: None,
            child_tools: self.local.clone(),
            event_bridge: None,
            tool_preparation: Some(Arc::new(ManualGatePreparation {
                calls: self.preparation_calls.clone(),
                workspace: self.workspace.clone(),
            })),
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: Some(budget()),
            system_instructions: String::new(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        }
    }

    fn actor(&self) -> Arc<NativeManualCommandActor> {
        Arc::new(
            NativeManualCommandActor::new(
                self.sessions.clone(),
                self.owner.clone(),
                None,
                Arc::new(|| Ok(manual_gate_policy())),
            )
            .unwrap(),
        )
    }
}

#[cfg(unix)]
#[tokio::test]
async fn requested_without_actor_and_started_with_or_without_actor_stop_before_mcp_and_provider() {
    for (started, with_actor) in [(false, false), (true, false), (true, true)] {
        let fixture = ManualGateFixture::new(true, started);
        let request = fixture.request();
        let actor = with_actor.then(|| fixture.actor());
        let before = fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events;
        let saved_before = NativeCommandOperationStore::new(fixture.sessions.clone())
            .snapshot(&fixture.owner, &fixture.operation.operation_id)
            .unwrap();
        let (sender, _receiver) = mpsc::sync_channel(64);
        let (_commands, commands) = tokio::sync::mpsc::unbounded_channel();
        let result = run_recovery_with_command_actor(
            request,
            fixture.lease,
            commands,
            &sender,
            CancellationToken::new(),
            actor,
        )
        .await;
        assert!(
            result.is_err(),
            "pending control unexpectedly allowed Resume: started={started} actor={with_actor}"
        );
        assert_eq!(fixture.preparation_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.model_calls.load(Ordering::SeqCst), 0);
        assert!(!fixture.workspace.join("prepare.effect").exists());
        assert_eq!(
            fixture
                .sessions
                .load(&fixture.owner)
                .unwrap()
                .unwrap()
                .events,
            before
        );
        assert_eq!(
            NativeCommandOperationStore::new(fixture.sessions.clone())
                .snapshot(&fixture.owner, &fixture.operation.operation_id)
                .unwrap(),
            saved_before
        );
        assert!(fixture
            .original_manager
            .list_sessions()
            .unwrap()
            .iter()
            .all(|session| session.id == fixture.operation.command_id));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn idle_resume_rejects_stale_requested_control_before_complete_and_never_prepares_mcp() {
    for with_actor in [false, true] {
        let fixture = ManualGateFixture::new(false, false);
        let request = fixture.request();
        let actor = with_actor.then(|| fixture.actor());
        let (sender, _receiver) = mpsc::sync_channel(64);
        let (_commands, commands) = tokio::sync::mpsc::unbounded_channel();
        let result = run_recovery_with_command_actor(
            request,
            fixture.lease,
            commands,
            &sender,
            CancellationToken::new(),
            actor,
        )
        .await;
        assert_eq!(fixture.preparation_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.model_calls.load(Ordering::SeqCst), 0);
        assert!(!fixture.workspace.join("prepare.effect").exists());
        let saved = NativeCommandOperationStore::new(fixture.sessions.clone())
            .snapshot(&fixture.owner, &fixture.operation.operation_id)
            .unwrap()
            .unwrap();
        if with_actor {
            assert!(matches!(result.unwrap(), ResumeDisposition::Complete));
            assert!(matches!(
                saved.state,
                NativeCommandOperationState::Rejected { .. }
            ));
        } else {
            assert!(result.is_err());
            assert!(matches!(
                saved.state,
                NativeCommandOperationState::Requested
            ));
        }
        assert!(fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events
            .is_empty());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn active_requested_with_recovery_actor_validates_catalog_before_rejecting_old_process_and_retrying_model(
) {
    let fixture = ManualGateFixture::new(true, false);
    let request = fixture.request();
    let actor = fixture.actor();
    let (sender, _receiver) = mpsc::sync_channel(64);
    let (_commands, commands) = tokio::sync::mpsc::unbounded_channel();
    let result = run_recovery_with_command_actor(
        request,
        fixture.lease,
        commands,
        &sender,
        CancellationToken::new(),
        Some(actor),
    )
    .await
    .unwrap();
    assert!(matches!(result, ResumeDisposition::Complete));
    assert_eq!(fixture.preparation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.model_calls.load(Ordering::SeqCst), 1);
    assert!(fixture.workspace.join("prepare.effect").exists());
    let saved = NativeCommandOperationStore::new(fixture.sessions.clone())
        .snapshot(&fixture.owner, &fixture.operation.operation_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.state,
        NativeCommandOperationState::Rejected { .. }
    ));
    let record = fixture.sessions.load(&fixture.owner).unwrap().unwrap();
    assert_eq!(
        record
            .events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ModelRequestStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        record
            .events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ModelResponseCommitted { .. }))
            .count(),
        1
    );
}
