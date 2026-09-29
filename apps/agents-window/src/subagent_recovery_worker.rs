//! Reopen one interrupted child without keeping the parent lease while a
//! human decides a pending permission. Every retry goes through the child
//! coordinator and the native replay engine in `SubagentWorker`.

use openide_core::{PermissionDecision, SessionStore, ToolCall, TurnMode};
use openide_harness::{
    PermissionChoice, SubagentRecoveryPause, SubagentRecoveryState, SubagentWorker,
};
use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc,
};
use tokio_util::sync::CancellationToken;

const MAX_PERMISSION_RETRIES: usize = 128;

pub(crate) struct SubagentRecoveryRequest {
    pub sessions: Arc<SessionStore>,
    pub worker: SubagentWorker,
    pub parent_id: String,
    pub child_id: String,
    pub spawn_id: String,
    pub prompt: String,
    pub mode: TurnMode,
}

#[derive(Debug)]
pub(crate) enum SubagentRecoveryUpdate {
    NeedsPermission {
        request_id: String,
        tool_call: ToolCall,
    },
    Finished(Result<SubagentRecoveryState, String>),
}

struct PermissionCommand {
    request_id: String,
    choice: PermissionChoice,
}

pub(crate) struct SubagentRecoveryHandle {
    pub child_id: String,
    pub parent_id: String,
    pub updates: Receiver<SubagentRecoveryUpdate>,
    permissions: tokio::sync::mpsc::UnboundedSender<PermissionCommand>,
    cancel: CancellationToken,
}

impl SubagentRecoveryHandle {
    pub fn start(request: SubagentRecoveryRequest) -> Result<Self, String> {
        let child_id = request.child_id.clone();
        let parent_id = request.parent_id.clone();
        let (updates_tx, updates) = mpsc::channel();
        let (permissions, commands) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("openide-child-resume".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run_recovery(
                        request,
                        commands,
                        &updates_tx,
                        worker_cancel,
                    )),
                    Err(error) => Err(format!("No se pudo iniciar Tokio: {error}")),
                };
                let _ = updates_tx.send(SubagentRecoveryUpdate::Finished(result));
            })
            .map_err(|error| {
                format!("No se pudo iniciar la recuperación del subagente: {error}")
            })?;
        Ok(Self {
            child_id,
            parent_id,
            updates,
            permissions,
            cancel,
        })
    }

    /// The UI must durably record this exact decision in the child's sidecar
    /// before sending it. The worker reads that sidecar before retrying.
    pub fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) -> bool {
        self.permissions
            .send(PermissionCommand {
                request_id: request_id.to_owned(),
                choice,
            })
            .is_ok()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for SubagentRecoveryHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn run_recovery(
    request: SubagentRecoveryRequest,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<PermissionCommand>,
    updates: &Sender<SubagentRecoveryUpdate>,
    cancel: CancellationToken,
) -> Result<SubagentRecoveryState, String> {
    for _ in 0..MAX_PERMISSION_RETRIES {
        if cancel.is_cancelled() {
            return Err("Recuperación del subagente cancelada".into());
        }
        let parent_lease = request
            .sessions
            .try_acquire_native_lease(&request.parent_id)
            .map_err(|error| format!("No se pudo bloquear la sesión padre: {error}"))?;
        let state = request
            .worker
            .resume(
                Arc::new(parent_lease),
                request.child_id.clone(),
                request.spawn_id.clone(),
                request.prompt.clone(),
                request.mode,
                cancel.clone(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let SubagentRecoveryState::Paused {
            reason:
                SubagentRecoveryPause::NeedsPermission {
                    request_id,
                    tool_call,
                },
            ..
        } = &state
        else {
            return Ok(state);
        };
        updates
            .send(SubagentRecoveryUpdate::NeedsPermission {
                request_id: request_id.clone(),
                tool_call: tool_call.clone(),
            })
            .map_err(|_| "La UI dejó de escuchar la recuperación del subagente".to_owned())?;
        let command = tokio::select! {
            _ = cancel.cancelled() => return Err("Recuperación del subagente cancelada".into()),
            command = commands.recv() => command.ok_or_else(|| "La UI cerró la decisión pendiente".to_owned())?,
        };
        if command.request_id != *request_id {
            return Err("El permiso del subagente cambió antes de la decisión".into());
        }
        if command.choice == PermissionChoice::AllowSession {
            let parent_lease = request
                .sessions
                .try_acquire_native_lease(&request.parent_id)
                .map_err(|error| format!("No se pudo bloquear la sesión padre: {error}"))?;
            request
                .worker
                .resolve_recovery_session_approval(
                    Arc::new(parent_lease),
                    &request.child_id,
                    &request.spawn_id,
                    &request.prompt,
                    request.mode,
                    &command.request_id,
                )
                .map_err(|error| error.to_string())?;
            continue;
        }
        let recorded = request
            .sessions
            .load_permission_decisions(&request.child_id)
            .map_err(|error| format!("No se pudo verificar el permiso registrado: {error}"))?
            .into_iter()
            .find(|decision| decision.request_id == command.request_id);
        let expected = match command.choice {
            PermissionChoice::Allow => PermissionDecision::Allow,
            PermissionChoice::Deny => PermissionDecision::Deny,
            PermissionChoice::AllowSession => unreachable!("handled above"),
        };
        if !recorded.is_some_and(|decision| decision.decision == expected) {
            return Err("El permiso del subagente no quedó registrado antes de reintentar".into());
        }
    }
    Err("La recuperación del subagente superó el límite de permisos".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use openide_core::{
        AgentEvent, AgentEventKind, AgentMessage, MessageRole, ToolCall,
        EXEC_POLICY_IDENTITY_VERSION, HOOK_CONFIG_IDENTITY_VERSION, MODEL_CONTEXT_VERSION,
    };
    use openide_harness::{
        native_model_context_digest, AgentError, CommandHooks, LocalTools, ModelBackend,
        ModelOutput, PermissionMode, PermissionPolicy, SubagentCoordinator, SubagentLimits,
        SubagentStatus, ToolCatalog, ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NeverModel;

    #[async_trait]
    impl ModelBackend for NeverModel {
        fn model_id(&self) -> &str {
            "test-model"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            panic!("the child must be rejected before a model request")
        }
    }

    struct RecoveryModel(AtomicUsize);

    #[async_trait]
    impl ModelBackend for RecoveryModel {
        fn model_id(&self) -> &str {
            "test-model"
        }

        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([7; 32])
        }

        async fn complete(
            &self,
            messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert!(messages
                .iter()
                .any(|message| message.role == MessageRole::Tool));
            Ok(ModelOutput {
                content: "Recovered".into(),
                tool_calls: Vec::new(),
            })
        }
    }

    struct CountingTool(AtomicUsize);

    #[async_trait]
    impl ToolExecutor for CountingTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "effect".into(),
                description: "Count one execution".into(),
                parameters: json!({"type":"object","properties":{}}),
            }]
        }

        async fn execute(
            &self,
            _call: &ToolCall,
            _context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::success("done"))
        }
    }

    fn append(sessions: &SessionStore, child_id: &str, kind: AgentEventKind) {
        let stored = sessions.load(child_id).unwrap().unwrap();
        let sequence = stored.events.last().map_or(1, |event| event.sequence + 1);
        sessions
            .append_native_event(&AgentEvent::new(child_id, sequence, kind))
            .unwrap();
    }

    fn pending_permission_child(
        root: &Path,
    ) -> (
        Arc<SessionStore>,
        SubagentWorker,
        String,
        String,
        Arc<RecoveryModel>,
        Arc<CountingTool>,
    ) {
        let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let parent = sessions.create(&workspace, "native").unwrap();
        let parent_lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let model = Arc::new(RecoveryModel(AtomicUsize::new(0)));
        let tools = Arc::new(CountingTool(AtomicUsize::new(0)));
        let policy = PermissionPolicy::new().with_tool("effect", PermissionMode::Ask);
        let policy_digest = policy.digest_hex();
        let worker = SubagentWorker::new(
            coordinator.clone(),
            sessions.clone(),
            model.clone(),
            tools.clone(),
            "custom",
            "http://localhost/never",
        )
        .with_permission_policy(policy);
        let child = worker
            .prepare_child(&parent_lease, "spawn-1", "Recover", TurnMode::Build)
            .unwrap();
        let child_id = child.session.id;
        sessions
            .admit_native_turn(
                &child_id,
                "spawn-1",
                "Recover",
                "test-model",
                TurnMode::Build,
            )
            .unwrap();
        let run_lease = coordinator.start_child(&parent_lease, &child_id).unwrap();
        let catalog = ToolCatalog::from_executor(tools.as_ref()).unwrap();
        for kind in [
            AgentEventKind::SessionStarted {
                workspace: workspace.clone(),
            },
            AgentEventKind::TurnStarted {
                prompt: "Recover".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: EXEC_POLICY_IDENTITY_VERSION,
                digest: policy_digest,
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: native_model_context_digest(
                    "custom",
                    "http://localhost/never",
                    "test-model",
                    TurnMode::Build,
                    &workspace,
                    &catalog,
                    "",
                    [7; 32],
                )
                .unwrap(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "test-model".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant(
                    "",
                    vec![ToolCall {
                        id: "effect-1".into(),
                        name: "effect".into(),
                        arguments: json!({}),
                    }],
                ),
                response_id: None,
                usage: None,
            },
            AgentEventKind::PermissionRequested {
                request_id: "permit-1".into(),
                tool_call: ToolCall {
                    id: "effect-1".into(),
                    name: "effect".into(),
                    arguments: json!({}),
                },
            },
        ] {
            append(&sessions, &child_id, kind);
        }
        drop(run_lease);
        drop(parent_lease);
        (sessions, worker, parent.id, child_id, model, tools)
    }

    #[test]
    fn rejects_unrelated_child_without_model_or_tool_effect() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let first = sessions.create(&workspace, "native").unwrap();
        let second = sessions.create(&workspace, "native").unwrap();
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let tools: Arc<dyn ToolExecutor> = Arc::new(LocalTools::new());
        let worker = SubagentWorker::new(
            coordinator,
            sessions.clone(),
            Arc::new(NeverModel),
            tools,
            "custom",
            "http://localhost/never",
        );
        let request = SubagentRecoveryRequest {
            sessions,
            worker,
            parent_id: first.id,
            child_id: second.id,
            spawn_id: "spawn-1".into(),
            prompt: "safe".into(),
            mode: TurnMode::Build,
        };
        let handle = SubagentRecoveryHandle::start(request).unwrap();
        let update = handle
            .updates
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(update, SubagentRecoveryUpdate::Finished(Err(_))));
    }

    #[test]
    fn replays_durable_permission_once_and_releases_parent_lease_while_waiting() {
        let root = tempfile::tempdir().unwrap();
        let (sessions, worker, parent_id, child_id, model, tools) =
            pending_permission_child(root.path());
        let request = SubagentRecoveryRequest {
            sessions: sessions.clone(),
            worker,
            parent_id: parent_id.clone(),
            child_id: child_id.clone(),
            spawn_id: "spawn-1".into(),
            prompt: "Recover".into(),
            mode: TurnMode::Build,
        };
        let handle = SubagentRecoveryHandle::start(request).unwrap();
        assert!(matches!(
            handle
                .updates
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            SubagentRecoveryUpdate::NeedsPermission { request_id, .. } if request_id == "permit-1"
        ));
        let parent_lease = sessions.try_acquire_native_lease(&parent_id).unwrap();
        drop(parent_lease);
        assert_eq!(model.0.load(Ordering::SeqCst), 0);
        assert_eq!(tools.0.load(Ordering::SeqCst), 0);
        sessions
            .record_permission_decision(&child_id, "permit-1", PermissionDecision::Allow)
            .unwrap();
        assert!(handle.resolve_permission("permit-1", PermissionChoice::Allow));
        assert!(matches!(
            handle
                .updates
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            SubagentRecoveryUpdate::Finished(Ok(SubagentRecoveryState::Terminal(
                openide_harness::SubagentTurnResult {
                    status: SubagentStatus::Completed,
                    ..
                }
            )))
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert_eq!(tools.0.load(Ordering::SeqCst), 1);
        let events = sessions.load(&child_id).unwrap().unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn recovery_can_save_child_session_approval_before_tool_execution() {
        let root = tempfile::tempdir().unwrap();
        let (sessions, worker, parent_id, child_id, model, tools) =
            pending_permission_child(root.path());
        let request = SubagentRecoveryRequest {
            sessions: sessions.clone(),
            worker: worker.with_session_approvals(openide_harness::CommandIsolation::HostUnrestricted),
            parent_id,
            child_id: child_id.clone(),
            spawn_id: "spawn-1".into(),
            prompt: "Recover".into(),
            mode: TurnMode::Build,
        };
        let handle = SubagentRecoveryHandle::start(request).unwrap();
        assert!(matches!(
            handle
                .updates
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            SubagentRecoveryUpdate::NeedsPermission { request_id, .. } if request_id == "permit-1"
        ));
        assert!(handle.resolve_permission("permit-1", PermissionChoice::AllowSession));
        assert!(matches!(
            handle
                .updates
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            SubagentRecoveryUpdate::Finished(Ok(SubagentRecoveryState::Terminal(
                openide_harness::SubagentTurnResult {
                    status: SubagentStatus::Completed,
                    ..
                }
            )))
        ));
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert_eq!(tools.0.load(Ordering::SeqCst), 1);
        let decisions = sessions.load_permission_decisions(&child_id).unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].grant.is_some());
    }
}
