use async_trait::async_trait;
use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentRunRequest, CheckpointBudgetSnapshot,
    CheckpointCompatibility, CheckpointFileStore, NativeEventJournal, ReplayRecovery, SessionStore,
    StoreError, ToolCall, TurnMode,
};
use openide_harness::{
    AgentError, AgentRunner, CheckpointCoordinator, LocalTools, ModelBackend,
    ModelRequestCapabilities, OpenAiCompatibleBackend, PermissionPolicy, ResumeDisposition,
    ResumeEngine, RunLimits, ToolCatalog, ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
};
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

mod active_checkpoint_acceptance {
    use super::*;
    use openide_harness::TokenBudgetPolicy;

    const BUILD_PROMPT: &str = "Apply one receipt, then report the retained Build result";
    const PARTIAL: &str = "Unconfirmed résumé 🌱 after checkpoint";
    const FINAL: &str = "The single receipt is confirmed after checkpoint recovery.";
    const CALL_ID: &str = "active-checkpoint-receipt";

    fn plan_response() -> String {
        "Historical confirmed Plan 🌱: preserve one receipt. ".repeat(120)
    }

    fn receipt_output() -> String {
        "receipt verified on disk\n".repeat(600)
    }

    fn budget() -> TokenBudgetPolicy {
        TokenBudgetPolicy {
            input_limit_tokens: None,
            context_window_tokens: 60_000,
            output_reserve_tokens: 1_024,
            tool_schema_reserve_tokens: 256,
            auto_compact_token_limit: Some(28_000),
        }
    }

    fn limits() -> RunLimits {
        RunLimits {
            max_model_steps: 3,
            max_tool_calls: 1,
            max_tool_output_chars: 20_000,
        }
    }

    struct ReceiptTool;

    #[async_trait]
    impl ToolExecutor for ReceiptTool {
        fn definitions(&self) -> Vec<ToolDefinition> {
            UncertainReceiptTool {
                calls: AtomicUsize::new(0),
            }
            .definitions()
        }

        async fn execute(
            &self,
            call: &ToolCall,
            context: &ToolContext,
        ) -> Result<ToolOutput, AgentError> {
            assert_eq!(call.id, CALL_ID);
            assert_eq!(call.name, "append_receipt");
            assert_eq!(call.arguments, json!({"label":"applied"}));
            let mut receipt = OpenOptions::new()
                .create(true)
                .append(true)
                .open(context.workspace.join("effects.log"))
                .unwrap();
            writeln!(receipt, "applied").unwrap();
            receipt.sync_all().unwrap();
            Ok(ToolOutput::success(receipt_output()))
        }
    }

    struct CutJournal {
        sessions: Arc<SessionStore>,
        point: String,
    }

    impl NativeEventJournal for CutJournal {
        fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
            if self.point == "checkpoint_committed"
                && matches!(
                    event.kind,
                    AgentEventKind::ModelRequestStarted { step: 2, .. }
                )
            {
                // The coordinator appends and syncs the marker directly to the
                // store. Intercept the next append before it reaches disk so
                // the abrupt process exit leaves that exact marker as the tail.
                let record = self.sessions.load(&event.session_id)?.unwrap();
                assert!(matches!(
                    record.events.last().unwrap().kind,
                    AgentEventKind::CheckpointCommitted { .. }
                ));
                std::process::exit(PROCESS_CUT_EXIT);
            }
            self.sessions.append_native_event(event)?;
            if self.point == "assistant_delta"
                && matches!(&event.kind, AgentEventKind::AssistantDelta { content } if content == PARTIAL)
            {
                let record = self.sessions.load(&event.session_id)?.unwrap();
                assert!(record
                    .events
                    .iter()
                    .any(|saved| matches!(saved.kind, AgentEventKind::CheckpointCommitted { .. })));
                assert!(record.events.iter().any(|saved| matches!(
                    saved.kind,
                    AgentEventKind::ModelRequestStarted { step: 2, .. }
                )));
                // No destructor, cancellation callback or terminal append runs.
                std::process::exit(PROCESS_CUT_EXIT);
            }
            Ok(())
        }
    }

    #[test]
    #[ignore = "subprocess fixture invoked by active-checkpoint HTTP acceptance"]
    fn active_checkpoint_cut_child() {
        let Ok(root) = std::env::var("OPENIDE_ACTIVE_CHECKPOINT_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let session_id = std::env::var("OPENIDE_ACTIVE_CHECKPOINT_SESSION").unwrap();
        let endpoint = std::env::var("OPENIDE_ACTIVE_CHECKPOINT_ENDPOINT").unwrap();
        let point = std::env::var("OPENIDE_ACTIVE_CHECKPOINT_POINT").unwrap();
        let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
        let checkpoints =
            Arc::new(CheckpointFileStore::new(root.join("checkpoint-store")).unwrap());
        let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
        let record = sessions.load(&session_id).unwrap().unwrap();
        let admission = record.events.last().unwrap();
        assert!(matches!(
            &admission.kind,
            AgentEventKind::TurnAdmitted { prompt, mode: TurnMode::Build, .. } if prompt == BUILD_PROMPT
        ));
        let prior = replay_native(
            &session_id,
            &record.events[..record.events.len() - 1],
            Some(&checkpoints),
            &process_compatibility(),
        )
        .unwrap();
        let runner = AgentRunner::new(process_backend(&endpoint), Arc::new(ReceiptTool))
            .with_system_instructions(process_goal_instructions(sessions.clone(), &session_id))
            .with_plan_snapshot(prior.latest_plan)
            .with_native_runtime_identity("fixture", &endpoint)
            .with_permission_policy(process_policy())
            .with_token_budget_policy(budget())
            .with_auto_compaction(sessions.clone(), checkpoints, process_compatibility())
            .with_event_journal(Arc::new(CutJournal { sessions, point }))
            .with_limits(limits());
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session_id),
                sequence_start: admission.sequence,
                workspace: root.join("workspace"),
                history: prior.history,
                prompt: BUILD_PROMPT.into(),
                mode: TurnMode::Build,
            },
            lease,
        );
        while let Ok(event) = handle.events.recv_timeout(Duration::from_secs(10)) {
            assert!(
                !matches!(
                    event.kind,
                    AgentEventKind::TurnFailed { .. }
                        | AgentEventKind::TurnCancelled
                        | AgentEventKind::TurnCompleted { .. }
                ),
                "unexpected terminal before active checkpoint cut: {:?}",
                event.kind
            );
            if let AgentEventKind::PermissionRequested { request_id, .. } = event.kind {
                assert!(handle
                    .resolve_permission(&request_id, openide_harness::PermissionChoice::Allow));
            }
        }
        panic!("active Build failed before its checkpoint interruption");
    }

    fn assert_retained_request(request: &Value, original_system: &str) {
        assert_eq!(request["stream"], true);
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], original_system);
        assert!(messages
            .iter()
            .any(|message| { message["role"] == "user" && message["content"] == BUILD_PROMPT }));
        assert!(messages.iter().any(|message| {
            message["role"] == "assistant"
                && message["tool_calls"][0]["id"] == CALL_ID
                && message["tool_calls"][0]["function"]["name"] == "append_receipt"
                && message["tool_calls"][0]["function"]["arguments"] == "{\"label\":\"applied\"}"
        }));
        assert!(messages.iter().any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == CALL_ID
                && message["content"] == receipt_output()
        }));
        assert!(!messages.iter().any(|message| {
            message["role"] == "assistant" && message["content"] == plan_response()
        }));
        assert!(!messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains(PARTIAL))
        }));
        assert!(messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("Only the completed Plan was summarized"))
        }));
    }

    #[test]
    fn active_build_checkpoint_process_cuts_reopen_and_resume_http_without_repeating_tool() {
        for point in ["checkpoint_committed", "assistant_delta"] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            let sessions_root = root.path().join("sessions");
            let checkpoints_root = root.path().join("checkpoint-store");
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let request_count = Arc::new(AtomicUsize::new(0));
            let server_count = request_count.clone();
            let (release_partial, partial_release) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let socket = accept_bounded(&listener);
                let plan = read_request(&socket);
                server_count.fetch_add(1, Ordering::SeqCst);
                assert!(plan.get("tools").is_none());
                assert!(plan["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains(PROCESS_GOAL));
                respond_sse(
                    socket,
                    &[
                        json!({"id":"prior-plan", "choices":[{"delta":{"content":plan_response()},"finish_reason":"stop"}]}),
                    ],
                );

                let socket = accept_bounded(&listener);
                let build = read_request(&socket);
                server_count.fetch_add(1, Ordering::SeqCst);
                let original_system = build["messages"][0]["content"].as_str().unwrap().to_owned();
                assert!(original_system.contains(PROCESS_GOAL));
                assert!(original_system.contains(PROCESS_CRITERION));
                assert!(original_system.contains("Current task plan snapshot"));
                assert!(build["messages"].as_array().unwrap().iter().any(|message| {
                    message["role"] == "assistant" && message["content"] == plan_response()
                }));
                respond_sse(
                    socket,
                    &[
                        json!({"id":"active-tool", "choices":[{"delta":{"tool_calls":[{"index":0,"id":CALL_ID,"function":{"name":"append_receipt","arguments":"{\"label\":\"applied\"}"}}]}}]}),
                        json!({"id":"active-tool", "choices":[{"finish_reason":"tool_calls"}]}),
                    ],
                );

                let socket = accept_bounded(&listener);
                let compact = read_request(&socket);
                server_count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(compact["stream"], false);
                assert!(compact.get("tools").is_none());
                let compact_messages = compact["messages"].as_array().unwrap();
                assert!(compact_messages.iter().any(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains(&plan_response()))
                }));
                assert!(!compact_messages.iter().any(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("receipt verified on disk"))
                }));
                let summary = json!({"objective":"Retain one receipt", "decisions":["Keep the active Build suffix exact"],
                    "files":[], "current_state":"Only the completed Plan was summarized", "constraints":["Never repeat a confirmed tool"], "remaining_work":["Complete the active Build"]});
                respond_json(
                    socket,
                    json!({"choices":[{"message":{"role":"assistant","content":summary.to_string()},"finish_reason":"stop"}]}),
                );

                if point == "assistant_delta" {
                    let mut socket = accept_bounded(&listener);
                    let in_flight = read_request(&socket);
                    server_count.fetch_add(1, Ordering::SeqCst);
                    assert_retained_request(&in_flight, &original_system);
                    let prefix = format!(
                        "data: {}\n\n",
                        json!({"id":"interrupted-after-checkpoint","choices":[{"delta":{"content":PARTIAL}}]})
                    );
                    let withheld = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"finish_reason":"stop"}]})
                    );
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{prefix}", prefix.len() + withheld.len()).unwrap();
                    socket.flush().unwrap();
                    // Never publish a terminal frame before the child is dead.
                    partial_release
                        .recv_timeout(Duration::from_secs(20))
                        .unwrap();
                    drop(socket);
                }

                let socket = accept_bounded(&listener);
                let resumed = read_request(&socket);
                server_count.fetch_add(1, Ordering::SeqCst);
                assert_retained_request(&resumed, &original_system);
                respond_sse(
                    socket,
                    &[
                        json!({"id":"resumed-after-checkpoint","choices":[{"delta":{"content":FINAL},"finish_reason":"stop"}]}),
                    ],
                );
                // Every legitimate request has already arrived. A retry or a
                // repeated tool loop would be an extra external model effect.
                assert!(
                    matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
                );
            });

            let session_id = {
                let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
                let session = sessions.create(&workspace, "native").unwrap();
                let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
                openide_core::NativeGoalStore::new(sessions.clone())
                    .create(
                        &lease,
                        "process-goal",
                        PROCESS_GOAL,
                        vec![openide_core::NativeGoalCriterion {
                            id: "recovery".into(),
                            description: PROCESS_CRITERION.into(),
                            evidence: None,
                        }],
                    )
                    .unwrap();
                let admission = sessions
                    .admit_native_turn(
                        &session.id,
                        "plan",
                        "Plan one receipt operation",
                        "fixture-model",
                        TurnMode::Plan,
                    )
                    .unwrap();
                let runner = AgentRunner::new(process_backend(&endpoint), Arc::new(ReceiptTool))
                    .with_system_instructions(process_goal_instructions(
                        sessions.clone(),
                        &session.id,
                    ))
                    .with_native_runtime_identity("fixture", &endpoint)
                    .with_event_journal(sessions.clone());
                let events = collect_run(&runner.start_with_lease(
                    AgentRunRequest {
                        session_id: Some(session.id.clone()),
                        sequence_start: admission.sequence,
                        workspace: workspace.clone(),
                        history: vec![],
                        prompt: "Plan one receipt operation".into(),
                        mode: TurnMode::Plan,
                    },
                    lease,
                ));
                assert!(matches!(
                    events.last().unwrap().kind,
                    AgentEventKind::TurnCompleted { .. }
                ));
                session.id
            };
            // Build derives its context from a newly opened store, not a live
            // Plan handle or a synthetic prefix of journal events.
            let sessions = SessionStore::open(&sessions_root).unwrap();
            let prior = sessions.load(&session_id).unwrap().unwrap();
            let completed_plan_sequence = prior.events.last().unwrap().sequence;
            let prior_replay =
                replay_native(&session_id, &prior.events, None, &process_compatibility()).unwrap();
            let plan_snapshot = prior_replay.latest_plan.unwrap();
            sessions
                .admit_native_turn(
                    &session_id,
                    "build",
                    BUILD_PROMPT,
                    "fixture-model",
                    TurnMode::Build,
                )
                .unwrap();
            drop(sessions);

            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "active_checkpoint_acceptance::active_checkpoint_cut_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("OPENIDE_ACTIVE_CHECKPOINT_ROOT", root.path())
                .env("OPENIDE_ACTIVE_CHECKPOINT_SESSION", &session_id)
                .env("OPENIDE_ACTIVE_CHECKPOINT_ENDPOINT", &endpoint)
                .env("OPENIDE_ACTIVE_CHECKPOINT_POINT", point)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("active checkpoint child timeout at {point}");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(
                status.code(),
                Some(PROCESS_CUT_EXIT),
                "real process cut at {point}"
            );
            if point == "assistant_delta" {
                release_partial.send(()).unwrap();
            }
            assert_eq!(
                std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
                "applied\n"
            );

            let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
            let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
            let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
            let before = sessions.load(&session_id).unwrap().unwrap().events;
            let marker = before
                .iter()
                .find_map(|event| {
                    if let AgentEventKind::CheckpointCommitted { reference } = &event.kind {
                        Some(reference.clone())
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(marker.covered_event_sequence, completed_plan_sequence);
            assert_eq!(
                before
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                    .count(),
                1
            );
            assert_eq!(
                before
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                    .count(),
                1
            );
            assert_eq!(
                before
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        AgentEventKind::PermissionRequested { .. }
                    ))
                    .count(),
                1
            );
            assert_eq!(
                before
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::PermissionResolved { .. }))
                    .count(),
                1
            );
            assert_eq!(
                before
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
                    .count(),
                1,
                "Build is still active at the cut"
            );
            let route = openide_core::ModelRouteSnapshot::new(
                "fixture",
                "fixture-model",
                &endpoint,
                process_backend(&endpoint)
                    .request_profile_fingerprint()
                    .unwrap(),
            )
            .unwrap();
            let tools = Arc::new(ReceiptTool);
            let engine = ResumeEngine::new(
                sessions.clone(),
                checkpoints.clone(),
                process_compatibility(),
                process_backend(&endpoint),
                tools.clone(),
                ToolCatalog::new(tools.definitions()).unwrap(),
                process_policy(),
                Some(budget()),
            )
            .with_native_runtime_identity("fixture", &endpoint)
            .with_system_instructions(
                "Changed project instructions must not replace the admitted snapshot",
            )
            .with_limits(limits());
            let request_count_before = request_count.load(Ordering::SeqCst);
            let interrupted = engine.inspect(&lease).unwrap();
            assert!(!interrupted.can_start_new_turn);
            assert_eq!(interrupted.active_model_route.as_ref(), Some(&route));
            assert_eq!(interrupted.latest_plan, Some(plan_snapshot.clone()));
            assert_eq!(
                interrupted.active_turn.as_ref().unwrap().mode,
                TurnMode::Build
            );
            assert_eq!(
                interrupted.active_turn.as_ref().unwrap().prompt,
                BUILD_PROMPT
            );
            let checkpoint = interrupted.checkpoint.clone().unwrap();
            assert_eq!(checkpoint.covered_event_sequence, completed_plan_sequence);
            assert!(!interrupted
                .history
                .iter()
                .any(|message| message.content.contains(PARTIAL)));
            assert!(interrupted
                .history
                .iter()
                .any(|message| message.tool_call_id.as_deref() == Some(CALL_ID)
                    && message.content == receipt_output()));
            if point == "checkpoint_committed" {
                assert!(matches!(
                    before.last().unwrap().kind,
                    AgentEventKind::CheckpointCommitted { .. }
                ));
                assert_eq!(
                    interrupted.recovery,
                    ReplayRecovery::ReadyForModel { next_step: 2 }
                );
                assert_eq!(request_count_before, 3);
            } else {
                assert!(
                    matches!(&before.last().unwrap().kind, AgentEventKind::AssistantDelta { content } if content == PARTIAL)
                );
                assert_eq!(
                    interrupted.recovery,
                    ReplayRecovery::ModelInFlight {
                        step: 2,
                        model: "fixture-model".into(),
                    }
                );
                assert_eq!(request_count_before, 4);
                assert!(!before.iter().any(|event| matches!(
                    event.kind,
                    AgentEventKind::ModelResponseCommitted { step: 2, .. }
                )));
            }
            assert_eq!(
                request_count.load(Ordering::SeqCst),
                request_count_before,
                "reopen/inspect must not infer"
            );
            assert_eq!(sessions.load(&session_id).unwrap().unwrap().events, before);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let resumed = runtime
                .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
                .unwrap();
            assert_eq!(resumed.disposition, ResumeDisposition::Complete);
            let completed = sessions.load(&session_id).unwrap().unwrap().events;
            assert_eq!(&completed[..before.len()], before);
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                    .count(),
                1
            );
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                    .count(),
                1
            );
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        AgentEventKind::CheckpointCommitted { .. }
                    ))
                    .count(),
                1
            );
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        AgentEventKind::ModelRouteCommitted { .. }
                    ))
                    .count(),
                2,
                "one route per Plan/Build, no resume duplicate"
            );
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        AgentEventKind::ModelRequestStarted { step: 2, .. }
                    ))
                    .count(),
                1,
                "an in-flight retry reuses the durable request"
            );
            assert_eq!(
                completed
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        AgentEventKind::ModelResponseCommitted { step: 2, .. }
                    ))
                    .count(),
                1
            );
            assert!(completed.iter().any(|event| matches!(&event.kind, AgentEventKind::ModelResponseCommitted { step:2, assistant, .. } if assistant.content == FINAL)));
            assert_eq!(
                std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
                "applied\n"
            );
            drop(engine);
            drop(lease);
            drop(sessions);
            drop(checkpoints);
            let reopened = SessionStore::open(&sessions_root).unwrap();
            let reopened_checkpoints = CheckpointFileStore::new(&checkpoints_root).unwrap();
            let record = reopened.load(&session_id).unwrap().unwrap();
            assert_eq!(record.events, completed);
            let replay = replay_native(
                &session_id,
                &record.events,
                Some(&reopened_checkpoints),
                &process_compatibility(),
            )
            .unwrap();
            assert!(replay.can_start_new_turn);
            assert_eq!(replay.checkpoint, Some(checkpoint));
            assert_eq!(replay.latest_plan, Some(plan_snapshot));
            assert_eq!(replay.history.last().unwrap().content, FINAL);
            assert!(!replay
                .history
                .iter()
                .any(|message| message.content.contains(PARTIAL)));
            let goals = openide_core::NativeGoalStore::new(Arc::new(reopened))
                .list(&session_id)
                .unwrap();
            assert_eq!(goals[0].objective, PROCESS_GOAL);
            assert_eq!(goals[0].criteria[0].description, PROCESS_CRITERION);
            assert_eq!(goals[0].revision, 0);
            assert!(goals[0].criteria[0].evidence.is_none());
            server.join().unwrap();
            assert_eq!(
                request_count.load(Ordering::SeqCst),
                if point == "assistant_delta" { 5 } else { 4 }
            );
        }
    }
}

fn fixture_capabilities() -> ModelRequestCapabilities {
    ModelRequestCapabilities {
        tool_calls: true,
        ..Default::default()
    }
}

fn read_request(socket: &TcpStream) -> Value {
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    assert!(request_line.starts_with("POST /v1/chat/completions "));
    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; content_length.expect("content length")];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn respond_sse(mut socket: TcpStream, events: &[Value]) {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    socket.flush().unwrap();
}

fn respond_json(mut socket: TcpStream, body: Value) {
    let body = body.to_string();
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    socket.flush().unwrap();
}

fn accept_bounded(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "fixture HTTP request never arrived"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    }
}

fn collect_run(handle: &openide_harness::AgentRunHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        match handle.events.recv_timeout(Duration::from_secs(10)) {
            Ok(event) => events.push(event),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return events,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                handle.cancel();
                panic!("native runner did not close within the fixture limit");
            }
        }
    }
}

/// One local effect is durably observable even when its executor loses the
/// acknowledgement. Reusing this executor after reopening would append again.
struct UncertainReceiptTool {
    calls: AtomicUsize,
}

#[async_trait]
impl ToolExecutor for UncertainReceiptTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "append_receipt".into(),
            description: "Append one receipt inside the workspace.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"label": {"type": "string"}},
                "required": ["label"],
                "additionalProperties": false
            }),
        }]
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        assert_eq!(call.name, "append_receipt");
        let label = call.arguments["label"].as_str().unwrap();
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(context.workspace.join("effects.log"))
            .unwrap();
        writeln!(file, "{label}").unwrap();
        file.sync_all().unwrap();
        Err(AgentError::EffectUncertain(
            "receipt persisted, acknowledgement lost".into(),
        ))
    }
}

/// Simulate losing the terminal journal append when the worker exits after an
/// uncertain tool result. The preceding synced ToolStarted remains recoverable.
struct DropTerminalJournal(Arc<SessionStore>);

impl NativeEventJournal for DropTerminalJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if matches!(event.kind, AgentEventKind::TurnFailed { .. }) {
            return Err(StoreError::Io(std::io::Error::other(
                "injected terminal journal interruption",
            )));
        }
        self.0.append_native_event(event)
    }
}

#[test]
fn plan_build_uncertain_effect_reopens_resumes_and_replays_checkpoint() {
    const MODEL: &str = "fixture-model";
    const PLAN_PROMPT: &str = "Plan one receipt operation";
    const BUILD_PROMPT: &str = "Append the receipt exactly once";
    const CALL_ID: &str = "receipt-call";
    const GOAL: &str = "Append exactly one receipt and verify recovery without repeating effects";
    const LIMITS: RunLimits = RunLimits {
        max_model_steps: 3,
        max_tool_calls: 1,
        max_tool_output_chars: 256,
    };

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sessions_root = root.path().join("sessions");
    let checkpoints_root = root.path().join("checkpoints");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let plan_socket = accept_bounded(&listener);
        let plan = read_request(&plan_socket);
        assert_eq!(plan["stream"], true);
        let system = plan["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "system")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(system.contains(GOAL));
        assert!(system.contains("Verify receipt on disk"));
        assert!(system.contains("Verify recovery after reopen"));
        assert!(system.contains("\"status\":\"active\""));

        assert!(plan.get("tools").is_none(), "Plan must not advertise tools");
        assert!(plan["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| { message["role"] == "user" && message["content"] == PLAN_PROMPT }));
        respond_sse(
            plan_socket,
            &[
                json!({"id":"plan-response","choices":[{"delta":{"content":"First record the receipt."},"finish_reason":"stop"}]}),
            ],
        );

        let build_socket = accept_bounded(&listener);
        let build = read_request(&build_socket);
        assert_eq!(build["stream"], true);
        let system = build["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "system")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(system.contains(GOAL));
        assert!(system.contains("Verify receipt on disk"));
        assert!(system.contains("Verify recovery after reopen"));
        assert!(system.contains("\"status\":\"active\""));

        assert_eq!(build["max_completion_tokens"], 4096);
        assert!(build["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["function"]["name"] == "append_receipt" }));
        assert!(build["messages"].as_array().unwrap().iter().any(|message| {
            message["role"] == "assistant" && message["content"] == "First record the receipt."
        }));
        respond_sse(
            build_socket,
            &[
                json!({"id":"build-tool-response","choices":[{"delta":{"tool_calls":[{
                    "index":0,"id":CALL_ID,"function":{"name":"append_receipt","arguments":"{\"label\":\"applied\"}"}
                }]}}]}),
                json!({"id":"build-tool-response","choices":[{"finish_reason":"tool_calls"}]}),
            ],
        );

        let recovered_socket = accept_bounded(&listener);
        let recovered = read_request(&recovered_socket);
        assert_eq!(recovered["stream"], true);
        let system = recovered["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "system")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(system.contains(GOAL));
        assert!(system.contains("Verify receipt on disk"));
        assert!(system.contains("Verify recovery after reopen"));
        assert!(system.contains("\"status\":\"active\""));

        assert_eq!(recovered["max_completion_tokens"], 4096);
        assert!(recovered["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == CALL_ID
                    && message["content"] == "receipt verified on disk"
            }));
        respond_sse(
            recovered_socket,
            &[
                json!({"id":"build-final-response","choices":[{"delta":{"content":"Receipt confirmed once."},"finish_reason":"stop"}]}),
            ],
        );

        let compact_socket = accept_bounded(&listener);
        let compact = read_request(&compact_socket);
        assert_eq!(compact["stream"], false);
        assert_eq!(compact["max_completion_tokens"], 4096);
        assert!(compact.get("tools").is_none());
        assert!(compact["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["content"].as_str().is_some_and(|text| {
                    text.contains("Receipt confirmed once.")
                        && text.contains("First record the receipt.")
                })
            }));
        let summary = json!({
            "objective":"Append one receipt",
            "decisions":["Plan before Build"],
            "files":[{"path":"effects.log","state":"one applied receipt verified"}],
            "current_state":"Receipt confirmed once after recovery",
            "constraints":["Do not repeat an uncertain effect"],
            "remaining_work":[]
        });
        respond_json(
            compact_socket,
            json!({"choices":[{"message":{"role":"assistant","content":summary.to_string()},"finish_reason":"stop"}]}),
        );
        let next_socket = accept_bounded(&listener);
        let next = read_request(&next_socket);
        let system = next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "system")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(system.contains(GOAL));
        assert!(system.contains("Verify receipt on disk"));
        assert!(system.contains("Verify recovery after reopen"));
        assert!(system.contains("\"status\":\"paused\""));
        assert!(next["messages"].as_array().unwrap().iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|s| s.contains("Receipt confirmed once"))
        }));
        respond_sse(
            next_socket,
            &[
                json!({"id":"after-checkpoint","choices":[{"delta":{"content":"Saved goal scope preserved."},"finish_reason":"stop"}]}),
            ],
        );
    });

    let endpoint = format!("http://{address}/v1");
    let backend: Arc<dyn ModelBackend> = Arc::new(
        OpenAiCompatibleBackend::new(&endpoint, None, MODEL)
            .with_capabilities(fixture_capabilities())
            .unwrap()
            .with_request_field("max_completion_tokens", json!(4096))
            .unwrap()
            .with_timeout(Duration::from_secs(10)),
    );
    let compatibility = CheckpointCompatibility {
        provider: "fixture".into(),
        model: MODEL.into(),
        context_format_key: "chat-v1".into(),
    };
    let expected_route = openide_core::ModelRouteSnapshot::new(
        "fixture",
        MODEL,
        &endpoint,
        backend.request_profile_fingerprint().unwrap(),
    )
    .unwrap();
    let tools = Arc::new(UncertainReceiptTool {
        calls: AtomicUsize::new(0),
    });
    let policy = PermissionPolicy::new();

    // Plan is a normal streamed native run. Close the store before deriving
    // Build history to prove that its context comes from the durable journal.
    let session_id = {
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let goal = openide_core::NativeGoalStore::new(sessions.clone())
            .create(
                &lease,
                "goal",
                GOAL,
                vec![
                    openide_core::NativeGoalCriterion {
                        id: "disk".into(),
                        description: "Verify receipt on disk".into(),
                        evidence: None,
                    },
                    openide_core::NativeGoalCriterion {
                        id: "recovery".into(),
                        description: "Verify recovery after reopen".into(),
                        evidence: None,
                    },
                ],
            )
            .unwrap();
        let instructions = openide_harness::native_goal_system_instructions(
            "Exact project instructions",
            Some(&goal),
        )
        .unwrap();
        let admission = sessions
            .admit_native_turn(&session.id, "plan", PLAN_PROMPT, MODEL, TurnMode::Plan)
            .unwrap();
        let runner = AgentRunner::new(backend.clone(), tools.clone())
            .with_system_instructions(instructions)
            .with_native_runtime_identity("fixture", &endpoint)
            .with_event_journal(sessions)
            .with_limits(LIMITS);
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace: workspace.clone(),
                history: vec![],
                prompt: PLAN_PROMPT.into(),
                mode: TurnMode::Plan,
            },
            lease,
        );
        let events = collect_run(&handle);
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(AgentEventKind::TurnCompleted { .. })
        ));
        session.id
    };

    let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
    let plan_record = sessions.load(&session_id).unwrap().unwrap();
    let plan_replay =
        replay_native(&session_id, &plan_record.events, None, &compatibility).unwrap();
    assert!(plan_replay.can_start_new_turn);
    assert!(plan_replay.active_model_route.is_none());
    assert_eq!(
        plan_record
            .events
            .iter()
            .filter_map(|event| openide_core::ModelRouteSnapshot::from_event(&event.kind))
            .collect::<Vec<_>>(),
        vec![expected_route.clone()]
    );
    assert!(plan_record.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::TurnAdmitted {
            mode: TurnMode::Plan,
            ..
        }
    )));
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    let admission = sessions
        .admit_native_turn(&session_id, "build", BUILD_PROMPT, MODEL, TurnMode::Build)
        .unwrap();
    let goal = openide_core::NativeGoalStore::new(sessions.clone())
        .list(&session_id)
        .unwrap()
        .pop()
        .unwrap();
    let instructions =
        openide_harness::native_goal_system_instructions("Exact project instructions", Some(&goal))
            .unwrap();
    let runner = AgentRunner::new(backend.clone(), tools.clone())
        .with_system_instructions(instructions.clone())
        .with_native_runtime_identity("fixture", &endpoint)
        .with_event_journal(Arc::new(DropTerminalJournal(sessions.clone())))
        .with_permission_policy(policy.clone())
        .with_plan_snapshot(plan_replay.latest_plan.clone())
        .with_limits(LIMITS);
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session_id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.clone(),
            history: plan_replay.history,
            prompt: BUILD_PROMPT.into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    let build_events = collect_run(&handle);
    assert!(handle
        .journal_error()
        .as_deref()
        .is_some_and(|error| error.contains("injected terminal journal interruption")));
    assert!(matches!(
        build_events.last().map(|event| &event.kind),
        Some(AgentEventKind::ToolStarted { tool_call }) if tool_call.id == CALL_ID
    ));
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
        "applied\n"
    );
    drop(sessions);

    let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
    let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
    let interrupted_record = sessions.load(&session_id).unwrap().unwrap();
    let interrupted = replay_native(
        &session_id,
        &interrupted_record.events,
        Some(&checkpoints),
        &compatibility,
    )
    .unwrap();
    assert!(matches!(
        interrupted.recovery,
        ReplayRecovery::ToolOutcomeUncertain { ref tool_call } if tool_call.id == CALL_ID
    ));
    assert!(!interrupted.can_start_new_turn);
    assert_eq!(
        interrupted.active_model_route.as_ref(),
        Some(&expected_route)
    );
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    let paused = openide_core::NativeGoalStore::new(sessions.clone())
        .update(
            &lease,
            "goal",
            "user-pause-after-cut",
            0,
            openide_core::NativeGoalMutation::PauseByUser,
        )
        .unwrap();
    let catalog = ToolCatalog::new(tools.definitions()).unwrap();
    let engine = ResumeEngine::new(
        sessions.clone(),
        checkpoints.clone(),
        compatibility.clone(),
        backend.clone(),
        tools.clone(),
        catalog,
        policy.clone(),
        None,
    )
    .with_native_runtime_identity("fixture", &endpoint)
    .with_limits(LIMITS);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // A changed or removed output cap is a different request profile after
    // reopening storage. It must not dispatch a model, repeat the uncertain
    // tool, or append a recovery event before rejecting the mismatch.
    for cap in [Some(2048), None] {
        let mut changed = OpenAiCompatibleBackend::new(&endpoint, None, MODEL)
            .with_capabilities(fixture_capabilities())
            .unwrap();
        if let Some(cap) = cap {
            changed = changed
                .with_request_field("max_completion_tokens", json!(cap))
                .unwrap();
        }
        let changed_engine = ResumeEngine::new(
            sessions.clone(),
            checkpoints.clone(),
            compatibility.clone(),
            Arc::new(changed),
            tools.clone(),
            ToolCatalog::new(tools.definitions()).unwrap(),
            policy.clone(),
            None,
        )
        .with_native_runtime_identity("fixture", &endpoint)
        .with_limits(LIMITS);
        let before = sessions.load(&session_id).unwrap().unwrap().events;
        assert!(matches!(
            runtime.block_on(changed_engine.run_until_pause(&lease, CancellationToken::new())),
            Err(openide_harness::ResumeError::ModelRouteMismatch)
        ));
        assert_eq!(sessions.load(&session_id).unwrap().unwrap().events, before);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
            "applied\n"
        );
    }
    // Real HTTP backend configuration is also checked before reopening can
    // dispatch anything to another provider, endpoint, or exact model ID.
    for (provider, route_endpoint, model_id, model_mismatch) in [
        ("another-provider", endpoint.as_str(), MODEL, false),
        ("fixture", "http://127.0.0.1:1/v1", MODEL, false),
        ("fixture", endpoint.as_str(), "another-exact-model", true),
    ] {
        let changed = OpenAiCompatibleBackend::new(route_endpoint, None, model_id)
            .with_capabilities(fixture_capabilities())
            .unwrap()
            .with_request_field("max_completion_tokens", json!(4096))
            .unwrap();
        let changed_engine = ResumeEngine::new(
            sessions.clone(),
            checkpoints.clone(),
            compatibility.clone(),
            Arc::new(changed),
            tools.clone(),
            ToolCatalog::new(tools.definitions()).unwrap(),
            policy.clone(),
            None,
        )
        .with_native_runtime_identity(provider, route_endpoint)
        .with_limits(LIMITS);
        let before = sessions.load(&session_id).unwrap().unwrap().events;
        let rejected =
            runtime.block_on(changed_engine.run_until_pause(&lease, CancellationToken::new()));
        assert!(if model_mismatch {
            matches!(rejected, Err(openide_harness::ResumeError::ModelMismatch))
        } else {
            matches!(
                rejected,
                Err(openide_harness::ResumeError::ModelRouteMismatch)
            )
        });
        assert_eq!(sessions.load(&session_id).unwrap().unwrap().events, before);
        assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
            "applied\n"
        );
    }
    let blocked = runtime
        .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
        .unwrap();
    assert!(matches!(
        blocked.disposition,
        ResumeDisposition::ToolOutcomeUncertain { ref tool_call, .. } if tool_call.id == CALL_ID
    ));
    assert!(blocked.appended_events.is_empty());
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
        "applied\n"
    );
    let resolved = engine
        .record_verified_tool_outcome(
            &lease,
            CALL_ID,
            ToolOutput::success("receipt verified on disk"),
        )
        .unwrap();
    assert_eq!(resolved.disposition, ResumeDisposition::Advanced);
    let finished = runtime
        .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
        .unwrap();
    assert_eq!(finished.disposition, ResumeDisposition::Complete);
    assert!(finished.replay.can_start_new_turn);
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    drop(lease);
    drop(engine);
    drop(sessions);

    // Compaction itself makes a real HTTP request with tools disabled. Its
    // content-addressed candidate becomes active only after the journal marker.
    let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
    let completed_record = sessions.load(&session_id).unwrap().unwrap();
    let completed = replay_native(
        &session_id,
        &completed_record.events,
        Some(&checkpoints),
        &compatibility,
    )
    .unwrap();
    assert!(completed.can_start_new_turn);
    assert!(completed_record.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::ToolFinished { tool_call_id, output, is_error: false }
            if tool_call_id == CALL_ID && output == "receipt verified on disk"
    )));
    assert_eq!(
        completed_record
            .events
            .iter()
            .filter(|event| matches!(
                &event.kind,
                AgentEventKind::ToolStarted { tool_call } if tool_call.id == CALL_ID
            ))
            .count(),
        1
    );
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    let coordinator = CheckpointCoordinator::new(
        sessions.clone(),
        checkpoints.clone(),
        backend.clone(),
        compatibility.clone(),
        CheckpointBudgetSnapshot::unverified(1_000, None),
    );
    let commit = runtime
        .block_on(coordinator.compact(&lease, CancellationToken::new()))
        .unwrap();
    assert_eq!(
        commit.checkpoint.covered_event_sequence,
        completed.last_sequence
    );
    assert!(commit.replay.can_start_new_turn);
    drop(lease);
    drop(coordinator);
    drop(sessions);
    drop(checkpoints);

    let reopened_sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
    let reopened_checkpoints = CheckpointFileStore::new(&checkpoints_root).unwrap();
    let record = reopened_sessions.load(&session_id).unwrap().unwrap();
    let replay = replay_native(
        &session_id,
        &record.events,
        Some(&reopened_checkpoints),
        &compatibility,
    )
    .unwrap();
    assert_eq!(replay.checkpoint, Some(commit.checkpoint));
    assert!(replay.active_model_route.is_none());
    assert_eq!(
        record
            .events
            .iter()
            .filter_map(|event| openide_core::ModelRouteSnapshot::from_event(&event.kind))
            .collect::<Vec<_>>(),
        vec![expected_route.clone(), expected_route]
    );
    assert!(replay.can_start_new_turn);
    assert!(replay.committed_messages.is_empty());
    assert!(replay
        .history
        .last()
        .unwrap()
        .content
        .contains("Receipt confirmed once"));
    assert_eq!(
        openide_core::NativeGoalStore::new(reopened_sessions.clone())
            .list(&session_id)
            .unwrap(),
        vec![paused.clone()]
    );
    let lease = reopened_sessions
        .try_acquire_native_lease(&session_id)
        .unwrap();
    let next_instructions = openide_harness::native_goal_system_instructions(
        "Exact project instructions",
        Some(&paused),
    )
    .unwrap();
    let admission = reopened_sessions
        .admit_native_turn(
            &session_id,
            "after-checkpoint",
            "Inspect saved state",
            MODEL,
            TurnMode::Plan,
        )
        .unwrap();
    let runner = AgentRunner::new(backend, tools.clone())
        .with_event_journal(reopened_sessions.clone())
        .with_native_runtime_identity("fixture", &endpoint)
        .with_system_instructions(next_instructions)
        .with_limits(LIMITS);
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session_id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.clone(),
            history: replay.history,
            prompt: "Inspect saved state".into(),
            mode: TurnMode::Plan,
        },
        lease,
    );
    let events = collect_run(&handle);
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(AgentEventKind::TurnCompleted { .. })
    ));
    assert_eq!(
        openide_core::NativeGoalStore::new(reopened_sessions)
            .list(&session_id)
            .unwrap(),
        vec![paused]
    );
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("effects.log")).unwrap(),
        "applied\n"
    );
    server.join().unwrap();
}

#[test]
fn streamed_http_model_executes_local_tool_and_replays_durable_turn() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "A confirmed local note").unwrap();
    let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
    let session = sessions.create(workspace.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "turn-http-tool",
            "Read note.txt and report its content",
            "fixture-model",
            TurnMode::Build,
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (first, _) = listener.accept().unwrap();
        let request = read_request(&first);
        assert_eq!(request["stream"], true);
        assert_eq!(request["model"], "fixture-model");
        assert!(request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "read_file"));
        respond_sse(
            first,
            &[
                json!({"id":"response-tool","choices":[{"delta":{"tool_calls":[{
                    "index":0,
                    "id":"call-read",
                    "function":{"name":"read_file","arguments":"{\"path\":\"note.txt\"}"}
                }]}}]}),
                json!({"id":"response-tool","choices":[{"finish_reason":"tool_calls"}]}),
                json!({"id":"response-tool","choices":[],"usage":{"prompt_tokens":21,"completion_tokens":5,"total_tokens":26}}),
            ],
        );

        let (second, _) = listener.accept().unwrap();
        let request = read_request(&second);
        assert!(request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["tool_call_id"] == "call-read"
                    && message["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("A confirmed local note"))
            }));
        respond_sse(
            second,
            &[
                json!({"id":"response-final","choices":[{"delta":{"content":"Done"}}]}),
                json!({"id":"response-final","choices":[],"usage":{"prompt_tokens":28,"completion_tokens":2,"total_tokens":30}}),
            ],
        );
    });

    let backend = Arc::new(
        OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "fixture-model")
            .with_capabilities(fixture_capabilities())
            .unwrap(),
    );
    let runner = AgentRunner::new(backend, Arc::new(LocalTools::new()))
        .with_native_runtime_identity("fixture", format!("http://{address}/v1"))
        .with_event_journal(sessions.clone());
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.path().to_path_buf(),
            history: vec![],
            prompt: "Read note.txt and report its content".into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    let terminal = loop {
        let event = handle
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("native runner terminal event");
        if matches!(
            event.kind,
            AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled
        ) {
            break event;
        }
    };
    assert!(matches!(
        terminal.kind,
        AgentEventKind::TurnCompleted { ref response, steps: 2 } if response == "Done"
    ));
    server.join().unwrap();
    let record = sessions.load(&session.id).unwrap().unwrap();
    assert_eq!(record.events.last().unwrap().sequence, terminal.sequence);
    assert!(record.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::ToolFinished { tool_call_id, is_error: false, output }
            if tool_call_id == "call-read" && output.contains("A confirmed local note")
    )));
    assert!(record.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::ModelResponseCommitted { response_id: Some(id), usage: Some(usage), .. }
            if id == "response-tool" && usage.input_tokens == 21
    )));
    let replay = replay_native(
        &session.id,
        &record.events,
        None,
        &CheckpointCompatibility {
            provider: "fixture".into(),
            model: "fixture-model".into(),
            context_format_key: "chat-v1".into(),
        },
    )
    .unwrap();
    assert!(replay.can_start_new_turn);
    assert_eq!(replay.history.last().unwrap().content, "Done");
}

#[test]
fn truncated_stream_does_not_execute_a_complete_looking_tool_call() {
    let workspace = tempfile::tempdir().unwrap();
    let sessions = Arc::new(SessionStore::open(workspace.path().join("sessions")).unwrap());
    let session = sessions.create(workspace.path(), "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "turn-truncated-tool",
            "Read a file",
            "fixture-model",
            TurnMode::Build,
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let _request = read_request(&socket);
        let event = json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,
            "id":"call-looks-complete",
            "function":{"name":"write_file","arguments":"{\"path\":\"created.txt\",\"content\":\"unsafe\"}"}
        }]}}]});
        let body = format!("data: {event}\n\n");
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        socket.flush().unwrap();
    });
    let backend = Arc::new(
        OpenAiCompatibleBackend::new(format!("http://{address}/v1"), None, "fixture-model")
            .with_capabilities(fixture_capabilities())
            .unwrap(),
    );
    let runner = AgentRunner::new(backend, Arc::new(LocalTools::new()))
        .with_native_runtime_identity("fixture", format!("http://{address}/v1"))
        .with_event_journal(sessions.clone());
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.path().to_path_buf(),
            history: vec![],
            prompt: "Read a file".into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    let terminal = loop {
        let event = handle
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("native runner terminal event");
        if matches!(
            event.kind,
            AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled
        ) {
            break event;
        }
    };
    server.join().unwrap();
    assert!(matches!(terminal.kind, AgentEventKind::TurnFailed { .. }));
    let record = sessions.load(&session.id).unwrap().unwrap();
    assert!(!record.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::ToolStarted { .. } | AgentEventKind::ToolFinished { .. }
    )));
    assert!(!workspace.path().join("created.txt").exists());
}

// Run this fixture only in a separate test process: exit skips Rust destructors,
// including the session lease and worker runtime, unlike an injected error.
const PROCESS_CUT_EXIT: i32 = 86;

struct ProcessCutJournal {
    store: Arc<SessionStore>,
    point: String,
}

impl NativeEventJournal for ProcessCutJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if self.point == "before_response_commit"
            && matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. })
        {
            std::process::exit(PROCESS_CUT_EXIT);
        }
        self.store.append_native_event(event)?;
        let point = match &event.kind {
            AgentEventKind::TurnStarted { .. } => "turn_started",
            AgentEventKind::SystemInstructionsCommitted { .. } => "instructions_committed",
            AgentEventKind::ModelContextCommitted { .. } => "context_committed",
            AgentEventKind::ModelRouteCommitted { .. } => "route_committed",
            AgentEventKind::ModelRequestStarted { .. } => "model_started",
            AgentEventKind::ModelResponseCommitted { .. } => "response_committed",
            AgentEventKind::PermissionRequested { .. } => "permission_requested",
            AgentEventKind::PermissionResolved { .. } => "permission_resolved",
            AgentEventKind::ToolStarted { .. } => "tool_started",
            AgentEventKind::ToolFinished { .. } => "tool_finished",
            AgentEventKind::HookStarted { phase, .. } if phase == "pre_tool_use" => {
                "pre_hook_started"
            }
            AgentEventKind::HookFinished { phase, .. } if phase == "pre_tool_use" => {
                "pre_hook_finished"
            }
            AgentEventKind::HookStarted { phase, .. } if phase == "post_tool_use" => {
                "post_hook_started"
            }
            AgentEventKind::HookFinished { phase, .. } if phase == "post_tool_use" => {
                "post_hook_finished"
            }
            AgentEventKind::HookDiagnosticCommitted { phase, .. } if phase == "pre_tool_use" => {
                "pre_hook_diagnostic"
            }
            AgentEventKind::HookDiagnosticCommitted { phase, .. } if phase == "post_tool_use" => {
                "post_hook_diagnostic"
            }
            AgentEventKind::TurnCompleted { .. } => "turn_completed",
            _ => "",
        };
        if self.point == point {
            std::process::exit(PROCESS_CUT_EXIT);
        }
        Ok(())
    }
}

struct ProcessReceiptTool {
    cut_after_effect: bool,
}

#[async_trait]
impl ToolExecutor for ProcessReceiptTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        UncertainReceiptTool {
            calls: AtomicUsize::new(0),
        }
        .definitions()
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        assert_eq!(call.name, "append_receipt");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(context.workspace.join("effects.log"))
            .unwrap();
        writeln!(file, "applied").unwrap();
        file.sync_all().unwrap();
        if self.cut_after_effect {
            std::process::exit(PROCESS_CUT_EXIT);
        }
        Ok(ToolOutput::success("receipt verified on disk"))
    }
}

const PROCESS_GOAL: &str = "Append exactly one receipt and retain the complete recovery goal";
const PROCESS_CRITERION: &str = "Verify a single effect after process exit and checkpoint reopen";

fn process_goal_instructions(sessions: Arc<SessionStore>, session_id: &str) -> String {
    let goal = openide_core::NativeGoalStore::new(sessions)
        .list(session_id)
        .unwrap()
        .pop()
        .expect("process acceptance goal exists");
    openide_harness::native_goal_system_instructions("Process project instructions", Some(&goal))
        .unwrap()
}

fn process_backend(endpoint: &str) -> Arc<dyn ModelBackend> {
    Arc::new(
        OpenAiCompatibleBackend::new(endpoint, None, "fixture-model")
            .with_capabilities(fixture_capabilities())
            .unwrap()
            .with_timeout(Duration::from_secs(10)),
    )
}

fn process_compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "fixture".into(),
        model: "fixture-model".into(),
        context_format_key: "chat-v1".into(),
    }
}

fn process_policy() -> PermissionPolicy {
    PermissionPolicy::new().with_tool("append_receipt", openide_harness::PermissionMode::Ask)
}

#[cfg(unix)]
#[test]
#[ignore = "hook subprocess fixture, only invoked by the process-cut acceptance"]
fn native_hook_receipt_child() {
    let Ok(root) = std::env::var("OPENIDE_HOOK_RECEIPT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let invocation: Value = serde_json::from_str(&input).unwrap();
    let phase = invocation["phase"].as_str().unwrap();
    assert!(matches!(phase, "pre_tool_use" | "post_tool_use"));
    assert_eq!(invocation["tool_name"], "append_receipt");
    let mut receipt = OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("workspace").join(format!("{phase}.log")))
        .unwrap();
    writeln!(receipt, "hook-applied").unwrap();
    receipt.sync_all().unwrap();
    let point = std::fs::read_to_string(root.join("hook-cut-point")).unwrap();
    if (phase == "pre_tool_use" && point == "pre_hook_effect_synced")
        || (phase == "post_tool_use" && point == "post_hook_effect_synced")
    {
        let pid: i32 = std::fs::read_to_string(root.join("runner.pid"))
            .unwrap()
            .parse()
            .unwrap();
        // The test helper kills only the runner PID written by its parent,
        // after its receipt has reached the filesystem sync boundary.
        assert!(pid > 1);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    }
}

fn process_hooks(root: &std::path::Path, point: &str) -> Arc<openide_harness::CommandHooks> {
    #[cfg(unix)]
    if point.contains("hook_") {
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
        let script = format!(
            "OPENIDE_HOOK_RECEIPT_ROOT={} {} --exact native_hook_receipt_child --ignored --nocapture >/dev/null && printf '%s' 'hook stderr' >&2 && printf '%s' '{{\"decision\":\"allow\",\"reason\":\"fixture-approved\"}}'",
            quote(&root.to_string_lossy()),
            quote(&std::env::current_exe().unwrap().to_string_lossy())
        );
        return Arc::new(
            openide_harness::CommandHooks::new(
                [
                    openide_harness::HookPhase::PreToolUse,
                    openide_harness::HookPhase::PostToolUse,
                ]
                .into_iter()
                .enumerate()
                .map(|(index, phase)| openide_harness::CommandHookConfig {
                    id: format!("process-hook-{index}"),
                    phase,
                    tool_name: "append_receipt".into(),
                    executable: std::path::PathBuf::from("/bin/sh"),
                    args: vec!["-c".into(), script.clone()],
                    cwd: root.join("workspace"),
                    timeout_ms: 15000,
                    max_output_bytes: 4096,
                })
                .collect(),
            )
            .unwrap(),
        );
    }
    let _ = (root, point);
    Arc::new(openide_harness::CommandHooks::default())
}

#[test]
#[ignore = "fixture invoked only by the process interruption acceptance test"]
fn native_process_cut_child() {
    let Ok(root) = std::env::var("OPENIDE_PROCESS_CUT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let session_id = std::env::var("OPENIDE_PROCESS_CUT_SESSION").unwrap();
    let endpoint = std::env::var("OPENIDE_PROCESS_CUT_ENDPOINT").unwrap();
    let point = std::env::var("OPENIDE_PROCESS_CUT_POINT").unwrap();
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    if point == "admitted" {
        std::process::exit(PROCESS_CUT_EXIT);
    }
    let record = sessions.load(&session_id).unwrap().unwrap();
    // The already admitted Build prompt is added by AgentRunner, not history.
    let admission = record.events.last().unwrap();
    let plan = replay_native(
        &session_id,
        &record.events[..record.events.len() - 1],
        None,
        &process_compatibility(),
    )
    .unwrap();
    #[cfg(unix)]
    std::fs::write(root.join("runner.pid"), std::process::id().to_string()).unwrap();
    let hooks = process_hooks(&root, &point);
    let instructions = process_goal_instructions(sessions.clone(), &session_id);
    let runner = AgentRunner::new(
        process_backend(&endpoint),
        Arc::new(ProcessReceiptTool {
            cut_after_effect: point == "effect_synced",
        }),
    )
    .with_system_instructions(instructions)
    .with_native_runtime_identity("fixture", &endpoint)
    .with_permission_policy(process_policy())
    .with_plan_snapshot(plan.latest_plan)
    .with_command_hooks(hooks)
    .with_event_journal(Arc::new(ProcessCutJournal {
        store: sessions,
        point,
    }));
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session_id),
            sequence_start: admission.sequence,
            workspace: root.join("workspace"),
            history: plan.history,
            prompt: "Append the receipt exactly once".into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    while let Ok(event) = handle.events.recv_timeout(Duration::from_secs(10)) {
        if let AgentEventKind::PermissionRequested { request_id, .. } = event.kind {
            assert!(
                handle.resolve_permission(&request_id, openide_harness::PermissionChoice::Allow)
            );
        }
    }
    panic!("child did not reach its requested interruption point");
}

#[test]
fn process_cuts_reopen_plan_build_resume_and_checkpoint_without_repeating_effects() {
    #[cfg_attr(not(unix), allow(unused_mut))] // Unix adds hook process boundaries.
    let mut points = vec![
        "admitted",
        "turn_started",
        "instructions_committed",
        "context_committed",
        "route_committed",
        "model_started",
        "before_response_commit",
        "response_committed",
        "permission_requested",
        "permission_resolved",
        "tool_started",
        "effect_synced",
        "tool_finished",
        "turn_completed",
    ];
    #[cfg(unix)]
    points.extend([
        "pre_hook_started",
        "pre_hook_effect_synced",
        "pre_hook_finished",
        "pre_hook_diagnostic",
        "post_hook_started",
        "post_hook_effect_synced",
        "post_hook_finished",
        "post_hook_diagnostic",
    ]);
    for point in points {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(root.path().join("hook-cut-point"), point).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server_stop = stop.clone();
        let server = std::thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("HTTP fixture: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_request(&socket);
                if request["stream"] == false {
                    assert!(request.get("tools").is_none());
                    let summary = json!({"objective":"Append one receipt", "decisions":["Plan before Build"],
                        "files":[{"path":"effects.log","state":"one receipt verified"}],
                        "current_state":"Receipt confirmed once", "constraints":["Never repeat uncertain effects"],
                        "remaining_work":[]});
                    respond_json(
                        socket,
                        json!({"choices":[{"message":{"role":"assistant", "content":summary.to_string()},"finish_reason":"stop"}]}),
                    );
                } else {
                    let system = request["messages"][0]["content"].as_str().unwrap();
                    assert!(system.contains(PROCESS_GOAL));
                    assert!(system.contains(PROCESS_CRITERION));
                    assert!(system.contains("\"status\":\"active\""));
                    if request.get("tools").is_none() {
                        respond_sse(
                            socket,
                            &[
                                json!({"id":"plan", "choices":[{"delta":{"content":"First record the receipt."},"finish_reason":"stop"}]}),
                            ],
                        );
                    } else if request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| message["role"] == "tool")
                    {
                        respond_sse(
                            socket,
                            &[
                                json!({"id":"finished", "choices":[{"delta":{"content":"Receipt confirmed once."},"finish_reason":"stop"}]}),
                            ],
                        );
                    } else {
                        respond_sse(
                            socket,
                            &[
                                json!({"id":"receipt-response", "choices":[{"delta":{"tool_calls":[{"index":0,"id":"receipt-call",
                            "function":{"name":"append_receipt","arguments":"{\"label\":\"applied\"}"}}]}}]}),
                                json!({"id":"receipt-response", "choices":[{"finish_reason":"tool_calls"}]}),
                            ],
                        );
                    }
                }
            }
        });
        let sessions_root = root.path().join("sessions");
        let session_id = {
            let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
            let session = sessions.create(&workspace, "native").unwrap();
            let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
            openide_core::NativeGoalStore::new(sessions.clone())
                .create(
                    &lease,
                    "process-goal",
                    PROCESS_GOAL,
                    vec![openide_core::NativeGoalCriterion {
                        id: "recovery".into(),
                        description: PROCESS_CRITERION.into(),
                        evidence: None,
                    }],
                )
                .unwrap();
            let instructions = process_goal_instructions(sessions.clone(), &session.id);
            let admission = sessions
                .admit_native_turn(
                    &session.id,
                    "plan",
                    "Plan one receipt operation",
                    "fixture-model",
                    TurnMode::Plan,
                )
                .unwrap();
            let runner = AgentRunner::new(
                process_backend(&endpoint),
                Arc::new(ProcessReceiptTool {
                    cut_after_effect: false,
                }),
            )
            .with_system_instructions(instructions)
            .with_native_runtime_identity("fixture", &endpoint)
            .with_event_journal(sessions.clone());
            let events = collect_run(&runner.start_with_lease(
                AgentRunRequest {
                    session_id: Some(session.id.clone()),
                    sequence_start: admission.sequence,
                    workspace: workspace.clone(),
                    history: vec![],
                    prompt: "Plan one receipt operation".into(),
                    mode: TurnMode::Plan,
                },
                lease,
            ));
            assert!(matches!(
                events.last().unwrap().kind,
                AgentEventKind::TurnCompleted { .. }
            ));
            // collect_run waits for channel closure, releasing the worker lease.
            sessions
                .admit_native_turn(
                    &session.id,
                    "build",
                    "Append the receipt exactly once",
                    "fixture-model",
                    TurnMode::Build,
                )
                .unwrap();
            session.id
        };
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_process_cut_child",
                "--ignored",
                "--nocapture",
            ])
            .env("OPENIDE_PROCESS_CUT_ROOT", root.path())
            .env("OPENIDE_PROCESS_CUT_SESSION", &session_id)
            .env("OPENIDE_PROCESS_CUT_ENDPOINT", &endpoint)
            .env("OPENIDE_PROCESS_CUT_POINT", point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child timeout at {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        #[cfg(unix)]
        if point.ends_with("hook_effect_synced") {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL), "cut at {point}");
        } else {
            assert_eq!(status.code(), Some(PROCESS_CUT_EXIT), "cut at {point}");
        }
        #[cfg(not(unix))]
        assert_eq!(status.code(), Some(PROCESS_CUT_EXIT), "cut at {point}");
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let checkpoints_root = root.path().join("checkpoints");
        let checkpoints = Arc::new(CheckpointFileStore::new(&checkpoints_root).unwrap());
        let lease = sessions
            .try_acquire_native_lease(&session_id)
            .expect("process exit releases lease");
        let tools = Arc::new(ProcessReceiptTool {
            cut_after_effect: false,
        });
        let engine = ResumeEngine::new(
            sessions.clone(),
            checkpoints.clone(),
            process_compatibility(),
            process_backend(&endpoint),
            tools.clone(),
            ToolCatalog::new(tools.definitions()).unwrap(),
            process_policy(),
            None,
        )
        .with_system_instructions(process_goal_instructions(sessions.clone(), &session_id))
        .with_native_runtime_identity("fixture", &endpoint)
        .with_command_hooks(process_hooks(root.path(), point));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let initial = engine.inspect(&lease).unwrap();
        let cut_route = if point == "route_committed" {
            let events = sessions.load(&session_id).unwrap().unwrap().events;
            let route = openide_core::ModelRouteSnapshot::new(
                "fixture",
                "fixture-model",
                &endpoint,
                process_backend(&endpoint)
                    .request_profile_fingerprint()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(initial.active_model_route.as_ref(), Some(&route));
            assert_eq!(
                openide_core::ModelRouteSnapshot::from_event(&events.last().unwrap().kind),
                Some(route.clone())
            );
            assert_eq!(initial.active_turn.as_ref().unwrap().last_step, 0);
            Some(route)
        } else {
            None
        };
        if point.contains("hook_") {
            let events = sessions.load(&session_id).unwrap().unwrap().events;
            let phase = if point.starts_with("pre_") {
                "pre_tool_use"
            } else {
                "post_tool_use"
            };
            let started = events.iter().filter(|event| matches!(&event.kind, AgentEventKind::HookStarted { phase: recorded, .. } if recorded == phase)).count();
            let finished = events.iter().filter(|event| matches!(&event.kind, AgentEventKind::HookFinished { phase: recorded, .. } if recorded == phase)).count();
            assert_eq!(started, 1, "hook start boundary at {point}");
            let diagnostics: Vec<_> = events
                .iter()
                .filter_map(|event| match &event.kind {
                    AgentEventKind::HookDiagnosticCommitted {
                        phase: recorded,
                        reason,
                        stdout,
                        stderr,
                        error,
                        ..
                    } if recorded == phase => Some((reason, stdout, stderr, error)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                diagnostics.len(),
                usize::from(point.ends_with("hook_diagnostic") || point.ends_with("hook_finished")),
                "diagnostics boundary at {point}"
            );
            if let Some((reason, stdout, stderr, error)) = diagnostics.first() {
                assert_eq!(reason.as_deref(), Some("fixture-approved"));
                assert_eq!(
                    stdout.as_deref(),
                    Some("{\"decision\":\"allow\",\"reason\":\"fixture-approved\"}")
                );
                assert_eq!(stderr.as_deref(), Some("hook stderr"));
                assert!(error.is_none());
            }
            assert_eq!(
                finished,
                usize::from(point.ends_with("hook_finished")),
                "hook result boundary at {point}"
            );
        }
        let receipt = workspace.join("effects.log");
        match point {
            "admitted" => assert_eq!(initial.recovery, ReplayRecovery::AdmittedNotStarted),
            "turn_started" | "instructions_committed" | "context_committed" | "route_committed" => {
                assert!(matches!(
                    initial.recovery,
                    ReplayRecovery::ReadyForModel { .. }
                ))
            }
            "model_started" | "before_response_commit" => assert!(matches!(
                initial.recovery,
                ReplayRecovery::ModelInFlight { .. }
            )),
            "response_committed" | "permission_resolved" => {
                assert!(matches!(initial.recovery, ReplayRecovery::ToolReady { .. }))
            }
            "permission_requested" => assert!(matches!(
                initial.recovery,
                ReplayRecovery::PermissionPending { .. }
            )),
            "tool_started"
            | "effect_synced"
            | "pre_hook_started"
            | "pre_hook_effect_synced"
            | "pre_hook_finished"
            | "pre_hook_diagnostic"
            | "post_hook_started"
            | "post_hook_effect_synced"
            | "post_hook_finished"
            | "post_hook_diagnostic" => assert!(matches!(
                initial.recovery,
                ReplayRecovery::ToolOutcomeUncertain { .. }
            )),
            "tool_finished" => assert!(matches!(
                initial.recovery,
                ReplayRecovery::ReadyForModel { .. }
            )),
            "turn_completed" => assert!(initial.can_start_new_turn),
            _ => unreachable!(),
        }
        let hook_receipts: Vec<_> = ["pre_tool_use", "post_tool_use"]
            .into_iter()
            .map(|phase| {
                let path = workspace.join(format!("{phase}.log"));
                let content = std::fs::read_to_string(&path).ok();
                if let Some(content) = &content {
                    assert_eq!(content, "hook-applied\n", "{phase} at {point}");
                }
                (path, content)
            })
            .collect();
        if point == "pre_hook_started" {
            assert!(hook_receipts[0].1.is_none());
        }
        if point.contains("hook_") && point != "pre_hook_started" {
            assert!(hook_receipts[0].1.is_some());
        }
        if matches!(
            point,
            "post_hook_effect_synced" | "post_hook_finished" | "post_hook_diagnostic"
        ) {
            assert!(hook_receipts[1].1.is_some());
        }
        if matches!(
            initial.recovery,
            ReplayRecovery::ToolOutcomeUncertain { .. }
        ) {
            let blocked = runtime
                .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
                .unwrap();
            assert!(matches!(
                blocked.disposition,
                ResumeDisposition::ToolOutcomeUncertain { .. }
            ));
            assert!(blocked.appended_events.is_empty());
            if matches!(
                point,
                "tool_started"
                    | "pre_hook_started"
                    | "pre_hook_effect_synced"
                    | "pre_hook_finished"
                    | "pre_hook_diagnostic"
            ) {
                assert!(!receipt.exists());
                // The operator verifies absence and performs the intended effect
                // explicitly; Resume itself must never execute this call again.
                let file = std::fs::File::create(&receipt).unwrap();
                let mut file = file;
                writeln!(file, "applied").unwrap();
                file.sync_all().unwrap();
            }
            assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "applied\n");
            engine
                .record_verified_tool_outcome(
                    &lease,
                    "receipt-call",
                    ToolOutput::success("receipt verified on disk"),
                )
                .unwrap();
        }
        loop {
            let step = runtime
                .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
                .unwrap();
            match step.disposition {
                ResumeDisposition::NeedsPermission { request_id, .. } => {
                    engine
                        .resolve_permission(
                            &lease,
                            &request_id,
                            openide_harness::PermissionChoice::Allow,
                        )
                        .unwrap();
                }
                ResumeDisposition::Complete => break,
                other => panic!("unexpected recovery at {point}: {other:?}"),
            }
        }
        assert_eq!(
            std::fs::read_to_string(&receipt).unwrap(),
            "applied\n",
            "effect at {point}"
        );
        let record = sessions.load(&session_id).unwrap().unwrap();
        assert_eq!(
            record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                .count(),
            1,
            "start at {point}"
        );
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            process_backend(&endpoint),
            process_compatibility(),
            CheckpointBudgetSnapshot::unverified(1_000, None),
        );
        let committed = runtime
            .block_on(coordinator.compact(&lease, CancellationToken::new()))
            .unwrap();
        drop(coordinator);
        for (path, before) in &hook_receipts {
            assert_eq!(
                &std::fs::read_to_string(path).ok(),
                before,
                "Resume repeated a hook at {point}"
            );
        }
        drop(engine);
        drop(lease);
        drop(sessions);
        drop(checkpoints);
        let reopened = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let reopened_checkpoints = CheckpointFileStore::new(&checkpoints_root).unwrap();
        let record = reopened.load(&session_id).unwrap().unwrap();
        if let Some(route) = cut_route {
            assert_eq!(
                record
                    .events
                    .iter()
                    .filter_map(|event| openide_core::ModelRouteSnapshot::from_event(&event.kind))
                    .collect::<Vec<_>>(),
                vec![route.clone(), route],
                "Plan and recovered Build keep one original route each after checkpoint reopen"
            );
        }
        let replay = replay_native(
            &session_id,
            &record.events,
            Some(&reopened_checkpoints),
            &process_compatibility(),
        )
        .unwrap();
        let goals = openide_core::NativeGoalStore::new(reopened.clone())
            .list(&session_id)
            .unwrap();
        assert_eq!(goals.len(), 1, "goal count at {point}");
        assert_eq!(goals[0].objective, PROCESS_GOAL);
        assert_eq!(goals[0].criteria[0].description, PROCESS_CRITERION);
        assert_eq!(
            goals[0].revision, 0,
            "recovery must not change goal at {point}"
        );
        assert!(goals[0].criteria[0].evidence.is_none());
        let instructions = process_goal_instructions(reopened.clone(), &session_id);
        let snapshots: Vec<_> = record
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::SystemInstructionsCommitted { content, .. } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(snapshots.len(), 2, "Plan and Build snapshots at {point}");
        assert!(snapshots.iter().all(|content| **content == instructions));
        assert!(replay.can_start_new_turn);
        assert_eq!(replay.checkpoint, Some(committed.checkpoint));
        assert!(replay.latest_plan.is_some(), "plan preserved at {point}");
        // Exercise the next model request after reopening the checkpoint as well:
        // the summary is history, while the complete goal remains instructions.
        let lease = reopened.try_acquire_native_lease(&session_id).unwrap();
        let admission = reopened
            .admit_native_turn(
                &session_id,
                "inspect-after-checkpoint",
                "Inspect saved scope without performing another effect",
                "fixture-model",
                TurnMode::Plan,
            )
            .unwrap();
        let runner = AgentRunner::new(
            process_backend(&endpoint),
            Arc::new(ProcessReceiptTool {
                cut_after_effect: false,
            }),
        )
        .with_system_instructions(instructions)
        .with_native_runtime_identity("fixture", &endpoint)
        .with_event_journal(reopened.clone());
        let events = collect_run(&runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session_id.clone()),
                sequence_start: admission.sequence,
                workspace: workspace.clone(),
                history: replay.history,
                prompt: "Inspect saved scope without performing another effect".into(),
                mode: TurnMode::Plan,
            },
            lease,
        ));
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ));
        assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "applied\n");
        assert_eq!(
            openide_core::NativeGoalStore::new(reopened.clone())
                .list(&session_id)
                .unwrap(),
            goals
        );
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }
}
