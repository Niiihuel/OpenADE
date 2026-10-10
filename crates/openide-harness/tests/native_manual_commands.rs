//! Real local HTTP and pipe acceptance for human controls during a native turn.
#![cfg(unix)]

use async_trait::async_trait;
use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentRunRequest, CheckpointCompatibility,
    CheckpointFileStore, NativeCommandOperationAction, NativeCommandOperationState,
    NativeCommandOperationStore, NativeEventJournal, ReplayRecovery, SessionStore, StoreError,
    ToolCall, TurnMode,
};
use openide_harness::{
    AgentError, AgentRunHandle, AgentRunner, CommandHooks, CommandIsolation, ModelBackend,
    ModelRequestCapabilities, NativeManualCommandActor, NativeManualCommandPolicy,
    OpenAiCompatibleBackend, PermissionPolicy, PersistentCommandEvent, PersistentCommandLimits,
    PersistentCommandManager, ResumeDisposition, ResumeEngine, ToolCatalog, ToolContext,
    ToolDefinition, ToolExecutor, ToolOutput,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "exact-local-manual-model";
const PROMPT: &str = "Report the approved process control separately from model tools";
const FIRST: &str = "Waiting for the human control 🌱";
const FINAL: &str = "Native HTTP response complete.";

struct NoTools;

#[async_trait]
impl ToolExecutor for NoTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn execute(&self, _: &ToolCall, _: &ToolContext) -> Result<ToolOutput, AgentError> {
        panic!("manual controls must never be executed as model tools")
    }
}

struct Fixture {
    _command_runtime: CommandRuntime,
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    sessions_root: PathBuf,
    checkpoints_root: PathBuf,
    sessions: Arc<SessionStore>,
    owner: String,
    manager: Arc<PersistentCommandManager>,
    actor: Arc<NativeManualCommandActor>,
    command_id: String,
}

/// Keep the command manager's reactor alive independently of the blocking
/// desktop event receiver. The crate deliberately uses current-thread Tokio.
struct CommandRuntime {
    stop: CancellationToken,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CommandRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn policy() -> NativeManualCommandPolicy {
    NativeManualCommandPolicy {
        permissions: PermissionPolicy::new(),
        run_command_enabled: true,
        isolation: CommandIsolation::HostUnrestricted,
        hooks: Arc::new(CommandHooks::default()),
    }
}

fn actor(
    sessions: Arc<SessionStore>,
    owner: &str,
    manager: Option<Arc<PersistentCommandManager>>,
) -> Arc<NativeManualCommandActor> {
    Arc::new(
        NativeManualCommandActor::new(sessions, owner.into(), manager, Arc::new(|| Ok(policy())))
            .unwrap(),
    )
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions_root = dir.path().join("sessions");
        let checkpoints_root = dir.path().join("checkpoints");
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        let stop = CancellationToken::new();
        let worker_stop = stop.clone();
        let command_workspace = workspace.clone();
        let command_owner = owner.clone();
        let (ready, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
                let manager = Arc::new(PersistentCommandManager::new(
                    command_workspace, CommandIsolation::HostUnrestricted, PersistentCommandLimits::default(),
                ).unwrap().with_native_session_owner(command_owner).unwrap());
                // A genuine pipe process records each line once, and EOF is observable.
                let command_id = manager.start("/bin/sh", &[
            "-c".into(),
            "printf 'READY\n'; while IFS= read -r line; do printf '%s\n' \"$line\" >> effects.log; printf 'ECHO:%s\n' \"$line\"; done; printf 'EOF\n'".into(),
        ], &CancellationToken::new()).unwrap().id;
                wait_output(&manager, &command_id, "READY\n").await;
                ready.send((manager.clone(), command_id)).unwrap();
                worker_stop.cancelled().await;
                manager.stop_all();
                tokio::time::sleep(Duration::from_millis(30)).await;
            });
        });
        let (manager, command_id) = received.recv_timeout(Duration::from_secs(8)).unwrap();
        let actor = actor(sessions.clone(), &owner, Some(manager.clone()));
        Self {
            _command_runtime: CommandRuntime {
                stop,
                worker: Some(worker),
            },
            _dir: dir,
            workspace,
            sessions_root,
            checkpoints_root,
            sessions,
            owner,
            manager,
            actor,
            command_id,
        }
    }

    fn request(&self) -> AgentRunRequest {
        let admitted = self
            .sessions
            .admit_native_turn(&self.owner, "manual-turn", PROMPT, MODEL, TurnMode::Build)
            .unwrap();
        AgentRunRequest {
            session_id: Some(self.owner.clone()),
            sequence_start: admitted.sequence,
            workspace: self.workspace.clone(),
            history: Vec::new(),
            prompt: PROMPT.into(),
            mode: TurnMode::Build,
        }
    }

    fn stdin(&self, text: &str) -> openide_core::NativeCommandOperationRequest {
        self.actor
            .prepare_request(
                &self.command_id,
                NativeCommandOperationAction::Stdin {
                    input: text.into(),
                    close_after: true,
                },
            )
            .unwrap()
    }
}

async fn wait_output(manager: &PersistentCommandManager, id: &str, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let copy = manager.inspect_pending_output(id).unwrap();
        let bytes = copy
            .events
            .iter()
            .flat_map(|event| match event {
                PersistentCommandEvent::Output { bytes, .. } => bytes.clone(),
                PersistentCommandEvent::Omitted { .. } => panic!("fixture output was omitted"),
            })
            .collect::<Vec<_>>();
        if String::from_utf8(bytes).unwrap().contains(text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "process did not publish {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    (listener, endpoint)
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                socket
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                return socket;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "provider request never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    }
}

fn read_request(socket: &TcpStream) -> Value {
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["stream"], true);
    body
}

fn prefix(socket: &mut TcpStream) -> String {
    let first = format!(
        "data: {}\n\n",
        json!({"id":"manual-response","choices":[{"delta":{"content":FIRST},"finish_reason":null}]})
    );
    let tail = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"id":"manual-response","choices":[{"delta":{"content":FINAL},"finish_reason":"stop"}]})
    );
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first}", first.len()+tail.len()).unwrap();
    socket.flush().unwrap();
    tail
}

fn backend(endpoint: &str) -> Arc<dyn ModelBackend> {
    Arc::new(
        OpenAiCompatibleBackend::new(endpoint, None, MODEL)
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: true,
                ..Default::default()
            })
            .unwrap()
            .with_timeout(Duration::from_secs(8)),
    )
}

fn runner(
    model: Arc<dyn ModelBackend>,
    endpoint: &str,
    journal: Arc<dyn NativeEventJournal>,
) -> AgentRunner {
    AgentRunner::new(model, Arc::new(NoTools))
        .with_permission_policy(PermissionPolicy::new())
        .with_native_runtime_identity("local", endpoint)
        .with_event_journal(journal)
}

fn compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "local".into(),
        model: MODEL.into(),
        context_format_key: "chat-v1".into(),
    }
}

fn engine(
    fixture: &Fixture,
    sessions: Arc<SessionStore>,
    model: Arc<dyn ModelBackend>,
    endpoint: &str,
) -> ResumeEngine {
    ResumeEngine::new(
        sessions,
        Arc::new(CheckpointFileStore::new(&fixture.checkpoints_root).unwrap()),
        compatibility(),
        model,
        Arc::new(NoTools),
        ToolCatalog::new([]).unwrap(),
        PermissionPolicy::new(),
        None,
    )
    .with_native_runtime_identity("local", endpoint)
}

fn through_first(handle: &AgentRunHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let event = handle.events.recv_timeout(Duration::from_secs(8)).unwrap();
        let first =
            matches!(&event.kind, AgentEventKind::AssistantDelta { content } if content == FIRST);
        events.push(event);
        if first {
            return events;
        }
    }
}

fn rest(handle: &AgentRunHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        match handle.events.recv_timeout(Duration::from_secs(8)) {
            Ok(event) => events.push(event),
            Err(mpsc::RecvTimeoutError::Disconnected) => return events,
            Err(error) => {
                handle.cancel();
                panic!("worker did not finish: {error}");
            }
        }
    }
}

fn assert_single_response(events: &[AgentEvent]) {
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ModelRequestStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::ModelResponseCommitted { .. }))
            .count(),
        1
    );
    assert!(!events.iter().any(|e| matches!(
        e.kind,
        AgentEventKind::ToolStarted { .. } | AgentEventKind::ToolFinished { .. }
    )));
    assert!(matches!(
        events.last().unwrap().kind,
        AgentEventKind::TurnCompleted { .. }
    ));
}

#[tokio::test]
async fn manual_stdin_is_executed_once_during_real_http_stream_and_reopens_separately() {
    let fixture = Fixture::new().await;
    let (listener, endpoint) = listener();
    let effect = fixture.workspace.join("effects.log");
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        read_request(&socket);
        let tail = prefix(&mut socket);
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if std::fs::read_to_string(&effect).ok().as_deref() == Some("approved café 🌱\n") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "manual command was not processed while model streamed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        socket.write_all(tail.as_bytes()).unwrap();
        socket.flush().unwrap();
    });
    let request = fixture.request();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let handle = runner(backend(&endpoint), &endpoint, fixture.sessions.clone())
        .with_manual_command_actor(fixture.actor.clone())
        .start_with_lease(request, lease);
    let mut events = through_first(&handle);
    let request = fixture.stdin("approved café 🌱\n");
    let operation = request.operation_id.clone();
    fixture.actor.enqueue_confirmed(request).unwrap();
    events.extend(rest(&handle));
    assert_single_response(&events);
    assert_eq!(
        fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events[1..],
        events
    );
    wait_output(
        &fixture.manager,
        &fixture.command_id,
        "ECHO:approved café 🌱\nEOF\n",
    )
    .await;
    server.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effects.log")).unwrap(),
        "approved café 🌱\n"
    );
    let saved = fixture
        .actor
        .operations()
        .snapshot(&fixture.owner, &operation)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.state,
        NativeCommandOperationState::Finished {
            outcome: openide_core::NativeCommandOperationOutcome::Succeeded { .. }
        }
    ));
    assert_eq!(
        saved
            .phase_diagnostics()
            .iter()
            .filter(|phase| phase.phase == openide_core::NativeCommandOperationPhase::BeforeCommand)
            .count(),
        1
    );
    let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
    let lease = reopened.try_acquire_native_lease(&fixture.owner).unwrap();
    assert!(matches!(
        engine(&fixture, reopened.clone(), backend(&endpoint), &endpoint)
            .inspect(&lease)
            .unwrap()
            .recovery,
        ReplayRecovery::Idle
    ));
    assert_eq!(
        NativeCommandOperationStore::new(reopened)
            .snapshot(&fixture.owner, &operation)
            .unwrap(),
        Some(saved)
    );
    fixture.manager.stop_all();
}

#[tokio::test]
async fn manual_requested_control_is_cancelled_before_effect_when_stream_is_cancelled() {
    let fixture = Fixture::new().await;
    let (listener, endpoint) = listener();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        read_request(&socket);
        prefix(&mut socket);
        released.recv_timeout(Duration::from_secs(8)).unwrap();
    });
    let request = fixture.request();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let handle = runner(backend(&endpoint), &endpoint, fixture.sessions.clone())
        .with_manual_command_actor(fixture.actor.clone())
        .start_with_lease(request, lease);
    let mut events = through_first(&handle);
    let request = fixture.stdin("must not be written\n");
    let operation = request.operation_id.clone();
    // Queue durably without Notify to deterministically exercise the cancelled
    // worker's terminal boundary, rather than race an already dispatched write.
    fixture
        .actor
        .operations()
        .enqueue_requested(request)
        .unwrap();
    handle.cancel();
    events.extend(rest(&handle));
    release.send(()).unwrap();
    server.join().unwrap();
    assert!(matches!(
        events.last().unwrap().kind,
        AgentEventKind::TurnCancelled
    ));
    assert!(events.iter().any(
        |e| matches!(&e.kind, AgentEventKind::AssistantDelta { content } if content == FIRST)
    ));
    assert!(!events
        .iter()
        .any(|e| matches!(e.kind, AgentEventKind::ModelResponseCommitted { .. })));
    assert!(!fixture.workspace.join("effects.log").exists());
    assert!(fixture
        .manager
        .control_snapshot(&fixture.command_id)
        .unwrap()
        .stdin_open());
    let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
    let saved = NativeCommandOperationStore::new(reopened)
        .snapshot(&fixture.owner, &operation)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.state,
        NativeCommandOperationState::Cancelled { .. }
    ));
    fixture.manager.stop_all();
}

struct DropTerminalJournal(Arc<SessionStore>);
impl NativeEventJournal for DropTerminalJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        if matches!(
            event.kind,
            AgentEventKind::TurnCancelled | AgentEventKind::TurnFailed { .. }
        ) {
            return Err(StoreError::Io(std::io::Error::other(
                "fixture terminal append cut",
            )));
        }
        self.0.append_native_event(event)
    }
}

async fn interrupted_stream(fixture: &Fixture, model: Arc<dyn ModelBackend>, endpoint: &str) {
    let request = fixture.request();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let handle = runner(
        model,
        endpoint,
        Arc::new(DropTerminalJournal(fixture.sessions.clone())),
    )
    .start_with_lease(request, lease);
    through_first(&handle);
    handle.cancel();
    rest(&handle);
    assert!(handle.journal_error().is_some());
}

#[tokio::test]
async fn manual_resume_rejects_reopened_process_and_does_not_retry_a_started_control() {
    let fixture = Fixture::new().await;
    let (listener, endpoint) = listener();
    let (advance, advanced) = mpsc::channel();
    let (received, observed) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut first = accept(&listener);
        read_request(&first);
        prefix(&mut first);
        advanced.recv_timeout(Duration::from_secs(8)).unwrap();
        drop(first);
        let mut resumed = accept(&listener);
        let body = read_request(&resumed);
        received.send(body).unwrap();
        let tail = prefix(&mut resumed);
        resumed.write_all(tail.as_bytes()).unwrap();
        resumed.flush().unwrap();
    });
    let model = backend(&endpoint);
    interrupted_stream(&fixture, model.clone(), &endpoint).await;
    let requested = fixture.stdin("stale input must never be written\n");
    let requested_id = requested.operation_id.clone();
    fixture.actor.enqueue_confirmed(requested).unwrap();
    let uncertain = fixture.stdin("uncertain input must not be replayed\n");
    fixture.manager.stop_all();

    let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
    let lease = reopened.try_acquire_native_lease(&fixture.owner).unwrap();
    let operations = NativeCommandOperationStore::new(reopened.clone());
    let engine_without_actor = engine(&fixture, reopened.clone(), model.clone(), &endpoint);
    assert!(engine_without_actor
        .inspect(&lease)
        .unwrap_err()
        .to_string()
        .contains("manual"));
    let recovery_actor = actor(reopened.clone(), &fixture.owner, None);
    let engine = engine_without_actor.with_manual_command_actor(recovery_actor.clone());
    assert!(matches!(
        engine.inspect(&lease).unwrap().recovery,
        ReplayRecovery::ModelInFlight { .. }
    ));

    // A Started receipt with no terminal observation must block a model retry
    // even though that retry reuses ModelRequestStarted and performs no append.
    let mut uncertain = uncertain;
    uncertain.operation_id = uuid::Uuid::new_v4().to_string();
    let uncertain_id = uncertain.operation_id.clone();
    let saved = operations.enqueue_requested(uncertain).unwrap();
    operations
        .preflight_capacity(&lease, &uncertain_id, 512 * 1024)
        .unwrap();
    let started = operations
        .begin_started(&lease, &uncertain_id, saved.revision)
        .unwrap()
        .snapshot;
    let journal_before = reopened.load(&fixture.owner).unwrap().unwrap().events;
    assert!(engine.inspect(&lease).is_err());
    assert!(engine
        .resume_step(&lease, CancellationToken::new())
        .await
        .is_err());
    assert!(
        observed.try_recv().is_err(),
        "uncertain control allowed a provider request"
    );
    assert_eq!(
        reopened.load(&fixture.owner).unwrap().unwrap().events,
        journal_before
    );
    assert_eq!(
        operations.snapshot(&fixture.owner, &uncertain_id).unwrap(),
        Some(started.clone())
    );
    operations
        .resolve_started(
            &lease,
            &uncertain_id,
            started.revision,
            "Human inspected the process; do not repeat input",
        )
        .unwrap();
    advance.send(()).unwrap();
    let step = engine
        .resume_step(&lease, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(step.disposition, ResumeDisposition::Complete));
    let body = observed.recv_timeout(Duration::from_secs(8)).unwrap();
    assert!(!body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "assistant"));
    let saved = operations
        .snapshot(&fixture.owner, &requested_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved.state,
        NativeCommandOperationState::Rejected { .. }
    ));
    assert!(!fixture.workspace.join("effects.log").exists());
    let record = reopened.load(&fixture.owner).unwrap().unwrap();
    assert_single_response(&record.events);
    let replay = replay_native(&fixture.owner, &record.events, None, &compatibility()).unwrap();
    assert!(matches!(replay.recovery, ReplayRecovery::Idle));
    assert!(!replay.history.iter().any(|m| m.content == FIRST));
    server.join().unwrap();
}

#[tokio::test]
async fn manual_resume_consumes_confirmed_stdin_while_retrying_real_http_under_the_same_lease() {
    let fixture = Fixture::new().await;
    let (listener, endpoint) = listener();
    let (advance, advanced) = mpsc::channel();
    let (ready, received) = tokio::sync::oneshot::channel();
    let effect = fixture.workspace.join("effects.log");
    let server = std::thread::spawn(move || {
        let mut first = accept(&listener);
        read_request(&first);
        prefix(&mut first);
        advanced.recv_timeout(Duration::from_secs(8)).unwrap();
        drop(first);
        let mut resumed = accept(&listener);
        let body = read_request(&resumed);
        assert!(!body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "assistant"));
        let tail = prefix(&mut resumed);
        ready.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            if std::fs::read_to_string(&effect).ok().as_deref()
                == Some("resumed approved input 🌱\n")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Resume did not consume a control during its provider request"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        resumed.write_all(tail.as_bytes()).unwrap();
        resumed.flush().unwrap();
    });
    let model = backend(&endpoint);
    interrupted_stream(&fixture, model.clone(), &endpoint).await;
    let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
    let lease = reopened.try_acquire_native_lease(&fixture.owner).unwrap();
    let active_actor = actor(
        reopened.clone(),
        &fixture.owner,
        Some(fixture.manager.clone()),
    );
    let engine = engine(&fixture, reopened.clone(), model, &endpoint)
        .with_manual_command_actor(active_actor.clone());
    let request = active_actor
        .prepare_request(
            &fixture.command_id,
            NativeCommandOperationAction::Stdin {
                input: "resumed approved input 🌱\n".into(),
                close_after: true,
            },
        )
        .unwrap();
    let operation_id = request.operation_id.clone();
    advance.send(()).unwrap();
    let driver = async {
        received.await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            let record = reopened.load(&fixture.owner).unwrap().unwrap();
            if record.events.iter().filter(|e| matches!(&e.kind, AgentEventKind::AssistantDelta { content } if content == FIRST)).count() == 2 { break; }
            assert!(
                Instant::now() < deadline,
                "Resume first delta was not persisted before control"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        active_actor.enqueue_confirmed(request).unwrap();
    };
    let (result, ()) = tokio::join!(engine.resume_step(&lease, CancellationToken::new()), driver);
    assert!(matches!(
        result.unwrap().disposition,
        ResumeDisposition::Complete
    ));
    wait_output(
        &fixture.manager,
        &fixture.command_id,
        "ECHO:resumed approved input 🌱\nEOF\n",
    )
    .await;
    let receipt = active_actor
        .operations()
        .snapshot(&fixture.owner, &operation_id)
        .unwrap()
        .unwrap();
    assert!(matches!(
        receipt.state,
        NativeCommandOperationState::Finished {
            outcome: openide_core::NativeCommandOperationOutcome::Succeeded { .. }
        }
    ));
    let record = reopened.load(&fixture.owner).unwrap().unwrap();
    assert_single_response(&record.events);
    let replay = replay_native(&fixture.owner, &record.events, None, &compatibility()).unwrap();
    assert_eq!(
        replay.history.last().unwrap().content,
        format!("{FIRST}{FINAL}")
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("effects.log")).unwrap(),
        "resumed approved input 🌱\n"
    );
    server.join().unwrap();
}
