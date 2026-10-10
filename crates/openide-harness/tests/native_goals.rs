use async_trait::async_trait;
use openide_core::{
    AgentEventKind, AgentMessage, AgentRunRequest, NativeGoalCriterion, NativeGoalError,
    NativeGoalEvidence, NativeGoalMutation, NativeGoalStatus, NativeGoalStore, SessionStore,
    TurnMode,
};
use openide_harness::{
    AgentError, AgentRunner, LocalTools, ModelBackend, ModelOutput, ToolDefinition,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
struct Fixture;
#[async_trait]
impl ModelBackend for Fixture {
    fn model_id(&self) -> &str {
        "goal-fixture"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([91; 32])
    }
    async fn complete(
        &self,
        _: &[AgentMessage],
        _: &[ToolDefinition],
        _: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        Ok(ModelOutput {
            content: "Verified fixture result".into(),
            tool_calls: vec![],
        })
    }
}
#[test]
fn native_goal_completion_requires_all_evidence_from_real_completed_turn_and_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let session = sessions.create(root.path(), "native").unwrap();
    let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
    let goals = NativeGoalStore::new(sessions.clone());
    goals
        .create(
            &lease,
            "goal",
            "Preserve the full objective",
            ["one", "two"]
                .into_iter()
                .map(|id| NativeGoalCriterion {
                    id: id.into(),
                    description: format!("Verify {id}"),
                    evidence: None,
                })
                .collect(),
        )
        .unwrap();
    assert!(matches!(
        goals.update(&lease, "goal", "premature", 0, NativeGoalMutation::Complete),
        Err(NativeGoalError::MissingEvidence)
    ));
    let evidence = NativeGoalEvidence {
        turn_id: "turn".into(),
        summary: "Fixture completed without effects".into(),
    };
    assert!(matches!(
        goals.update(
            &lease,
            "goal",
            "unknown",
            0,
            NativeGoalMutation::RecordEvidence {
                criterion_id: "one".into(),
                evidence: evidence.clone()
            }
        ),
        Err(NativeGoalError::UnverifiedTurn)
    ));
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "turn",
            "Verify fixture",
            "goal-fixture",
            TurnMode::Build,
        )
        .unwrap();
    let runner = AgentRunner::new(Arc::new(Fixture), Arc::new(LocalTools::new()))
        .with_event_journal(sessions.clone())
        .with_native_runtime_identity("fixture", "https://fixture.example/v1");
    let handle = runner.start_with_shared_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: root.path().into(),
            history: vec![],
            prompt: "Verify fixture".into(),
            mode: TurnMode::Build,
        },
        lease.clone(),
    );
    let mut completed = false;
    let mut observed = vec![];
    loop {
        match handle.events.recv_timeout(Duration::from_secs(10)) {
            Ok(event) => {
                completed |= matches!(event.kind, AgentEventKind::TurnCompleted { .. });
                observed.push(event);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("{error}"),
        }
    }
    assert!(completed, "{observed:?}");
    drop(handle);
    goals
        .update(
            &lease,
            "goal",
            "evidence-one",
            0,
            NativeGoalMutation::RecordEvidence {
                criterion_id: "one".into(),
                evidence: evidence.clone(),
            },
        )
        .unwrap();
    assert!(matches!(
        goals.update(
            &lease,
            "goal",
            "still-incomplete",
            1,
            NativeGoalMutation::Complete
        ),
        Err(NativeGoalError::MissingEvidence)
    ));
    goals
        .update(&lease, "goal", "pause", 1, NativeGoalMutation::PauseByUser)
        .unwrap();
    assert!(matches!(
        goals.update(
            &lease,
            "goal",
            "paused-close",
            2,
            NativeGoalMutation::Complete
        ),
        Err(NativeGoalError::Conflict)
    ));
    goals
        .update(
            &lease,
            "goal",
            "resume",
            2,
            NativeGoalMutation::ResumeByUser,
        )
        .unwrap();
    goals
        .update(
            &lease,
            "goal",
            "evidence-two",
            3,
            NativeGoalMutation::RecordEvidence {
                criterion_id: "two".into(),
                evidence,
            },
        )
        .unwrap();
    let done = goals
        .update(&lease, "goal", "complete", 4, NativeGoalMutation::Complete)
        .unwrap();
    assert_eq!(done.status, NativeGoalStatus::Complete);
    assert_eq!(
        goals
            .update(&lease, "goal", "complete", 4, NativeGoalMutation::Complete)
            .unwrap(),
        done
    );
    let journal = sessions.load(&session.id).unwrap().unwrap().events;
    drop(goals);
    drop(runner);
    drop(lease);
    drop(sessions);
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let goals = NativeGoalStore::new(sessions.clone());
    assert_eq!(goals.list(&session.id).unwrap(), vec![done]);
    assert_eq!(sessions.load(&session.id).unwrap().unwrap().events, journal);
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    goals
        .create(
            &lease,
            "next",
            "Next explicit goal",
            vec![NativeGoalCriterion {
                id: "next".into(),
                description: "Next criterion".into(),
                evidence: None,
            }],
        )
        .unwrap();
    assert_eq!(goals.list(&session.id).unwrap().len(), 2);
    let receipt = goals
        .committed_update(&lease, "goal", "complete", 4, &NativeGoalMutation::Complete)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.committed_revision, 5);
    assert_eq!(receipt.current_revision, 5);
    drop(lease);
    sessions.delete_native_tree(&session.id).unwrap();
    assert!(!sessions
        .storage_root()
        .join(format!("{}.native-goals.json", session.id))
        .exists());
}

struct GoalToolModel(std::sync::atomic::AtomicUsize);
#[async_trait]
impl ModelBackend for GoalToolModel {
    fn model_id(&self) -> &str {
        "goal-tools-fixture"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([92; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        _: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        use serde_json::json;
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            for name in ["get_goal", "record_goal_evidence", "complete_goal"] {
                assert!(tools.iter().any(|t| t.name == name));
            }
            assert!(!tools.iter().any(|t| matches!(
                t.name.as_str(),
                "pause_goal" | "resume_goal" | "create_goal"
            )));
            Ok(ModelOutput {
                content: String::new(),
                tool_calls: vec![
                    openide_core::ToolCall {
                        id: "read-goal".into(),
                        name: "get_goal".into(),
                        arguments: json!({}),
                    },
                    openide_core::ToolCall {
                        id: "record-evidence".into(),
                        name: "record_goal_evidence".into(),
                        arguments: json!({"goal_id":"goal","expected_revision":0,"criterion_id":"verify","turn_id":"seed","summary":"Local fixture completed"}),
                    },
                    openide_core::ToolCall {
                        id: "finish-goal".into(),
                        name: "complete_goal".into(),
                        arguments: json!({"goal_id":"goal","expected_revision":1}),
                    },
                ],
            })
        } else {
            assert!(messages
                .iter()
                .any(|m| m.content.contains("Full original objective")));
            assert!(messages
                .iter()
                .any(|m| m.content.contains("\"status\":\"complete\"")));
            Ok(ModelOutput {
                content: "Goal fixture finished".into(),
                tool_calls: vec![],
            })
        }
    }
}
#[test]
fn native_goal_tools_run_serially_in_real_turn_and_reopen_completed_criteria() {
    let root = tempfile::tempdir().unwrap();
    run_goal_tools(root.path(), None);
}
fn run_goal_tools(root: &std::path::Path, cut: Option<&str>) {
    use openide_harness::NativeGoalToolExecutor;
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let session = sessions.create(root, "native").unwrap();
    let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
    let goals = NativeGoalStore::new(sessions.clone());
    goals
        .create(
            &lease,
            "goal",
            "Full original objective",
            vec![NativeGoalCriterion {
                id: "verify".into(),
                description: "Verify fixture".into(),
                evidence: None,
            }],
        )
        .unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "seed",
            "Seed completed turn",
            "goal-fixture",
            TurnMode::Build,
        )
        .unwrap();
    let seed = AgentRunner::new(Arc::new(Fixture), Arc::new(LocalTools::new()))
        .with_event_journal(sessions.clone())
        .with_native_runtime_identity("fixture", "https://fixture.example/v1");
    let handle = seed.start_with_shared_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: root.into(),
            history: vec![],
            prompt: "Seed completed turn".into(),
            mode: TurnMode::Build,
        },
        lease.clone(),
    );
    while let Ok(_event) = handle.events.recv_timeout(Duration::from_secs(10)) {}
    assert!(sessions
        .load(&session.id)
        .unwrap()
        .unwrap()
        .events
        .iter()
        .any(|e| matches!(e.kind, AgentEventKind::TurnCompleted { .. })));
    drop(handle);
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "tools",
            "Read, verify and close goal",
            "goal-tools-fixture",
            TurnMode::Build,
        )
        .unwrap();
    let tools = Arc::new(
        NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "tools",
        )
        .unwrap(),
    );
    let model = Arc::new(GoalToolModel(std::sync::atomic::AtomicUsize::new(0)));
    let executor: Arc<dyn openide_harness::ToolExecutor> = match cut {
        Some(name) => Arc::new(CutGoalExecutor {
            inner: tools.clone(),
            name: name.into(),
        }),
        None => tools.clone(),
    };
    let runner = AgentRunner::new(model.clone(), executor)
        .with_event_journal(sessions.clone())
        .with_native_runtime_identity("fixture", "https://fixture.example/v1");
    let handle = runner.start_with_shared_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: root.into(),
            history: vec![],
            prompt: "Read, verify and close goal".into(),
            mode: TurnMode::Build,
        },
        lease.clone(),
    );
    let mut events = vec![];
    loop {
        match handle.events.recv_timeout(Duration::from_secs(10)) {
            Ok(event) => events.push(event),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(error) => panic!("{error}"),
        }
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, AgentEventKind::TurnCompleted { .. })),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ToolFinished { .. }))
            .count(),
        3
    );
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    let goal = goals.list(&session.id).unwrap().pop().unwrap();
    assert_eq!(goal.status, NativeGoalStatus::Complete);
    assert_eq!(goal.revision, 2);
    assert_eq!(goal.criteria[0].evidence.as_ref().unwrap().turn_id, "seed");
    tools.shutdown();
    drop(handle);
    drop(runner);
    drop(seed);
    drop(tools);
    drop(lease);
    drop(sessions);
    drop(goals);
    let goals = NativeGoalStore::new(Arc::new(SessionStore::open(root.join("sessions")).unwrap()));
    assert_eq!(goals.list(&session.id).unwrap(), vec![goal]);
}

struct CutGoalExecutor {
    inner: Arc<openide_harness::NativeGoalToolExecutor>,
    name: String,
}
#[async_trait]
impl openide_harness::ToolExecutor for CutGoalExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.inner.definitions()
    }
    async fn execute(
        &self,
        call: &openide_core::ToolCall,
        context: &openide_harness::ToolContext,
    ) -> Result<openide_harness::ToolOutput, AgentError> {
        let output = self.inner.execute(call, context).await?;
        if call.name == self.name {
            assert!(!output.is_error);
            std::process::exit(86);
        }
        Ok(output)
    }
}
#[test]
#[ignore = "subprocess helper: exit after goal commit before ToolFinished"]
fn goal_tool_commit_cut_child() {
    let Some(root) = std::env::var_os("OPENIDE_GOAL_TOOL_CUT_ROOT") else {
        return;
    };
    let cut = std::env::var("OPENIDE_GOAL_TOOL_CUT_NAME").unwrap();
    run_goal_tools(std::path::Path::new(&root), Some(&cut));
    panic!("cut was not reached");
}
#[test]
fn goal_tool_commit_cuts_reopen_exact_receipts_without_repeating_mutation() {
    use openide_harness::{NativeGoalToolExecutor, ToolContext, ToolExecutor};
    for name in ["record_goal_evidence", "complete_goal"] {
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "goal_tool_commit_cut_child",
                "--nocapture",
            ])
            .env("OPENIDE_GOAL_TOOL_CUT_ROOT", root.path())
            .env("OPENIDE_GOAL_TOOL_CUT_NAME", name)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(86),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let listing = sessions.list_with_diagnostics().unwrap();
        assert!(listing.diagnostics.is_empty());
        assert_eq!(listing.sessions.len(), 1);
        let id = &listing.sessions[0].session.id;
        let lease = Arc::new(sessions.try_acquire_native_lease(id).unwrap());
        let journal = sessions.load(id).unwrap().unwrap().events;
        let call = journal
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                AgentEventKind::ToolStarted { tool_call } if tool_call.name == name => {
                    Some(tool_call.clone())
                }
                _ => None,
            })
            .unwrap();
        assert!(!journal.iter().any(|e| matches!(&e.kind, AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == &call.id)));
        let goals = NativeGoalStore::new(sessions.clone());
        let before = goals.list(id).unwrap();
        let path = sessions
            .storage_root()
            .join(format!("{id}.native-goals.json"));
        let bytes = std::fs::read(&path).unwrap();
        let tools = NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease,
            "tools",
        )
        .unwrap();
        let context = ToolContext {
            workspace: root.path().into(),
            cancel: CancellationToken::new(),
        };
        let receipt = tools.committed_mutation(&call, &context).unwrap().unwrap();
        assert_eq!(
            receipt.committed_revision,
            if name == "complete_goal" { 2 } else { 1 }
        );
        assert_eq!(receipt.current_revision, receipt.committed_revision);
        assert_eq!(
            tools.committed_mutation(&call, &context).unwrap(),
            Some(receipt)
        );
        let mut changed = call.clone();
        changed.arguments["expected_revision"] = serde_json::json!(99);
        assert!(tools.committed_mutation(&changed, &context).is_err());
        let mut absent = call.clone();
        absent.id = "uncommitted".into();
        assert!(tools
            .committed_mutation(&absent, &context)
            .unwrap()
            .is_none());
        let foreign = tempfile::tempdir().unwrap();
        assert!(tools
            .committed_mutation(
                &call,
                &ToolContext {
                    workspace: foreign.path().into(),
                    cancel: CancellationToken::new()
                }
            )
            .is_err());
        // Inspection never executes a call or appends a result. An exact retry
        // under the same admission still yields only the existing revision.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            !runtime
                .block_on(tools.execute(&call, &context))
                .unwrap()
                .is_error
        );
        assert_eq!(goals.list(id).unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(sessions.load(id).unwrap().unwrap().events, journal);
        tools.shutdown();
        assert!(tools.committed_mutation(&call, &context).is_err());
    }
}

struct PlanGoalReader(std::sync::atomic::AtomicUsize);
#[async_trait]
impl ModelBackend for PlanGoalReader {
    fn model_id(&self) -> &str {
        "goal-plan-fixture"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([93; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        _: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        assert!(messages
            .first()
            .unwrap()
            .content
            .contains("Full original objective"));
        assert!(messages.first().unwrap().content.contains("Verify fixture"));
        assert!(tools.iter().any(|t| t.name == "get_goal"));
        assert!(!tools.iter().any(|t| matches!(
            t.name.as_str(),
            "record_goal_evidence" | "complete_goal" | "write_file" | "run_command"
        )));
        if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Ok(ModelOutput {
                content: String::new(),
                tool_calls: vec![openide_core::ToolCall {
                    id: "inspect-goal".into(),
                    name: "get_goal".into(),
                    arguments: serde_json::json!({}),
                }],
            })
        } else {
            assert!(messages
                .iter()
                .any(|m| m.content.contains("Full original objective")));
            Ok(ModelOutput {
                content: "Inspect every criterion before Build".into(),
                tool_calls: vec![],
            })
        }
    }
}
#[test]
fn plan_runner_reads_native_goal_without_advertising_mutations_or_changing_ledger() {
    use openide_harness::{NativeGoalToolExecutor, ToolExecutor};
    let root = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let session = sessions.create(root.path(), "native").unwrap();
    let lease = Arc::new(sessions.try_acquire_native_lease(&session.id).unwrap());
    let goals = NativeGoalStore::new(sessions.clone());
    let goal = goals
        .create(
            &lease,
            "goal",
            "Full original objective",
            vec![NativeGoalCriterion {
                id: "verify".into(),
                description: "Verify fixture".into(),
                evidence: None,
            }],
        )
        .unwrap();
    let path = sessions
        .storage_root()
        .join(format!("{}.native-goals.json", session.id));
    let bytes = std::fs::read(&path).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "plan",
            "Inspect",
            "goal-plan-fixture",
            TurnMode::Plan,
        )
        .unwrap();
    let tools = Arc::new(
        NativeGoalToolExecutor::new(
            Arc::new(LocalTools::new()),
            sessions.clone(),
            lease.clone(),
            "plan",
        )
        .unwrap(),
    );
    assert!(tools.certifies_plan_read("get_goal"));
    assert!(!tools.certifies_parallel_read("get_goal"));
    let model = Arc::new(PlanGoalReader(std::sync::atomic::AtomicUsize::new(0)));
    let instructions = openide_harness::native_goal_system_instructions(
        "Frozen project instructions",
        Some(&goal),
    )
    .unwrap();
    let runner = AgentRunner::new(model.clone(), tools)
        .with_system_instructions(instructions.clone())
        .with_event_journal(sessions.clone())
        .with_native_runtime_identity("fixture", "https://fixture.example/v1");
    let handle = runner.start_with_shared_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: root.path().into(),
            history: vec![],
            prompt: "Inspect".into(),
            mode: TurnMode::Plan,
        },
        lease,
    );
    let mut events = vec![];
    loop {
        match handle.events.recv_timeout(Duration::from_secs(10)) {
            Ok(event) => events.push(event),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, AgentEventKind::TurnCompleted { .. })),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ToolFinished { .. }))
            .count(),
        1
    );
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(goals.list(&session.id).unwrap(), vec![goal]);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let saved = sessions.load(&session.id).unwrap().unwrap();
    assert!(saved.events.iter().any(|e|matches!(&e.kind, AgentEventKind::SystemInstructionsCommitted {content, ..} if content == &instructions)));
}
