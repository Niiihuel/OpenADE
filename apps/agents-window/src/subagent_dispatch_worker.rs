//! Dispatch a matching initial reservation or a completed child's durable mailbox.
//! The coordinator owns claims; this host handle never creates a new spawn.

use crate::subagent_recovery_worker::SubagentRecoveryRequest;
use openide_harness::{SubagentCoordinator, SubagentLimits, SubagentStatus};
use std::sync::mpsc::{self, Receiver};
use tokio_util::sync::CancellationToken;

pub(crate) struct SubagentDispatchHandle {
    pub updates: Receiver<Result<(), String>>,
    cancellation: CancellationToken,
}

impl SubagentDispatchHandle {
    pub fn start(request: SubagentRecoveryRequest) -> Result<Self, String> {
        let coordinator =
            SubagentCoordinator::new(request.sessions.clone(), SubagentLimits::default())
                .map_err(|error| error.to_string())?;
        let lease = request
            .sessions
            .try_acquire_native_lease(&request.parent_id)
            .map_err(|error| format!("La sesión padre está en uso: {error}"))?;
        let candidate = coordinator
            .list_children_snapshot_for_ui(&request.parent_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|child| child.agent.session.id == request.child_id)
            .ok_or("El subagente ya no pertenece al padre")?;
        let initial = candidate.agent.status == SubagentStatus::Ready;
        if candidate.agent.cancellation_requested
            || (!initial
                && (candidate.agent.status != SubagentStatus::Completed
                    || candidate.open_inputs == 0))
        {
            return Err("La cola ya no está lista para despacho; revisá su recuperación".into());
        }
        if initial {
            request
                .worker
                .validate_initial_child(
                    &request.child_id,
                    &request.spawn_id,
                    &request.prompt,
                    request.mode,
                )
                .map_err(|error| error.to_string())?;
        } else {
            request
                .worker
                .validate_follow_up_child(&request.child_id)
                .map_err(|error| error.to_string())?;
        }
        let control = coordinator
            .issue_control(&lease)
            .map_err(|error| error.to_string())?;
        drop(lease);
        let cancellation = CancellationToken::new();
        let worker_cancel = cancellation.clone();
        let (sender, updates) = mpsc::channel();
        std::thread::Builder::new()
            .name("openide-child-mailbox".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        if initial {
                            runtime
                                .block_on(request.worker.run_prepared(
                                    control,
                                    request.child_id,
                                    request.spawn_id,
                                    request.prompt,
                                    request.mode,
                                    worker_cancel,
                                ))
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        } else {
                            runtime
                                .block_on(request.worker.drain_queued_prepared(
                                    control,
                                    request.child_id,
                                    worker_cancel,
                                ))
                                .map_err(|error| error.to_string())
                        }
                    });
                let _ = sender.send(result);
            })
            .map_err(|error| format!("No se pudo iniciar el despacho del subagente: {error}"))?;
        Ok(Self {
            updates,
            cancellation,
        })
    }
}

impl Drop for SubagentDispatchHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use openide_core::{AgentEventKind, AgentMessage, SessionStore, TurnMode};
    use openide_harness::{
        AgentError, LocalTools, ModelBackend, ModelOutput, SubagentWorker, ToolDefinition,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};

    struct GatedModel {
        requests: AtomicUsize,
        gate: tokio::sync::Semaphore,
    }

    #[async_trait]
    impl ModelBackend for GatedModel {
        fn model_id(&self) -> &str {
            "dispatch-fixture"
        }
        fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
            Some([17; 32])
        }
        async fn complete(
            &self,
            _messages: &[AgentMessage],
            _tools: &[ToolDefinition],
            cancellation: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            if self.requests.fetch_add(1, Ordering::SeqCst) > 0 {
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                    permit = self.gate.acquire() => { permit.unwrap().forget(); }
                }
            }
            Ok(ModelOutput {
                content: "done".into(),
                tool_calls: vec![],
            })
        }
    }

    #[tokio::test]
    async fn initial_ready_dispatch_reopens_validates_scope_and_claims_one_worker() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        // Gate the first call too, so both dispatchers contend while it runs.
        let model = Arc::new(GatedModel {
            requests: AtomicUsize::new(1),
            gate: tokio::sync::Semaphore::new(0),
        });
        let worker = SubagentWorker::new(
            coordinator,
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        )
        .with_system_instructions("Frozen initial scope");
        let lease = sessions.try_acquire_native_lease(&parent.id).unwrap();
        let child = worker
            .prepare_child(&lease, "initial-spawn", "Initial task", TurnMode::Build)
            .unwrap();
        drop(lease);
        let child_id = child.session.id;
        let make_request = |prompt: &str| SubagentRecoveryRequest {
            sessions: Arc::new(SessionStore::open(sessions.storage_root()).unwrap()),
            worker: worker.clone(),
            parent_id: parent.id.clone(),
            child_id: child_id.clone(),
            spawn_id: "initial-spawn".into(),
            prompt: prompt.into(),
            mode: TurnMode::Build,
        };
        let before = sessions.load(&child_id).unwrap().unwrap().events;
        assert!(SubagentDispatchHandle::start(make_request("Changed task")).is_err());
        let mut wrong_mode = make_request("Initial task");
        wrong_mode.mode = TurnMode::Plan;
        assert!(SubagentDispatchHandle::start(wrong_mode).is_err());
        let mut wrong_spawn = make_request("Initial task");
        wrong_spawn.spawn_id = "different-spawn".into();
        assert!(SubagentDispatchHandle::start(wrong_spawn).is_err());
        assert_eq!(sessions.load(&child_id).unwrap().unwrap().events, before);
        assert_eq!(model.requests.load(Ordering::SeqCst), 1);
        let barrier = std::sync::Barrier::new(2);
        let attempts = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                SubagentDispatchHandle::start(make_request("Initial task"))
            });
            let second = scope.spawn(|| {
                barrier.wait();
                SubagentDispatchHandle::start(make_request("Initial task"))
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        let handles: Vec<_> = attempts.into_iter().filter_map(Result::ok).collect();
        assert!(!handles.is_empty());
        let deadline = Instant::now() + Duration::from_secs(10);
        while model.requests.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        model.gate.add_permits(1);
        for handle in handles {
            let result = handle.updates.recv_timeout(Duration::from_secs(5)).unwrap();
            if let Err(error) = result {
                // Either worker can lose the claim. State-transition errors
                // include the enum name `Running`; lease errors use prose.
                let normalized = error.to_ascii_lowercase();
                assert!(
                    normalized.contains("running")
                        || normalized.contains("already")
                        || normalized.contains("lease"),
                    "{error}"
                );
            }
        }
        assert_eq!(model.requests.load(Ordering::SeqCst), 2);
        let reopened = Arc::new(SessionStore::open(sessions.storage_root()).unwrap());
        let record = reopened.load(&child_id).unwrap().unwrap();
        assert_eq!(record.events.iter().filter(|event| matches!(&event.kind, AgentEventKind::TurnAdmitted { turn_id, prompt, .. } if turn_id == "initial-spawn" && prompt == "Initial task")).count(), 1);
        assert_eq!(
            record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
                .count(),
            1
        );
        let coordinator =
            SubagentCoordinator::new(reopened.clone(), SubagentLimits::default()).unwrap();
        assert_eq!(
            coordinator
                .list_children_snapshot_for_ui(&parent.id)
                .unwrap()[0]
                .agent
                .status,
            SubagentStatus::Completed
        );
        let _released = reopened.try_acquire_native_lease(&child_id).unwrap();
    }

    #[tokio::test]
    async fn competing_dispatchers_claim_one_reopened_input_and_keep_one_model_effect() {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let parent = sessions.create(root.path(), "native").unwrap();
        let coordinator = Arc::new(
            SubagentCoordinator::new(sessions.clone(), SubagentLimits::default()).unwrap(),
        );
        let model = Arc::new(GatedModel {
            requests: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(0),
        });
        let worker = SubagentWorker::new(
            coordinator,
            sessions.clone(),
            model.clone(),
            Arc::new(LocalTools::new()),
            "fixture",
            "https://fixture.example/v1",
        );
        let parent_lease = Arc::new(sessions.try_acquire_native_lease(&parent.id).unwrap());
        let initial = worker
            .run(
                parent_lease.clone(),
                "spawn-dispatch-race".into(),
                "first".into(),
                TurnMode::Build,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        drop(parent_lease);
        let path = sessions.storage_root().join("subagents.registry.json");
        let mut registry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // A reopened mailbox fixture; original journal/worker identity comes
        // from real harness execution above.
        registry["inputs"][&initial.child_session_id] = serde_json::json!([{"id":"race-input", "prompt":"second", "mode":"build", "digest":"1c000c8b21d542902173f501e78f174932b845c630aad69ed6a58131df19aaff", "state":"pending", "revision":0}]);
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        let make_request = || SubagentRecoveryRequest {
            sessions: Arc::new(SessionStore::open(sessions.storage_root()).unwrap()),
            worker: worker.clone(),
            parent_id: parent.id.clone(),
            child_id: initial.child_session_id.clone(),
            spawn_id: "spawn-dispatch-race".into(),
            prompt: "first".into(),
            mode: TurnMode::Build,
        };
        let barrier = std::sync::Barrier::new(2);
        let attempts = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                SubagentDispatchHandle::start(make_request())
            });
            let second = scope.spawn(|| {
                barrier.wait();
                SubagentDispatchHandle::start(make_request())
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        let mut handles = Vec::new();
        for attempt in attempts {
            match attempt {
                Ok(handle) => handles.push(handle),
                Err(error) => assert!(
                    error.contains("en uso") || error.contains("ya no está lista"),
                    "{error}"
                ),
            }
        }
        assert!(!handles.is_empty());
        let deadline = Instant::now() + Duration::from_secs(10);
        while model.requests.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(model.requests.load(Ordering::SeqCst), 2);
        model.gate.add_permits(1);
        let mut done = vec![false; handles.len()];
        while done.iter().any(|done| !done) {
            for (index, handle) in handles.iter().enumerate() {
                if !done[index] {
                    if let Ok(result) = handle.updates.try_recv() {
                        result.unwrap();
                        done[index] = true;
                    }
                }
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(model.requests.load(Ordering::SeqCst), 2);
        let stored = sessions.load(&initial.child_session_id).unwrap().unwrap();
        assert_eq!(stored.events.iter().filter(|event| matches!(&event.kind, AgentEventKind::TurnAdmitted {turn_id,..} if turn_id=="race-input")).count(),1);
        let reopened = SubagentCoordinator::new(
            Arc::new(SessionStore::open(sessions.storage_root()).unwrap()),
            SubagentLimits::default(),
        )
        .unwrap();
        let snapshot = reopened.list_children_snapshot_for_ui(&parent.id).unwrap();
        assert_eq!(snapshot[0].agent.status, SubagentStatus::Completed);
        assert_eq!(snapshot[0].open_inputs, 0);
        assert_eq!(
            snapshot[0].inputs[0].status,
            openide_harness::SubagentInputStatus::Done
        );
    }
}
