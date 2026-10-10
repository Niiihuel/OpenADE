//! Deterministic publication races around the real command ledger fence.
use super::*;
use crate::{LocalTools, ModelOutput, NativeManualCommandPolicy, PersistentCommandManager};
use async_trait::async_trait;
use openide_core::{NativeCommandOperationAction, NativeCommandOperationRequest};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct NeverCalledModel;
#[async_trait]
impl ModelBackend for NeverCalledModel {
    fn model_id(&self) -> &str {
        "initial-fence-model"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([4; 32])
    }
    async fn complete(
        &self,
        _: &[AgentMessage],
        _: &[ToolDefinition],
        _: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        panic!("Initial rejection cannot call a model");
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    sessions: Arc<SessionStore>,
    owner: String,
    manager: Arc<PersistentCommandManager>,
    actor: Arc<NativeManualCommandActor>,
    request: AgentRunRequest,
    queued: NativeCommandOperationRequest,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        let admission = sessions
            .admit_native_turn(
                &owner,
                "initial-fence-turn",
                "original admitted input",
                "initial-fence-model",
                TurnMode::Build,
            )
            .unwrap();
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.clone(),
                CommandIsolation::HostUnrestricted,
                Default::default(),
            )
            .unwrap()
            .with_native_session_owner(&owner)
            .unwrap(),
        );
        let command = manager
            .start(
                "/bin/sh",
                &["-c".into(), "exec cat > never-input".into()],
                &CancellationToken::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !workspace.join("never-input").exists() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let actor = Arc::new(
            NativeManualCommandActor::new(
                sessions.clone(),
                owner.clone(),
                Some(manager.clone()),
                Arc::new(|| {
                    Ok(NativeManualCommandPolicy {
                        permissions: PermissionPolicy::new(),
                        run_command_enabled: true,
                        isolation: CommandIsolation::HostUnrestricted,
                        hooks: Arc::new(CommandHooks::default()),
                    })
                }),
            )
            .unwrap(),
        );
        let queued = actor
            .prepare_request(
                &command.id,
                NativeCommandOperationAction::Stdin {
                    input: "never enter stdin\n".into(),
                    close_after: false,
                },
            )
            .unwrap();
        let request = AgentRunRequest {
            session_id: Some(owner.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.clone(),
            history: vec![],
            prompt: "original admitted input".into(),
            mode: TurnMode::Build,
        };
        Self {
            _root: root,
            workspace,
            sessions,
            owner,
            manager,
            actor,
            request,
            queued,
        }
    }
    fn runner(&self) -> AgentRunner {
        AgentRunner::new(Arc::new(NeverCalledModel), Arc::new(LocalTools::new()))
            .with_event_journal(self.sessions.clone())
            .with_native_runtime_identity("unit", "http://127.0.0.1:9/v1")
            .with_manual_command_actor(self.actor.clone())
    }
    fn sink(&self) -> (EventSink, mpsc::Receiver<AgentEvent>) {
        let (sender, receiver) = mpsc::sync_channel(64);
        (
            EventSink {
                session_id: self.owner.clone(),
                sequence: self.request.sequence_start,
                sender,
                journal: Some(self.sessions.clone()),
                journal_failed: false,
                journal_error: Arc::new(Mutex::new(None)),
                pending_stream_event: None,
            },
            receiver,
        )
    }
    fn no_effects(&self) {
        assert!(std::fs::read(self.workspace.join("never-input"))
            .unwrap()
            .is_empty());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.stop_all();
    }
}

/// Insert an actual ledger mutation at the append boundary, after the runner's
/// pending read. The wrapped SessionStore supplies the transactional fence;
/// the private helper's authority phase is separately checked against the real
/// store before installing this publication-only test seam.
struct EnqueueAtPublication {
    sessions: Arc<SessionStore>,
    actor: Arc<NativeManualCommandActor>,
    lease: Arc<NativeSessionLease>,
    request: NativeCommandOperationRequest,
    started: bool,
    inserted: AtomicBool,
}
impl NativeEventJournal for EnqueueAtPublication {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if !self.inserted.swap(true, Ordering::SeqCst) {
            self.actor.enqueue_confirmed(self.request.clone()).unwrap();
            if self.started {
                self.actor
                    .operations()
                    .preflight_capacity(&self.lease, &self.request.operation_id, 512 * 1024)
                    .unwrap();
                self.actor
                    .operations()
                    .begin_started(&self.lease, &self.request.operation_id, 0)
                    .unwrap();
            }
        }
        self.sessions.append_native_event(event)
    }
}

#[tokio::test]
async fn initial_rejection_publication_race_retires_requested_but_never_started() {
    for started in [false, true] {
        let fixture = Fixture::new().await;
        let runner = fixture.runner();
        let lease = Arc::new(
            fixture
                .sessions
                .try_acquire_native_lease(&fixture.owner)
                .unwrap(),
        );
        let (mut sink, receiver) = fixture.sink();
        runner.validate_manual_actor(Some(&lease), &sink).unwrap();
        runner
            .validate_initial_admission(
                &fixture.request,
                &fixture.workspace.canonicalize().unwrap(),
                Some(&lease),
                &sink,
            )
            .unwrap();
        let original = fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events;
        sink.journal = Some(Arc::new(EnqueueAtPublication {
            sessions: fixture.sessions.clone(),
            actor: fixture.actor.clone(),
            lease: lease.clone(),
            request: fixture.queued.clone(),
            started,
            inserted: AtomicBool::new(false),
        }));
        let terminal = AgentEventKind::TurnFailed {
            error: "invalid initial instruction snapshot".into(),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            runner.emit_worker_failure_boundary(
                Some(&lease),
                &CancellationToken::new(),
                &mut sink,
                terminal,
                &InitialRunValidation::InitialAuthorityVerified,
            ),
        )
        .await
        .expect("Started must release the worker, not wait for a manual resolution");
        let receipt = fixture
            .actor
            .operations()
            .snapshot(&fixture.owner, &fixture.queued.operation_id)
            .unwrap()
            .unwrap();
        fixture.no_effects();
        assert_eq!(receipt.request, fixture.queued);
        assert!(receipt.effect_result.is_none());
        if started {
            assert!(matches!(result, Err(AgentError::EffectUncertain(_))));
            assert!(receipt.state.requires_verification());
            assert_eq!(receipt.revision, 1);
            assert!(receiver.try_recv().is_err());
            assert_eq!(
                fixture
                    .sessions
                    .load(&fixture.owner)
                    .unwrap()
                    .unwrap()
                    .events,
                original
            );
        } else {
            result.unwrap();
            assert!(matches!(
                receipt.state,
                NativeCommandOperationState::Rejected { .. }
            ));
            assert!(receipt.phase_diagnostics().is_empty());
            assert!(matches!(
                receiver.try_recv().unwrap().kind,
                AgentEventKind::TurnFailed { .. }
            ));
            assert!(receiver.try_recv().is_err());
        }
        let reopened = Arc::new(SessionStore::open(fixture.sessions.storage_root()).unwrap());
        assert_eq!(
            openide_core::NativeCommandOperationStore::new(reopened.clone())
                .snapshot(&fixture.owner, &fixture.queued.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
        drop(sink);
        drop(lease);
        drop(reopened.try_acquire_native_lease(&fixture.owner).unwrap());
    }
}

#[tokio::test]
async fn initial_request_sink_and_lease_owner_mismatch_has_no_publication_or_intention_effect() {
    let fixture = Fixture::new().await;
    fixture
        .actor
        .enqueue_confirmed(fixture.queued.clone())
        .unwrap();
    let other = fixture
        .sessions
        .create(&fixture.workspace, "native")
        .unwrap();
    let admitted = fixture
        .sessions
        .admit_native_turn(
            &other.id,
            "other-turn",
            "other prompt",
            "initial-fence-model",
            TurnMode::Build,
        )
        .unwrap();
    let original = fixture
        .sessions
        .load(&fixture.owner)
        .unwrap()
        .unwrap()
        .events;
    let other_original = fixture.sessions.load(&other.id).unwrap().unwrap().events;
    let receipt = fixture
        .actor
        .operations()
        .snapshot(&fixture.owner, &fixture.queued.operation_id)
        .unwrap()
        .unwrap();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let request = AgentRunRequest {
        session_id: Some(other.id.clone()),
        sequence_start: admitted.sequence,
        prompt: "other prompt".into(),
        ..fixture.request.clone()
    };
    // A lease remains an authority boundary even without a manual actor or
    // steering store; the unrelated session has no receipt fence to save it.
    for with_actor in [true, false] {
        let mut runner = fixture.runner();
        if !with_actor {
            runner.manual_command_actor = None;
        }
        assert!(runner.native_steering_store.is_none());
        let (mut sink, receiver) = fixture.sink();
        sink.session_id = other.id.clone();
        sink.sequence = admitted.sequence;
        let mut phase = InitialRunValidation::default();
        let error = runner
            .run_inner(
                request.clone(),
                CancellationToken::new(),
                Arc::new(PendingPermissions::default()),
                &mut sink,
                Some(&lease),
                &mut phase,
            )
            .await
            .unwrap_err();
        assert!(phase == InitialRunValidation::UnverifiedAuthority);
        assert!(runner
            .emit_worker_failure_boundary(
                Some(&lease),
                &CancellationToken::new(),
                &mut sink,
                AgentEventKind::TurnFailed {
                    error: error.to_string()
                },
                &phase
            )
            .await
            .is_err());
        assert!(sink.journal_error.lock().unwrap().is_some());
        assert!(receiver.try_recv().is_err());
        fixture.no_effects();
        assert_eq!(
            fixture
                .sessions
                .load(&fixture.owner)
                .unwrap()
                .unwrap()
                .events,
            original
        );
        assert_eq!(
            fixture.sessions.load(&other.id).unwrap().unwrap().events,
            other_original
        );
        assert_eq!(
            fixture
                .actor
                .operations()
                .snapshot(&fixture.owner, &fixture.queued.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
    }
}
