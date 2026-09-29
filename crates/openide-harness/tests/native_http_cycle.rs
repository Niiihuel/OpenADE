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
    });

    let endpoint = format!("http://{address}/v1");
    let backend: Arc<dyn ModelBackend> = Arc::new(
        OpenAiCompatibleBackend::new(&endpoint, None, MODEL)
            .with_capabilities(fixture_capabilities())
            .unwrap()
            .with_timeout(Duration::from_secs(10)),
    );
    let compatibility = CheckpointCompatibility {
        provider: "fixture".into(),
        model: MODEL.into(),
        context_format_key: "chat-v1".into(),
    };
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
        let admission = sessions
            .admit_native_turn(&session.id, "plan", PLAN_PROMPT, MODEL, TurnMode::Plan)
            .unwrap();
        let runner = AgentRunner::new(backend.clone(), tools.clone())
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
    let runner = AgentRunner::new(backend.clone(), tools.clone())
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
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    let catalog = ToolCatalog::new(tools.definitions()).unwrap();
    let engine = ResumeEngine::new(
        sessions.clone(),
        checkpoints.clone(),
        compatibility.clone(),
        backend.clone(),
        tools.clone(),
        catalog,
        policy,
        None,
    )
    .with_native_runtime_identity("fixture", &endpoint)
    .with_limits(LIMITS);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
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
        backend,
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

    let reopened_sessions = SessionStore::open(&sessions_root).unwrap();
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
    assert!(replay.can_start_new_turn);
    assert!(replay.committed_messages.is_empty());
    assert!(replay
        .history
        .last()
        .unwrap()
        .content
        .contains("Receipt confirmed once"));
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
