//! Durable publication of semantic checkpoint candidates. A journal marker
//! is written only after its content-addressed file is fully published.

use crate::{
    AgentError, CommittedMessage, CompactLimits, CompactRequest, CompactService, ModelBackend,
};
use openide_core::{
    replay_native, AgentEvent, AgentEventKind, Checkpoint, CheckpointBudgetSnapshot,
    CheckpointCompatibility, CheckpointError, CheckpointFileStore, CheckpointReference,
    NativeReplay, NativeSessionLease, ReplayError, SessionStore, StoreError,
};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CheckpointCoordinatorError {
    #[error("session store failed: {0}")]
    Store(#[from] StoreError),
    #[error("native replay failed: {0}")]
    Replay(#[from] ReplayError),
    #[error("compaction failed: {0}")]
    Compact(#[from] AgentError),
    #[error("checkpoint candidate failed: {0}")]
    Checkpoint(#[from] CheckpointError),
    #[error("native session is missing")]
    SessionMissing,
    #[error("checkpointing requires a native session")]
    NonNativeSession,
    #[error("the interrupted turn must be resolved before checkpointing")]
    UnresolvedTurn,
    #[error("there is no new committed model history to checkpoint")]
    NoNewHistory,
    #[error("event sequence exhausted")]
    SequenceExhausted,
    #[error("checkpoint marker was not verifiable after journal append")]
    VerificationFailed,
}

pub struct CheckpointCommit {
    pub checkpoint: Checkpoint,
    pub reference: CheckpointReference,
    pub marker_event: AgentEvent,
    pub replay: NativeReplay,
}

/// The caller holds a native session lease across this entire async method.
/// A failure before marker append leaves a harmless candidate file that
/// replay ignores. A marker append uses SessionStore's synchronous journal.
pub struct CheckpointCoordinator {
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    compactor: CompactService,
    compatibility: CheckpointCompatibility,
    budget: CheckpointBudgetSnapshot,
}

impl CheckpointCoordinator {
    pub fn new(
        sessions: Arc<SessionStore>,
        checkpoints: Arc<CheckpointFileStore>,
        model: Arc<dyn ModelBackend>,
        compatibility: CheckpointCompatibility,
        budget: CheckpointBudgetSnapshot,
    ) -> Self {
        Self {
            sessions,
            checkpoints,
            compactor: CompactService::new(model),
            compatibility,
            budget,
        }
    }

    /// See `CompactService::with_foreign_summarizer`.
    pub fn with_foreign_summarizer(mut self) -> Self {
        self.compactor = self.compactor.with_foreign_summarizer();
        self
    }

    pub fn with_compact_limits(mut self, limits: CompactLimits) -> Self {
        self.compactor = self.compactor.with_limits(limits);
        self
    }

    pub async fn compact(
        &self,
        lease: &NativeSessionLease,
        cancel: CancellationToken,
    ) -> Result<CheckpointCommit, CheckpointCoordinatorError> {
        self.compact_inner(lease, cancel, false).await
    }

    /// Compact the last completed-turn prefix while a later turn is still
    /// running. The active prompt, committed model response, and complete or
    /// pending tool batch remain beyond the checkpoint's covered sequence and
    /// are replayed verbatim after its summary.
    pub async fn compact_completed_prefix(
        &self,
        lease: &NativeSessionLease,
        cancel: CancellationToken,
    ) -> Result<CheckpointCommit, CheckpointCoordinatorError> {
        self.compact_inner(lease, cancel, true).await
    }

    async fn compact_inner(
        &self,
        lease: &NativeSessionLease,
        cancel: CancellationToken,
        allow_active_turn: bool,
    ) -> Result<CheckpointCommit, CheckpointCoordinatorError> {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        let session_id = lease.session_id();
        let record = self
            .sessions
            .load(session_id)?
            .ok_or(CheckpointCoordinatorError::SessionMissing)?;
        if record.session.backend != "native" {
            return Err(CheckpointCoordinatorError::NonNativeSession);
        }
        let replay = replay_native(
            session_id,
            &record.events,
            Some(&self.checkpoints),
            &self.compatibility,
        )?;
        let covered_event_sequence = if replay.can_start_new_turn {
            replay.last_sequence
        } else if allow_active_turn
            && replay
                .active_turn
                .as_ref()
                .is_some_and(|turn| !turn.terminated)
        {
            let completed_sequence = record.events.iter().rev().find_map(|event| {
                matches!(event.kind, AgentEventKind::TurnCompleted { .. }).then_some(event.sequence)
            });
            let Some(completed_sequence) = completed_sequence else {
                return Err(CheckpointCoordinatorError::NoNewHistory);
            };
            let prefix_length = usize::try_from(completed_sequence)
                .map_err(|_| CheckpointCoordinatorError::SequenceExhausted)?;
            let prefix = replay_native(
                session_id,
                &record.events[..prefix_length],
                Some(&self.checkpoints),
                &self.compatibility,
            )?;
            if !prefix.can_start_new_turn {
                return Err(CheckpointCoordinatorError::UnresolvedTurn);
            }
            completed_sequence
        } else {
            return Err(CheckpointCoordinatorError::UnresolvedTurn);
        };
        if replay
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.covered_event_sequence >= covered_event_sequence)
        {
            return Err(CheckpointCoordinatorError::NoNewHistory);
        }
        let committed_prefix = replay
            .committed_messages
            .iter()
            .filter(|entry| entry.event_sequence <= covered_event_sequence)
            .cloned()
            .collect::<Vec<_>>();
        if committed_prefix.is_empty() {
            return Err(CheckpointCoordinatorError::NoNewHistory);
        }
        let untouched_suffix = replay
            .committed_messages
            .iter()
            .filter(|entry| entry.event_sequence > covered_event_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let marker_sequence = replay
            .last_sequence
            .checked_add(1)
            .ok_or(CheckpointCoordinatorError::SequenceExhausted)?;
        let request = CompactRequest {
            session_id: session_id.to_owned(),
            checkpoint_id: Uuid::new_v4().to_string(),
            covered_event_sequence,
            compatibility: self.compatibility.clone(),
            prior_checkpoint: replay.checkpoint.clone(),
            committed_messages: committed_prefix
                .into_iter()
                .map(|entry| CommittedMessage {
                    event_sequence: entry.event_sequence,
                    message: entry.message,
                })
                .collect(),
            budget: self.budget.clone(),
        };
        let checkpoint = self.compactor.compact(request, cancel.clone()).await?;
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        #[cfg(test)]
        tests::process_cut("summary_returned");
        let reference = self.checkpoints.write_candidate(&checkpoint)?;
        #[cfg(test)]
        tests::process_cut("candidate_published");
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled.into());
        }
        let marker_event = AgentEvent::new(
            session_id,
            marker_sequence,
            AgentEventKind::CheckpointCommitted {
                reference: reference.clone(),
            },
        );
        self.sessions.append_native_event(&marker_event)?;
        #[cfg(test)]
        tests::process_cut("marker_synced");

        let verified = self
            .sessions
            .load(session_id)?
            .ok_or(CheckpointCoordinatorError::VerificationFailed)?;
        let verified_replay = replay_native(
            session_id,
            &verified.events,
            Some(&self.checkpoints),
            &self.compatibility,
        )?;
        if verified_replay.last_sequence != marker_sequence
            || verified_replay.checkpoint.as_ref() != Some(&checkpoint)
            || verified_replay.active_turn != replay.active_turn
            || verified_replay.recovery != replay.recovery
            || verified_replay.pending_tool_batch != replay.pending_tool_batch
            || verified_replay.active_tool_catalog_digest != replay.active_tool_catalog_digest
            || verified_replay.active_model_context_digest != replay.active_model_context_digest
            || verified_replay.active_model_context_version != replay.active_model_context_version
            || verified_replay.committed_messages != untouched_suffix
        {
            return Err(CheckpointCoordinatorError::VerificationFailed);
        }
        Ok(CheckpointCommit {
            checkpoint,
            reference,
            marker_event,
            replay: verified_replay,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelOutput, ToolDefinition};
    use async_trait::async_trait;
    use openide_core::{AgentMessage, ModelTokenUsage, ReplayRecovery, ToolCall, TurnMode};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) fn process_cut(point: &str) {
        if std::env::var("OPENIDE_CHECKPOINT_CUT_POINT")
            .ok()
            .as_deref()
            == Some(point)
        {
            std::process::exit(86);
        }
    }

    #[test]
    #[ignore = "subprocess fixture for checkpoint publication interruption"]
    fn checkpoint_process_cut_child() {
        let root = std::path::PathBuf::from(std::env::var("OPENIDE_CHECKPOINT_CUT_ROOT").unwrap());
        let session_id = std::env::var("OPENIDE_CHECKPOINT_CUT_SESSION").unwrap();
        let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
        let checkpoints =
            Arc::new(CheckpointFileStore::new(root.join("checkpoint-store")).unwrap());
        let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
        let coordinator = CheckpointCoordinator::new(
            sessions,
            checkpoints,
            model("gpt-example", &session_id, None),
            compatibility("gpt-example"),
            budget(),
        );
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(coordinator.compact(&lease, CancellationToken::new()))
            .unwrap();
        panic!("checkpoint child did not reach its interruption point");
    }

    #[test]
    fn checkpoint_publication_process_cuts_keep_last_committed_history_after_reopen() {
        for point in ["summary_returned", "candidate_published", "marker_synced"] {
            let root = tempfile::tempdir().unwrap();
            let sessions_root = root.path().join("sessions");
            let checkpoints_root = root.path().join("checkpoint-store");
            let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
            let session = sessions.create(root.path(), "native").unwrap();
            let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            append_completed_turn(&sessions, &session.id, "first", "First request");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let coordinator = CheckpointCoordinator::new(
                sessions.clone(),
                checkpoints.clone(),
                model("gpt-example", &session.id, None),
                compatibility("gpt-example"),
                budget(),
            );
            let first = runtime
                .block_on(coordinator.compact(&lease, CancellationToken::new()))
                .unwrap();
            append_completed_turn(&sessions, &session.id, "second", "Second request");
            let covered_sequence = sessions
                .load(&session.id)
                .unwrap()
                .unwrap()
                .events
                .last()
                .unwrap()
                .sequence;
            drop(coordinator);
            drop(lease);
            drop(sessions);
            drop(checkpoints);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "checkpoint_coordinator::tests::checkpoint_process_cut_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("OPENIDE_CHECKPOINT_CUT_ROOT", root.path())
                .env("OPENIDE_CHECKPOINT_CUT_SESSION", &session.id)
                .env("OPENIDE_CHECKPOINT_CUT_POINT", point)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("checkpoint child timeout at {point}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert_eq!(status.code(), Some(86), "checkpoint cut at {point}");
            let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
            let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            let record = sessions.load(&session.id).unwrap().unwrap();
            let replay = replay_native(
                &session.id,
                &record.events,
                Some(&checkpoints),
                &compatibility("gpt-example"),
            )
            .unwrap();
            assert!(replay.can_start_new_turn);
            let marker_count = record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. }))
                .count();
            let candidate_count =
                std::fs::read_dir(checkpoints_root.join("checkpoints").join(&session.id))
                    .unwrap()
                    .count();
            if point == "marker_synced" {
                assert_eq!(marker_count, 2);
                assert_eq!(candidate_count, 2);
                let checkpoint = replay.checkpoint.unwrap();
                assert_eq!(checkpoint.covered_event_sequence, covered_sequence);
                assert_ne!(checkpoint.checkpoint_id, first.checkpoint.checkpoint_id);
                assert!(replay.committed_messages.is_empty());
            } else {
                assert_eq!(marker_count, 1);
                assert_eq!(
                    candidate_count,
                    if point == "candidate_published" { 2 } else { 1 }
                );
                assert_eq!(replay.checkpoint, Some(first.checkpoint));
                assert_eq!(
                    replay.history.last(),
                    Some(&AgentMessage::assistant("Done", vec![]))
                );
                assert!(replay
                    .history
                    .iter()
                    .any(|message| message.content == "Second request"));
                // An unreferenced candidate is ignored, and retry publishes a
                // fresh checkpoint from the preserved history, not its orphan.
                let coordinator = CheckpointCoordinator::new(
                    sessions,
                    checkpoints,
                    model("gpt-example", &session.id, None),
                    compatibility("gpt-example"),
                    budget(),
                );
                let retried = runtime
                    .block_on(coordinator.compact(&lease, CancellationToken::new()))
                    .unwrap();
                assert_eq!(retried.checkpoint.covered_event_sequence, covered_sequence);
                assert!(retried.replay.can_start_new_turn);
                assert!(retried.replay.committed_messages.is_empty());
            }
        }
    }

    struct SummaryModel {
        model_id: String,
        calls: AtomicUsize,
        interfere: Option<Arc<SessionStore>>,
        session_id: String,
    }

    #[async_trait]
    impl ModelBackend for SummaryModel {
        fn model_id(&self) -> &str {
            &self.model_id
        }

        async fn complete(
            &self,
            _messages: &[AgentMessage],
            tools: &[ToolDefinition],
            _cancel: CancellationToken,
        ) -> Result<ModelOutput, AgentError> {
            assert!(tools.is_empty());
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(store) = &self.interfere {
                let record = store.load(&self.session_id).unwrap().unwrap();
                let next = record.events.last().unwrap().sequence + 1;
                store
                    .append_native_event(&AgentEvent::new(
                        &self.session_id,
                        next,
                        AgentEventKind::ProcessOutput {
                            stream: openide_core::OutputStream::Stdout,
                            text: "concurrent journal update".into(),
                        },
                    ))
                    .unwrap();
            }
            Ok(ModelOutput {
                content: json!({
                    "objective": "Build an owned Rust harness",
                    "decisions": ["Use the native Rust runner"],
                    "files": [{"path":"crates/openide-harness/src/runner.rs","state":"exists"}],
                    "current_state": "Durable checkpoint coordinator is under test",
                    "constraints": ["Preserve the custom UI"],
                    "remaining_work": ["Connect UI controls"]
                })
                .to_string(),
                tool_calls: vec![],
            })
        }
    }

    fn compatibility(model: &str) -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: "openai".into(),
            model: model.into(),
            context_format_key: "responses-v1".into(),
        }
    }

    fn budget() -> CheckpointBudgetSnapshot {
        CheckpointBudgetSnapshot {
            input_limit_tokens: None,
            context_window_tokens: 128_000,
            estimated_input_tokens: 50_000,
            observed_input_tokens: Some(14_000),
            output_reserve_tokens: 4_096,
            tool_schema_reserve_tokens: 1_024,
            limits_unverified: false,
        }
    }

    fn model(
        model_id: &str,
        session_id: &str,
        interfere: Option<Arc<SessionStore>>,
    ) -> Arc<SummaryModel> {
        Arc::new(SummaryModel {
            model_id: model_id.into(),
            calls: AtomicUsize::new(0),
            interfere,
            session_id: session_id.into(),
        })
    }

    fn append_completed_turn(store: &SessionStore, session_id: &str, turn_id: &str, prompt: &str) {
        let admission = store
            .admit_native_turn(session_id, turn_id, prompt, "gpt-example", TurnMode::Build)
            .unwrap();
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: prompt.into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("Done", vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: "Done".into(),
            },
            AgentEventKind::TurnCompleted {
                response: "Done".into(),
                steps: 1,
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(session_id, sequence, kind))
                .unwrap();
        }
    }

    fn append_active_tool_turn(store: &SessionStore, session_id: &str) -> ToolCall {
        let call = ToolCall {
            id: "call-second".into(),
            name: "read_file".into(),
            arguments: json!({"path":"src/main.rs"}),
        };
        let admission = store
            .admit_native_turn(
                session_id,
                "turn-2",
                "Second request",
                "gpt-example",
                TurnMode::Build,
            )
            .unwrap();
        let mut assistant = AgentMessage::assistant("Working", vec![call.clone()]);
        assistant.provider_output_items = vec![json!({"type":"reasoning","id":"reason-1"})];
        let mut sequence = admission.sequence;
        for kind in [
            AgentEventKind::TurnStarted {
                prompt: "Second request".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: "a".repeat(64),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: "gpt-example".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant,
                response_id: Some("response-active".into()),
                usage: Some(ModelTokenUsage {
                    input_tokens: 1_234,
                    output_tokens: 56,
                    total_tokens: 1_290,
                    reasoning_tokens: Some(20),
                    cached_input_tokens: Some(100),
                }),
            },
            AgentEventKind::AssistantMessage {
                content: "Working".into(),
            },
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: "confirmed read".into(),
                is_error: false,
            },
        ] {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(session_id, sequence, kind))
                .unwrap();
        }
        call
    }

    #[tokio::test]
    async fn candidate_orphan_is_ignored_and_marker_is_verified() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let orphan = Checkpoint {
            session_id: session.id.clone(),
            checkpoint_id: "orphan".into(),
            covered_event_sequence: 6,
            compatibility: compatibility("gpt-example"),
            summary: "orphan summary".into(),
            retained_user_messages: vec![],
            budget: budget(),
        };
        checkpoints.write_candidate(&orphan).unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let backend = model("gpt-example", &session.id, None);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            backend.clone(),
            compatibility("gpt-example"),
            budget(),
        );
        let result = coordinator
            .compact(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_ne!(result.reference.checkpoint_id, "orphan");
        assert_eq!(result.marker_event.sequence, 7);
        assert_eq!(result.replay.checkpoint, Some(result.checkpoint.clone()));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            coordinator.compact(&lease, CancellationToken::new()).await,
            Err(CheckpointCoordinatorError::NoNewHistory)
        ));
    }

    #[tokio::test]
    async fn marker_append_failure_leaves_only_an_orphan_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let backend = model("gpt-example", &session.id, Some(sessions.clone()));
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            backend,
            compatibility("gpt-example"),
            budget(),
        );
        assert!(matches!(
            coordinator.compact(&lease, CancellationToken::new()).await,
            Err(CheckpointCoordinatorError::Store(_))
        ));
        let record = sessions.load(&session.id).unwrap().unwrap();
        assert!(!record
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })));
        let replay = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility("gpt-example"),
        )
        .unwrap();
        assert!(replay.checkpoint.is_none());
        let candidates = std::fs::read_dir(dir.path().join("checkpoints").join(&session.id))
            .unwrap()
            .count();
        assert_eq!(candidates, 1);
    }

    #[tokio::test]
    async fn active_turn_keeps_prompt_and_closed_tool_batch_after_prefix_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        let call = append_active_tool_turn(&sessions, &session.id);
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let backend = model("gpt-example", &session.id, None);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            backend.clone(),
            compatibility("gpt-example"),
            budget(),
        );
        let committed = coordinator
            .compact_completed_prefix(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(committed.checkpoint.covered_event_sequence, 6);
        assert_eq!(committed.marker_event.sequence, 15);
        assert_eq!(
            committed.replay.active_tool_catalog_digest.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(
            committed.replay.history[2],
            AgentMessage::user("Second request")
        );
        assert_eq!(committed.replay.history[3].tool_calls, vec![call]);
        assert_eq!(
            committed.replay.history[3].provider_output_items,
            vec![json!({"type":"reasoning","id":"reason-1"})]
        );
        assert_eq!(
            committed.replay.history[4],
            AgentMessage::tool("call-second", "confirmed read")
        );
        assert_eq!(
            committed.replay.latest_response_id.as_deref(),
            Some("response-active")
        );
        assert_eq!(committed.replay.latest_usage.unwrap().input_tokens, 1_234);
        assert_eq!(
            committed.replay.recovery,
            ReplayRecovery::ReadyForModel { next_step: 2 }
        );
        assert!(matches!(
            coordinator
                .compact_completed_prefix(&lease, CancellationToken::new())
                .await,
            Err(CheckpointCoordinatorError::NoNewHistory)
        ));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

        let mut sequence = committed.marker_event.sequence;
        for kind in [
            AgentEventKind::ModelRequestStarted {
                step: 2,
                model: "gpt-example".into(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 2,
                assistant: AgentMessage::assistant("Finished", vec![]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::AssistantMessage {
                content: "Finished".into(),
            },
            AgentEventKind::TurnCompleted {
                response: "Finished".into(),
                steps: 2,
            },
        ] {
            sequence += 1;
            sessions
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        }
        let record = sessions.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility("gpt-example"),
        )
        .unwrap();
        assert!(replay.can_start_new_turn);
        assert_eq!(
            replay.history.last(),
            Some(&AgentMessage::assistant("Finished", vec![]))
        );
        assert_eq!(replay.checkpoint, Some(committed.checkpoint));
    }

    #[tokio::test]
    async fn failed_marker_append_does_not_replace_active_turn_history() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        append_active_tool_turn(&sessions, &session.id);
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            model("gpt-example", &session.id, Some(sessions.clone())),
            compatibility("gpt-example"),
            budget(),
        );
        assert!(matches!(
            coordinator
                .compact_completed_prefix(&lease, CancellationToken::new())
                .await,
            Err(CheckpointCoordinatorError::Store(_))
        ));
        let record = sessions.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility("gpt-example"),
        )
        .unwrap();
        assert!(replay.checkpoint.is_none());
        assert_eq!(
            replay.recovery,
            ReplayRecovery::ReadyForModel { next_step: 2 }
        );
        assert_eq!(replay.history[0], AgentMessage::user("First request"));
        assert_eq!(replay.history[2], AgentMessage::user("Second request"));
        assert!(!record
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::CheckpointCommitted { .. })));
        assert_eq!(
            std::fs::read_dir(dir.path().join("checkpoints").join(&session.id))
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn successive_compactions_include_only_new_messages_after_prior_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let backend = model("gpt-example", &session.id, None);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints,
            backend.clone(),
            compatibility("gpt-example"),
            budget(),
        );
        let first = coordinator
            .compact(&lease, CancellationToken::new())
            .await
            .unwrap();
        append_completed_turn(&sessions, &session.id, "turn-2", "Second request");
        let second = coordinator
            .compact(&lease, CancellationToken::new())
            .await
            .unwrap();
        assert!(second.checkpoint.covered_event_sequence > first.checkpoint.covered_event_sequence);
        assert_eq!(second.checkpoint.retained_user_messages.len(), 2);
        assert_eq!(
            second.checkpoint.retained_user_messages[1].content,
            "Second request"
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
        assert_eq!(second.replay.checkpoint, Some(second.checkpoint));
    }

    #[tokio::test]
    async fn incompatible_checkpoint_is_rejected_before_calling_model() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        append_completed_turn(&sessions, &session.id, "turn-1", "First request");
        let checkpoints = Arc::new(CheckpointFileStore::new(dir.path()).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let first = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            model("gpt-example", &session.id, None),
            compatibility("gpt-example"),
            budget(),
        );
        first
            .compact(&lease, CancellationToken::new())
            .await
            .unwrap();
        let other_model = model("another-model", &session.id, None);
        let incompatible = CheckpointCoordinator::new(
            sessions,
            checkpoints,
            other_model.clone(),
            compatibility("another-model"),
            budget(),
        );
        assert!(matches!(
            incompatible.compact(&lease, CancellationToken::new()).await,
            Err(CheckpointCoordinatorError::Replay(
                ReplayError::IncompatibleCheckpoint
            ))
        ));
        assert_eq!(other_model.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unresolved_admission_is_not_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(dir.path()).unwrap());
        let session = sessions.create(dir.path(), "native").unwrap();
        sessions
            .admit_native_turn(
                &session.id,
                "turn-1",
                "Waiting to start",
                "gpt-example",
                TurnMode::Build,
            )
            .unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let backend = model("gpt-example", &session.id, None);
        let coordinator = CheckpointCoordinator::new(
            sessions,
            Arc::new(CheckpointFileStore::new(dir.path()).unwrap()),
            backend.clone(),
            compatibility("gpt-example"),
            budget(),
        );
        assert!(matches!(
            coordinator.compact(&lease, CancellationToken::new()).await,
            Err(CheckpointCoordinatorError::UnresolvedTurn)
        ));
        assert!(matches!(
            coordinator
                .compact_completed_prefix(&lease, CancellationToken::new())
                .await,
            Err(CheckpointCoordinatorError::NoNewHistory)
        ));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
}
