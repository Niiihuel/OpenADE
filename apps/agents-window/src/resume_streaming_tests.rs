//! Real HTTP recovery acceptance. The server cannot finish until the test has
//! observed and rendered the first journal-backed text delta.

use super::*;
use crate::transcript::{TranscriptItem, TranscriptProjection};
use openide_core::{
    system_instructions_digest, AgentEventKind, ModelRouteSnapshot, ReplayRecovery, TurnMode,
    MODEL_CONTEXT_VERSION, SYSTEM_INSTRUCTIONS_VERSION,
};
use openide_harness::{
    native_model_context_digest, ModelRequestCapabilities, OpenAiCompatibleBackend,
};
use openide_ui::{Size, UiTree};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MODEL: &str = "resume-stream/exact-model";
const INSTRUCTIONS: &str = "Initial additional instructions ñ for streaming recovery";
const FIRST: &str = "Primera parte ñ🙂 ";
const LAST: &str = "segunda parte 終わり";

enum Release {
    Complete,
    Disconnect,
}

struct GatedServer {
    endpoint: String,
    release: Sender<Release>,
    requests: Receiver<Value>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

fn request_body(socket: &mut TcpStream) -> Result<Value, String> {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let (start, length) = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 || bytes.len() > 1024 * 1024 {
            return Err("Incomplete or oversized HTTP request".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
            if !header.starts_with("POST /v1/chat/completions ") {
                return Err(format!("Unexpected request: {header}"));
            }
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .ok_or("Missing content length")?;
            break (end + 4, length);
        }
    };
    while bytes.len() < start + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 || bytes.len() > 1024 * 1024 {
            return Err("Incomplete or oversized HTTP body".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    serde_json::from_slice(&bytes[start..start + length]).map_err(|e| e.to_string())
}

impl GatedServer {
    fn new(attempts: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (release, gates) = mpsc::sync_channel(4);
        let (request_tx, requests) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            for attempt in 0..attempts {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return Err(format!("No HTTP attempt {attempt}"));
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?;
                let request = request_body(&mut socket)?;
                if request["model"] != MODEL || request["stream"] != true {
                    return Err(format!("Wrong streaming request: {request}"));
                }
                request_tx.send(request).map_err(|e| e.to_string())?;
                let first = format!(
                    "data: {}\n\n",
                    json!({"id":format!("attempt-{attempt}"),"choices":[{"delta":{"content":FIRST}}]})
                );
                let last = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":format!("attempt-{attempt}"),"choices":[{"delta":{"content":LAST}}]}),
                    json!({"id":format!("attempt-{attempt}"),"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":200,"completion_tokens":30,"total_tokens":230}})
                );
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",first.len()+last.len()).map_err(|e| e.to_string())?;
                // Split in the middle of the emoji's UTF-8 encoding. The parser
                // must wait for a complete SSE frame, not corrupt text bytes.
                let split = first.find('🙂').unwrap() + 1;
                socket
                    .write_all(&first.as_bytes()[..split])
                    .map_err(|e| e.to_string())?;
                socket.flush().map_err(|e| e.to_string())?;
                socket
                    .write_all(&first.as_bytes()[split..])
                    .map_err(|e| e.to_string())?;
                socket.flush().map_err(|e| e.to_string())?;
                match gates
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?
                {
                    Release::Complete => {
                        socket
                            .write_all(last.as_bytes())
                            .map_err(|e| e.to_string())?;
                        socket.flush().map_err(|e| e.to_string())?;
                    }
                    Release::Disconnect => {}
                }
            }
            Ok(())
        });
        Self {
            endpoint,
            release,
            requests,
            thread: Some(thread),
        }
    }

    fn finish(mut self) {
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

impl Drop for GatedServer {
    fn drop(&mut self) {
        // A failed assertion must unblock a server already waiting at its gate.
        let _ = self.release.send(Release::Disconnect);
    }
}

fn backend(endpoint: &str) -> Arc<dyn ModelBackend> {
    Arc::new(
        OpenAiCompatibleBackend::new(endpoint, None, MODEL)
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: true,
                ..Default::default()
            })
            .unwrap()
            .with_timeout(Duration::from_secs(15)),
    )
}

fn compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "fixture".into(),
        model: MODEL.into(),
        context_format_key: "chat-completions-v1".into(),
    }
}

struct Fixture {
    root: tempfile::TempDir,
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    session_id: String,
    endpoint: String,
    model: Arc<dyn ModelBackend>,
    tools: Arc<openide_harness::LocalTools>,
    initial: Vec<AgentEvent>,
}

impl Fixture {
    fn new(endpoint: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let checkpoints = Arc::new(CheckpointFileStore::new(root.path().join("sessions")).unwrap());
        let session = sessions.create(root.path(), "native").unwrap();
        let admission = sessions
            .admit_native_turn(
                &session.id,
                "stream-turn",
                "Recover the streamed response",
                MODEL,
                TurnMode::Build,
            )
            .unwrap();
        let tools = Arc::new(openide_harness::LocalTools::new());
        let catalog = ToolCatalog::from_executor(tools.as_ref()).unwrap();
        let model = backend(endpoint);
        let profile = model.request_profile_fingerprint().unwrap();
        let context = native_model_context_digest(
            "fixture",
            endpoint,
            MODEL,
            TurnMode::Build,
            root.path(),
            &catalog,
            INSTRUCTIONS,
            profile,
        )
        .unwrap();
        let kinds = [
            AgentEventKind::TurnStarted {
                prompt: "Recover the streamed response".into(),
            },
            AgentEventKind::ToolCatalogCommitted {
                digest: catalog.digest_hex(),
            },
            AgentEventKind::HooksCommitted {
                version: openide_core::HOOK_CONFIG_IDENTITY_VERSION,
                digest: CommandHooks::default().digest_hex().unwrap(),
            },
            AgentEventKind::ExecPolicyCommitted {
                version: openide_core::EXEC_POLICY_IDENTITY_VERSION,
                digest: PermissionPolicy::new().digest_hex(),
            },
            AgentEventKind::SystemInstructionsCommitted {
                version: SYSTEM_INSTRUCTIONS_VERSION,
                digest: system_instructions_digest(INSTRUCTIONS).unwrap(),
                content: INSTRUCTIONS.into(),
            },
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                digest: context,
            },
            ModelRouteSnapshot::new("fixture", MODEL, endpoint, profile)
                .unwrap()
                .into_event(),
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: MODEL.into(),
            },
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            sessions
                .append_native_event(&AgentEvent::new(
                    &session.id,
                    admission.sequence + index as u64 + 1,
                    kind,
                ))
                .unwrap();
        }
        let initial = sessions.load(&session.id).unwrap().unwrap().events;
        Self {
            root,
            sessions,
            checkpoints,
            session_id: session.id,
            endpoint: endpoint.into(),
            model,
            tools,
            initial,
        }
    }

    fn request(&self) -> ResumeWorkerRequest {
        ResumeWorkerRequest {
            native_automation: None,
            session_id: self.session_id.clone(),
            provider: "fixture".into(),
            endpoint: self.endpoint.clone(),
            sessions: self.sessions.clone(),
            checkpoints: self.checkpoints.clone(),
            compatibility: compatibility(),
            model: self.model.clone(),
            base_tools: self.tools.clone(),
            child_command_profile: None,
            child_tools: self.tools.clone(),
            event_bridge: None,
            tool_preparation: None,
            permission_policy: PermissionPolicy::new(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: None,
            child_identity_budget: None,
            system_instructions: INSTRUCTIONS.into(),
            pre_context_instructions: None,
            limits: RunLimits::default(),
        }
    }

    fn reopen(&mut self) {
        self.sessions = Arc::new(SessionStore::open(self.root.path().join("sessions")).unwrap());
        self.checkpoints =
            Arc::new(CheckpointFileStore::new(self.root.path().join("sessions")).unwrap());
        self.model = backend(&self.endpoint);
    }

    fn events(&self) -> Vec<AgentEvent> {
        self.sessions
            .load(&self.session_id)
            .unwrap()
            .unwrap()
            .events
    }

    fn projection(&self) -> TranscriptProjection {
        TranscriptProjection::from_events(&self.events())
    }
}

fn observe_first(
    fixture: &Fixture,
    handle: &ResumeWorkerHandle,
    projection: &mut TranscriptProjection,
    delivered: &mut Vec<AgentEvent>,
) {
    loop {
        match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
            ResumeUpdate::Events(events) => {
                assert_eq!(
                    events.len(),
                    1,
                    "Each UI update consumes one event of the frame budget"
                );
                for event in events {
                    let saved = fixture.events();
                    assert_eq!(
                        saved.iter().find(|saved| saved.sequence == event.sequence),
                        Some(&event),
                        "No publish before append"
                    );
                    projection.apply_event(&event);
                    let first = matches!(&event.kind,AgentEventKind::AssistantDelta {content} if content == FIRST);
                    delivered.push(event);
                    if first {
                        assert!(projection.items().iter().any(|row| matches!(row,TranscriptItem::Assistant {content,streaming:true} if content == FIRST)));
                        let mut state = crate::view::tests::empty_state();
                        state.main_page = crate::view::MainPage::Workspace;
                        state.workspace = fixture.root.path().display().to_string();
                        state.selected_session = Some(fixture.session_id.clone());
                        state.selected_backend = "native".into();
                        state.running = true;
                        state.transcript = projection.items().to_vec();
                        let frame = UiTree::new(crate::view::build_view(&state, 900.0))
                            .layout(Size::new(900.0, 720.0));
                        assert!(
                            frame
                                .text_layouts()
                                .iter()
                                .any(|block| block.source.contains(FIRST.trim())),
                            "First SSE text reaches a real UI frame before terminal release"
                        );
                        assert!(!saved.iter().any(|event| matches!(
                            event.kind,
                            AgentEventKind::ModelResponseCommitted { .. }
                                | AgentEventKind::TurnCompleted { .. }
                        )));
                        assert!(fixture
                            .sessions
                            .try_acquire_native_lease(&fixture.session_id)
                            .is_err());
                        return;
                    }
                }
            }
            other => panic!("Recovery finished before the server released its terminal: {other:?}"),
        }
    }
}

fn finish_updates(
    handle: &ResumeWorkerHandle,
    projection: &mut TranscriptProjection,
    delivered: &mut Vec<AgentEvent>,
) -> Result<ResumeDisposition, String> {
    loop {
        match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
            ResumeUpdate::Events(events) => {
                assert_eq!(events.len(), 1);
                for event in events {
                    projection.apply_event(&event);
                    delivered.push(event);
                }
            }
            ResumeUpdate::Finished(result) => return result,
            other => panic!("Unexpected recovery state: {other:?}"),
        }
    }
}

fn assert_exact_delivery(fixture: &Fixture, baseline: u64, delivered: &[AgentEvent]) {
    let saved = fixture.events();
    let expected: Vec<_> = saved
        .into_iter()
        .filter(|event| event.sequence > baseline)
        .collect();
    assert_eq!(
        delivered, expected,
        "Immediate deltas and final step snapshots deliver each event exactly once"
    );
    assert!(delivered
        .windows(2)
        .all(|pair| pair[0].sequence + 1 == pair[1].sequence));
}

#[test]
fn resume_streaming_http_paints_first_delta_before_terminal_and_delivers_final_events_once() {
    let server = GatedServer::new(1);
    let fixture = Fixture::new(&server.endpoint);
    let handle = ResumeWorkerHandle::start(fixture.request()).unwrap();
    let mut projection = fixture.projection();
    let mut delivered = Vec::new();
    observe_first(&fixture, &handle, &mut projection, &mut delivered);
    let request = server
        .requests
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(request["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(INSTRUCTIONS));
    server.release.send(Release::Complete).unwrap();
    assert_eq!(
        finish_updates(&handle, &mut projection, &mut delivered).unwrap(),
        ResumeDisposition::Complete
    );
    assert_exact_delivery(
        &fixture,
        fixture.initial.last().unwrap().sequence,
        &delivered,
    );
    let response = format!("{FIRST}{LAST}");
    assert_eq!(
        projection
            .items()
            .iter()
            .filter(|row| matches!(row, TranscriptItem::Assistant { .. }))
            .count(),
        1
    );
    assert!(projection.items().iter().any(|row| matches!(row,TranscriptItem::Assistant {content,streaming:false} if content == &response)));
    assert!(fixture
        .sessions
        .try_acquire_native_lease(&fixture.session_id)
        .is_ok());
    server.finish();
}

fn partial_then_manual_reopen(cancel: bool) {
    let server = GatedServer::new(2);
    let mut fixture = Fixture::new(&server.endpoint);
    let first = ResumeWorkerHandle::start(fixture.request()).unwrap();
    let mut projection = fixture.projection();
    let mut delivered = Vec::new();
    observe_first(&fixture, &first, &mut projection, &mut delivered);
    if cancel {
        first.cancel();
        assert!(finish_updates(&first, &mut projection, &mut delivered).is_err());
        server.release.send(Release::Disconnect).unwrap();
    } else {
        server.release.send(Release::Disconnect).unwrap();
        assert!(finish_updates(&first, &mut projection, &mut delivered).is_err());
    }
    assert_exact_delivery(
        &fixture,
        fixture.initial.last().unwrap().sequence,
        &delivered,
    );
    assert!(fixture
        .sessions
        .try_acquire_native_lease(&fixture.session_id)
        .is_ok());
    let partial = fixture.events();
    assert!(partial.iter().any(
        |event| matches!(&event.kind,AgentEventKind::AssistantDelta {content} if content == FIRST)
    ));
    assert!(!partial.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::ModelResponseCommitted { .. }
            | AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnCancelled
    )));
    drop(first);
    fixture.reopen();
    assert_eq!(fixture.events(), partial);
    let replay = replay_native(
        &fixture.session_id,
        &partial,
        Some(&fixture.checkpoints),
        &compatibility(),
    )
    .unwrap();
    assert!(matches!(
        replay.recovery,
        ReplayRecovery::ModelInFlight { step: 1, .. }
    ));
    assert!(!replay
        .history
        .iter()
        .any(|message| message.content.contains(FIRST)));
    let mut reopened = fixture.projection();
    assert!(reopened.items().iter().any(
        |row| matches!(row,TranscriptItem::Assistant {content,streaming:false} if content == FIRST)
    ));
    assert!(reopened.items().iter().any(
        |row| matches!(row,TranscriptItem::Notice {message,..} if message.contains("interrumpido"))
    ));
    // Nothing resumes on opening a store. This new handle is an explicit action.
    let second = ResumeWorkerHandle::start(fixture.request()).unwrap();
    let mut second_delivery = Vec::new();
    observe_first(&fixture, &second, &mut reopened, &mut second_delivery);
    server.release.send(Release::Complete).unwrap();
    assert_eq!(
        finish_updates(&second, &mut reopened, &mut second_delivery).unwrap(),
        ResumeDisposition::Complete
    );
    assert_exact_delivery(&fixture, partial.last().unwrap().sequence, &second_delivery);
    let completed = fixture.events();
    assert_eq!(&completed[..partial.len()], partial);
    assert_eq!(
        completed
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::ModelRouteCommitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        completed
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
            .count(),
        1
    );
    let final_projection = fixture.projection();
    assert_eq!(
        final_projection
            .items()
            .iter()
            .filter(|row| matches!(row, TranscriptItem::Assistant { .. }))
            .count(),
        1
    );
    assert!(final_projection.items().iter().any(|row| matches!(row,TranscriptItem::Assistant {content,streaming:false} if content == &format!("{FIRST}{LAST}"))));
    server.finish();
}

#[test]
fn resume_streaming_http_disconnect_preserves_partial_reopen_and_manual_retry() {
    partial_then_manual_reopen(false);
}

#[test]
fn resume_streaming_http_cancel_preserves_partial_reopen_and_manual_retry() {
    partial_then_manual_reopen(true);
}

#[test]
fn resume_streaming_delivery_rejects_owner_gaps_and_live_duplicates_without_advancing() {
    let (updates, receiver) = mpsc::sync_channel(64);
    let forwarder = ResumeEventForwarder {
        session_id: "owner".into(),
        updates,
        last_reported_sequence: Mutex::new(10),
    };
    let delta = |owner: &str, sequence| {
        AgentEvent::new(
            owner,
            sequence,
            AgentEventKind::AssistantDelta {
                content: format!("delta {sequence}"),
            },
        )
    };
    for invalid in [delta("another-session", 11), delta("owner", 12)] {
        assert!(forwarder.on_persisted_event(&invalid).is_err());
        assert_eq!(*forwarder.last_reported_sequence.lock().unwrap(), 10);
        assert!(receiver.try_recv().is_err());
    }
    let first = delta("owner", 11);
    forwarder.on_persisted_event(&first).unwrap();
    assert!(
        matches!(receiver.try_recv().unwrap(), ResumeUpdate::Events(events) if events == [first.clone()])
    );
    assert!(forwarder.on_persisted_event(&first).is_err());
    assert!(receiver.try_recv().is_err());
    let last = delta("owner", 12);
    forwarder
        .forward_step_events(vec![first.clone(), last.clone()])
        .unwrap();
    assert!(
        matches!(receiver.try_recv().unwrap(), ResumeUpdate::Events(events) if events == [last.clone()])
    );
    forwarder.forward_step_events(vec![first, last]).unwrap();
    assert_eq!(*forwarder.last_reported_sequence.lock().unwrap(), 12);
    assert!(receiver.try_recv().is_err());
    assert!(forwarder
        .forward_step_events(vec![delta("owner", 13), delta("owner", 15)])
        .is_err());
    assert_eq!(*forwarder.last_reported_sequence.lock().unwrap(), 12);
    assert!(receiver.try_recv().is_err());
    drop(receiver);
    assert!(forwarder.on_persisted_event(&delta("owner", 13)).is_err());
    assert_eq!(*forwarder.last_reported_sequence.lock().unwrap(), 12);
}
