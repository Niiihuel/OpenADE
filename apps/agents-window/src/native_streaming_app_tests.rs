//! HTTP/SSE acceptance through the real native runner, host poll and UI tree.
//! The server cannot send another delta or a terminal until the test releases
//! the corresponding barrier after inspecting a frame.

use super::*;
use crate::transcript::TranscriptItem;
use openide_core::MessageRole;
use openide_ui::{DrawCommand, Point};
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

const MODEL: &str = "streaming/model:exact";
const FIRST_VISIBLE: &str = "VISIBLE_PRIMER_DELTA ñ🦀";
const SECOND_VISIBLE: &str = "VISIBLE_SEGUNDO_DELTA";
const PROMPT: &str = "Comprueba la respuesta incremental nativa";
const DEADLINE: Duration = Duration::from_secs(15);

enum ServerCommand {
    NextDelta,
    Terminal,
    Truncate { clean_http: bool },
    Observe(Sender<()>),
    Stop,
}

use std::sync::mpsc::Sender;

struct StreamingHttpFixture {
    endpoint: String,
    commands: Sender<ServerCommand>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    first_flushed: Arc<AtomicBool>,
    second_flushed: Arc<AtomicBool>,
    terminal_flushed: Arc<AtomicBool>,
    client_closed: Arc<AtomicBool>,
    split_utf8: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

fn read_request(stream: &mut TcpStream) -> Result<(String, Value), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let (header_end, length, path) = loop {
        let mut fragment = [0u8; 2048];
        let count = stream
            .read(&mut fragment)
            .map_err(|error| error.to_string())?;
        if count == 0 || bytes.len() + count > 512 * 1024 {
            return Err("Incomplete or oversized fixture HTTP request".into());
        }
        bytes.extend_from_slice(&fragment[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let path = headers
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("POST "))
                .and_then(|line| line.strip_suffix(" HTTP/1.1"))
                .ok_or("Expected a fixture POST request")?
                .to_owned();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .filter(|length| *length <= 512 * 1024)
                .ok_or("Expected a bounded request body")?;
            break (end + 4, length, path);
        }
    };
    while bytes.len() < header_end + length {
        let mut fragment = [0u8; 2048];
        let count = stream
            .read(&mut fragment)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("Truncated fixture request body".into());
        }
        bytes.extend_from_slice(&fragment[..count]);
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + length])
        .map_err(|error| error.to_string())?;
    // Authorization headers are neither retained nor printed by the fixture.
    Ok((path, body))
}

fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    write!(stream, "{:x}\r\n", bytes.len()).map_err(|error| error.to_string())?;
    stream.write_all(bytes).map_err(|error| error.to_string())?;
    stream
        .write_all(b"\r\n")
        .map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())
}

fn write_delta(stream: &mut TcpStream, text: &str) -> Result<bool, String> {
    let event = json!({"id":"stream-fixture", "choices":[{
        "index":0, "delta":{"content":text}, "finish_reason":null
    }]});
    let payload = format!("data: {event}\n\n").into_bytes();
    if let Some(index) = payload
        .windows(2)
        .position(|window| window == "ñ".as_bytes())
    {
        let boundary = index + 1;
        assert!(std::str::from_utf8(&payload[..boundary]).is_err());
        // HTTP chunk boundaries intentionally divide a UTF-8 scalar. SSE
        // framing is completed only after subsequent chunks arrive.
        for fragment in payload[..boundary].chunks(7) {
            write_chunk(stream, fragment)?;
        }
        for fragment in payload[boundary..].chunks(5) {
            write_chunk(stream, fragment)?;
        }
        Ok(true)
    } else {
        for fragment in payload.chunks(7) {
            write_chunk(stream, fragment)?;
        }
        Ok(false)
    }
}

impl StreamingHttpFixture {
    fn new(first: String, second: String) -> Self {
        Self::with_second_deltas(first, vec![second])
    }

    fn with_second_deltas(first: String, second: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (commands, receiver) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let first_flushed = Arc::new(AtomicBool::new(false));
        let second_flushed = Arc::new(AtomicBool::new(false));
        let terminal_flushed = Arc::new(AtomicBool::new(false));
        let client_closed = Arc::new(AtomicBool::new(false));
        let split_utf8 = Arc::new(AtomicBool::new(false));
        let state = (
            requests.clone(),
            first_flushed.clone(),
            second_flushed.clone(),
            terminal_flushed.clone(),
            client_closed.clone(),
            split_utf8.clone(),
        );
        let worker = std::thread::spawn(move || {
            let (
                requests,
                first_flushed,
                second_flushed,
                terminal_flushed,
                client_closed,
                split_utf8,
            ) = state;
            let deadline = Instant::now() + DEADLINE;
            let mut connection = None;
            let mut admitted = false;
            loop {
                // Keep listening through cancellation/completion and reopen.
                // An automatic retry would be an additional observable POST.
                while let Ok((mut stream, _)) = listener.accept() {
                    let request = read_request(&mut stream)?;
                    requests.lock().unwrap().push(request);
                    if admitted {
                        stream.write_all(b"HTTP/1.1 500 Unexpected request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .map_err(|error| error.to_string())?;
                        continue;
                    }
                    admitted = true;
                    stream
                        .set_nodelay(true)
                        .map_err(|error| error.to_string())?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .map_err(|error| error.to_string())?;
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                        .map_err(|error| error.to_string())?;
                    split_utf8.store(write_delta(&mut stream, &first)?, Ordering::Release);
                    first_flushed.store(true, Ordering::Release);
                    stream
                        .set_nonblocking(true)
                        .map_err(|error| error.to_string())?;
                    connection = Some(stream);
                }
                if !admitted && Instant::now() >= deadline {
                    return Err("The native runner never requested the streaming fixture".into());
                }
                if let Some(stream) = connection.as_mut() {
                    match stream.peek(&mut [0u8; 1]) {
                        Ok(0) => {
                            client_closed.store(true, Ordering::Release);
                            connection = None;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                            client_closed.store(true, Ordering::Release);
                            connection = None;
                        }
                        Err(error) => return Err(error.to_string()),
                        _ => {}
                    }
                }
                match receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(ServerCommand::NextDelta) => {
                        let stream = connection
                            .as_mut()
                            .ok_or("The client closed before the second delta")?;
                        stream
                            .set_nonblocking(false)
                            .map_err(|error| error.to_string())?;
                        for delta in &second {
                            write_delta(stream, delta)?;
                        }
                        stream
                            .set_nonblocking(true)
                            .map_err(|error| error.to_string())?;
                        second_flushed.store(true, Ordering::Release);
                    }
                    Ok(ServerCommand::Terminal) => {
                        if !second_flushed.load(Ordering::Acquire) {
                            return Err("Terminal released before the second delta barrier".into());
                        }
                        let mut stream = connection
                            .take()
                            .ok_or("The client closed before the terminal")?;
                        stream
                            .set_nonblocking(false)
                            .map_err(|error| error.to_string())?;
                        let terminal = json!({"id":"stream-fixture", "choices":[{
                            "index":0, "delta":{}, "finish_reason":"stop"
                        }], "usage":{"prompt_tokens":17,"completion_tokens":9,"total_tokens":26}});
                        write_chunk(
                            &mut stream,
                            format!("data: {terminal}\n\ndata: [DONE]\n\n").as_bytes(),
                        )?;
                        stream
                            .write_all(b"0\r\n\r\n")
                            .map_err(|error| error.to_string())?;
                        stream.flush().map_err(|error| error.to_string())?;
                        terminal_flushed.store(true, Ordering::Release);
                    }
                    Ok(ServerCommand::Truncate { clean_http }) => {
                        let mut stream = connection
                            .take()
                            .ok_or("The client closed before fixture truncation")?;
                        if clean_http {
                            // Valid HTTP EOF, but no SSE finish_reason/[DONE].
                            stream
                                .set_nonblocking(false)
                                .map_err(|error| error.to_string())?;
                            stream
                                .write_all(b"0\r\n\r\n")
                                .map_err(|error| error.to_string())?;
                            stream.flush().map_err(|error| error.to_string())?;
                        }
                        // Otherwise cut the HTTP chunked body itself short.
                        drop(stream);
                    }
                    Ok(ServerCommand::Observe(reply)) => {
                        let _ = reply.send(());
                    }
                    Ok(ServerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Ok(());
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Self {
            endpoint,
            commands,
            requests,
            first_flushed,
            second_flushed,
            terminal_flushed,
            client_closed,
            split_utf8,
            worker: Some(worker),
        }
    }

    fn next_delta(&self) {
        self.commands.send(ServerCommand::NextDelta).unwrap();
    }

    fn terminal(&self) {
        self.commands.send(ServerCommand::Terminal).unwrap();
    }

    fn truncate(&self, clean_http: bool) {
        self.commands
            .send(ServerCommand::Truncate { clean_http })
            .unwrap();
    }

    fn assert_one_native_request(&self) {
        let (reply, observed) = mpsc::channel();
        self.commands.send(ServerCommand::Observe(reply)).unwrap();
        observed.recv_timeout(DEADLINE).unwrap();
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "unexpected retry or automatic resume");
        assert_eq!(requests[0].0, "/v1/chat/completions");
        assert_eq!(requests[0].1["model"], MODEL);
        assert_eq!(requests[0].1["stream"], true);
    }
}

impl Drop for StreamingHttpFixture {
    fn drop(&mut self) {
        let _ = self.commands.send(ServerCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap().unwrap();
            }
        }
    }
}

fn app_for_stream(root: &Path, fixture: &StreamingHttpFixture) -> (AgentsWindow, String) {
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = root.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root, "native").unwrap();
    app.open_session(&session.id);
    app.view.sidebar_collapsed = true;
    app.view.show_side_pane = false;
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.settings_preferences.reduced_motion = true;
    app.reduced_motion = true;
    native_frame(&mut app);
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(
        matches!(app.active_run, Some(ActiveRun::Native(_))),
        "{}",
        app.view.status
    );
    (app, session.id)
}

fn native_frame(app: &mut AgentsWindow) -> UiFrame {
    native_frame_at(app, Size::new(960.0, 620.0), Instant::now())
}

fn native_frame_at(app: &mut AgentsWindow, viewport: Size, now: Instant) -> UiFrame {
    // Exercise the actual UTF-8 reveal cursor before the retained tree, as
    // App::ui does. The fixture uses reduced motion for deterministic frames.
    app.sync_stream_presentation(now);
    app.view.show_jump_to_bottom =
        !app.view.transcript.is_empty() && !app.tree.is_scroll_at_end("transcript", 24.0);
    app.layout_workspace_frame(viewport)
}

fn wait_app(app: &mut AgentsWindow, mut condition: impl FnMut(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        app.poll_run();
        if condition(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native streaming condition timed out: {}",
            app.view.status
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_observation(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "streaming server observation timed out"
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn assistant_rows(app: &AgentsWindow) -> Vec<(&str, bool)> {
    app.view
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant { content, streaming } => {
                Some((content.as_str(), *streaming))
            }
            _ => None,
        })
        .collect()
}

fn visible(frame: &UiFrame, fragment: &str) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command,
            DrawCommand::Text { text, .. } if text.contains(fragment)
        )
    })
}

fn assert_no_committed_response(app: &AgentsWindow, session_id: &str) {
    let stored = app
        .store
        .as_ref()
        .unwrap()
        .load(session_id)
        .unwrap()
        .unwrap();
    assert!(
        stored
            .events
            .iter()
            .any(|event| matches!(&event.kind, AgentEventKind::AssistantDelta { .. }))
    );
    assert!(!stored.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::ModelResponseCommitted { .. } | AgentEventKind::TurnCompleted { .. }
    )));
    assert!(app.view.running);
    assert!(app.active_run.is_some());
    assert!(
        app.history.is_empty(),
        "partial deltas are not authoritative model history"
    );
}

fn assert_one_admission_without_tools(events: &[AgentEvent]) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.kind,
                AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
            .count(),
        1
    );
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::ToolStarted { .. }
            | AgentEventKind::ToolFinished { .. }
            | AgentEventKind::PermissionRequested { .. }
    )));
}

#[test]
fn streaming_http_frames_precede_terminal_and_commit_one_exact_history_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), format!("\n\n{SECOND_VISIBLE}"));
    let (mut app, session_id) = app_for_stream(root.path(), &fixture);
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(FIRST_VISIBLE, true)]
    });
    let first_frame = native_frame(&mut app);
    assert!(fixture.first_flushed.load(Ordering::Acquire));
    assert!(fixture.split_utf8.load(Ordering::Acquire));
    assert!(!fixture.second_flushed.load(Ordering::Acquire));
    assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
    assert!(visible(&first_frame, FIRST_VISIBLE));
    assert!(!visible(&first_frame, "Respondiendo"));
    assert!(!visible(&first_frame, "AGENTE · ESCRIBIENDO"));
    assert!(!app.view.stream_presentation.has_pending());
    assert!(
        first_frame
            .regions
            .iter()
            .any(|region| region.id == "run_toggle")
    );
    assert_no_committed_response(&app, &session_id);

    fixture.next_delta();
    let expected = format!("{FIRST_VISIBLE}\n\n{SECOND_VISIBLE}");
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(expected.as_str(), true)]
    });
    let second_frame = native_frame(&mut app);
    assert!(visible(&second_frame, SECOND_VISIBLE));
    assert!(!visible(&second_frame, "Respondiendo"));
    assert!(!app.view.stream_presentation.has_pending());
    assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
    assert_no_committed_response(&app, &session_id);
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    assert!(!app.view.running);
    assert_eq!(assistant_rows(&app), vec![(expected.as_str(), false)]);
    let final_frame = native_frame(&mut app);
    assert!(visible(&final_frame, FIRST_VISIBLE));
    assert!(visible(&final_frame, SECOND_VISIBLE));
    assert!(!visible(&final_frame, "Respondiendo"));
    let stored = app
        .store
        .as_ref()
        .unwrap()
        .load(&session_id)
        .unwrap()
        .unwrap();
    assert_one_admission_without_tools(&stored.events);
    let deltas: Vec<_> = stored
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::AssistantDelta { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![FIRST_VISIBLE, format!("\n\n{SECOND_VISIBLE}").as_str()]
    );
    let committed: Vec<_> = stored
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ModelResponseCommitted {
                assistant,
                response_id,
                usage,
                ..
            } => Some((assistant, response_id, usage)),
            _ => None,
        })
        .collect();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].0.content, expected);
    assert!(committed[0].0.tool_calls.is_empty());
    assert_eq!(committed[0].1.as_deref(), Some("stream-fixture"));
    assert_eq!(committed[0].2.as_ref().unwrap().total_tokens, 26);
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
            .count(),
        1
    );
    let expected_history = vec![
        AgentMessage::user(PROMPT),
        AgentMessage::assistant(&expected, Vec::new()),
    ];
    assert_eq!(app.history, expected_history);
    assert_eq!(
        replay_stored_native(&app.data_root, &stored)
            .unwrap()
            .history,
        expected_history
    );
    let original_events = stored.events;
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(root.path().join("data"));
    reopened.open_session(&session_id);
    assert_eq!(reopened.history, expected_history);
    reopened.view.sidebar_collapsed = true;
    reopened.view.show_side_pane = false;
    reopened.tree.set_reduced_motion(true);
    let reopened_frame = native_frame(&mut reopened);
    assert!(visible(&reopened_frame, FIRST_VISIBLE));
    assert!(visible(&reopened_frame, SECOND_VISIBLE));
    assert!(!visible(&reopened_frame, "Respondiendo"));
    assert_eq!(assistant_rows(&reopened), vec![(expected.as_str(), false)]);
    assert!(reopened.active_run.is_none());
    assert!(reopened.active_resume.is_none());
    assert_eq!(
        reopened
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events,
        original_events
    );
    fixture.assert_one_native_request();
}

#[test]
fn streaming_http_cancel_after_visible_delta_keeps_partial_ui_out_of_history_without_retry() {
    cancel_after_visible_delta("run_toggle");
}

#[test]
fn stop_on_the_running_user_message_cancels_like_the_composer() {
    cancel_after_visible_delta("turn_stop");
}

fn cancel_after_visible_delta(control: &str) {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, session_id) = app_for_stream(root.path(), &fixture);
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(FIRST_VISIBLE, true)]
    });
    let frame = native_frame(&mut app);
    assert!(visible(&frame, FIRST_VISIBLE));
    assert_no_committed_response(&app, &session_id);
    let stop = frame
        .regions
        .iter()
        .find(|region| region.id == control)
        .unwrap()
        .rect;
    let point = Point::new(stop.x + stop.width / 2.0, stop.y + stop.height / 2.0);
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(
        actions,
        vec![UiAction::Clicked {
            id: control.into()
        }]
    );
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    wait_app(&mut app, |app| {
        app.active_run.is_none() && fixture.client_closed.load(Ordering::Acquire)
    });
    assert!(!fixture.second_flushed.load(Ordering::Acquire));
    assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
    assert!(!app.view.running);
    assert_eq!(assistant_rows(&app), vec![(FIRST_VISIBLE, false)]);
    assert!(app.history.is_empty());
    assert!(app.view.prompt.is_empty());
    let cancelled = native_frame(&mut app);
    assert!(visible(&cancelled, FIRST_VISIBLE));
    assert!(visible(&cancelled, "Turno cancelado"));
    assert!(!visible(&cancelled, "Respondiendo"));
    let stored = app
        .store
        .as_ref()
        .unwrap()
        .load(&session_id)
        .unwrap()
        .unwrap();
    assert_one_admission_without_tools(&stored.events);
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnCancelled))
            .count(),
        1
    );
    assert!(!stored.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::ModelResponseCommitted { .. }
            | AgentEventKind::AssistantMessage { .. }
            | AgentEventKind::TurnCompleted { .. }
            | AgentEventKind::TurnFailed { .. }
    )));
    let replay = replay_stored_native(&app.data_root, &stored).unwrap();
    assert_eq!(replay.recovery, ReplayRecovery::Idle);
    assert!(replay.history.is_empty());
    let original_events = stored.events;
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(root.path().join("data"));
    reopened.open_session(&session_id);
    reopened.view.sidebar_collapsed = true;
    reopened.view.show_side_pane = false;
    reopened.tree.set_reduced_motion(true);
    let reopened_frame = native_frame(&mut reopened);
    assert!(visible(&reopened_frame, FIRST_VISIBLE));
    assert!(visible(&reopened_frame, "Turno cancelado"));
    assert!(!visible(&reopened_frame, "Respondiendo"));
    assert_eq!(assistant_rows(&reopened), vec![(FIRST_VISIBLE, false)]);
    assert!(
        reopened
            .history
            .iter()
            .all(|message| message.role != MessageRole::Assistant)
    );
    assert!(reopened.history.is_empty());
    assert!(reopened.active_run.is_none());
    assert!(reopened.active_resume.is_none());
    reopened.poll_run();
    assert_eq!(
        reopened
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events,
        original_events
    );
    fixture.assert_one_native_request();
}

#[test]
fn streaming_http_scroll_follows_the_tail_and_preserves_a_user_scrolled_position() {
    for follow in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let first = format!(
            "Primer bloque ñ🦀\n\n{}VISIBLE_PRIMER_FINAL",
            (0..30)
                .map(|index| format!("Primera fila {index}: información renderizada.\n\n"))
                .collect::<String>()
        );
        let second = format!(
            "\n\n{}VISIBLE_SEGUNDO_FINAL",
            (0..20)
                .map(|index| format!("Segunda fila {index}: información incremental.\n\n"))
                .collect::<String>()
        );
        let fixture = StreamingHttpFixture::new(first.clone(), second.clone());
        let (mut app, session_id) = app_for_stream(root.path(), &fixture);
        wait_app(&mut app, |app| {
            assistant_rows(app) == vec![(first.as_str(), true)]
        });
        let frame = native_frame(&mut app);
        assert!(app.tree.is_scroll_at_end("transcript", 0.1));
        assert!(app.tree.scroll_offset("transcript") > 0.0);
        assert!(visible(&frame, "VISIBLE_PRIMER_FINAL"));
        if !follow {
            let region = frame
                .regions
                .iter()
                .find(|region| region.id == "transcript")
                .unwrap()
                .rect;
            app.tree.handle_event(UiEvent::ScrollDirect {
                position: Point::new(
                    region.x + region.width / 2.0,
                    region.y + region.height / 2.0,
                ),
                delta_y: 350.0,
            });
            native_frame(&mut app);
            assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
        }
        let previous_offset = app.tree.scroll_offset("transcript");
        fixture.next_delta();
        let expected = format!("{first}{second}");
        wait_app(&mut app, |app| {
            assistant_rows(app) == vec![(expected.as_str(), true)]
        });
        let delta_frame = native_frame(&mut app);
        assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
        assert_no_committed_response(&app, &session_id);
        if follow {
            assert!(app.tree.is_scroll_at_end("transcript", 0.1));
            assert!(app.tree.scroll_offset("transcript") > previous_offset);
            assert!(visible(&delta_frame, "VISIBLE_SEGUNDO_FINAL"));
        } else {
            assert!((app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
            assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
            assert!(!visible(&delta_frame, "VISIBLE_SEGUNDO_FINAL"));
        }
        fixture.terminal();
        wait_app(&mut app, |app| app.active_run.is_none());
        assert_eq!(assistant_rows(&app), vec![(expected.as_str(), false)]);
        // The ended turn grows its actions row under the text, with motion
        // on as in the app; a follower sees it once things settle.
        app.tree.set_reduced_motion(false);
        app.settings_preferences.reduced_motion = false;
        app.reduced_motion = false;
        let ended_at = Instant::now();
        for step in 0..40 {
            native_frame_at(&mut app, Size::new(960.0, 620.0), ended_at + Duration::from_millis(50 * step));
        }
        assert_eq!(app.tree.is_scroll_at_end("transcript", 0.1), follow, "offset {}", app.tree.scroll_offset("transcript"));
        fixture.assert_one_native_request();
    }
}

fn transcript_rect(frame: &UiFrame) -> openide_ui::Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == "transcript" && region.kind == openide_ui::HitKind::Scroll)
        .unwrap()
        .rect
}

fn scroll_up_from_tail(app: &mut AgentsWindow, frame: &UiFrame) {
    let region = transcript_rect(frame);
    app.tree.handle_event(UiEvent::ScrollDirect {
        position: Point::new(
            region.x + region.width / 2.0,
            region.y + region.height / 2.0,
        ),
        delta_y: 350.0,
    });
    assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
}

fn reasoning_scroll_fixture() -> (tempfile::TempDir, AgentsWindow, String) {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.selected_session = Some("reasoning-scroll-owner".into());
    app.view.sidebar_collapsed = true;
    app.view.show_side_pane = false;
    app.reduced_motion = true;
    app.tree.set_reduced_motion(true);
    app.view.transcript = vec![
        TranscriptItem::Assistant {
            content: (0..30)
                .map(|index| format!("Historial {index}: información anterior.\n\n"))
                .collect(),
            streaming: false,
        },
        TranscriptItem::Reasoning {
            id: "reason-live".into(),
            step: 1,
            content: "Primer fragmento breve.".into(),
            status: crate::transcript::ModelStreamStatus::Receiving,
            truncated: false,
        },
    ];
    // Reasoning text opens only by the reader's choice.
    app.view
        .conversation_details
        .sync(Some("reasoning-scroll-owner"));
    app.view
        .conversation_details
        .set_expanded("reason-live", true);
    app.tree.scroll_to_end("transcript");
    native_frame(&mut app);
    let body = "reasoning-body:reasoning-scroll-owner:reason-live".to_owned();
    assert!(app.tree.is_scroll_at_end(&body, 0.1));
    assert_eq!(app.tree.scroll_offset(&body), 0.0);
    append_reasoning_lines(&mut app);
    native_frame(&mut app);
    assert!(app.tree.is_scroll_at_end(&body, 0.1));
    assert!(
        app.tree.scroll_offset(&body) > 160.0,
        "first overflow follows its tail"
    );
    (root, app, body)
}

fn append_reasoning_lines(app: &mut AgentsWindow) {
    let TranscriptItem::Reasoning { content, .. } = &mut app.view.transcript[1] else {
        panic!("reasoning fixture row");
    };
    content.push_str(
        &(0..24)
            .map(|index| format!("\n\nRazonamiento {index}: resumen público."))
            .collect::<String>(),
    );
}

#[test]
fn receiving_reasoning_follows_its_own_tail_without_moving_a_manual_reader() {
    for follow_main in [true, false] {
        for follow_body in [true, false] {
            let (_root, mut app, body) = reasoning_scroll_fixture();
            // Passive rows do not change which reasoning chunk is receiving.
            // A trailing usage row also means text reveal cannot supply the
            // missing scroll request for the next append.
            app.view.transcript.push(TranscriptItem::PlanSnapshot {
                id: "passive-plan".into(),
                sequence: Some(9),
                plan: openide_core::PlanSnapshot::from_plan_response("1. Revisar"),
                is_proposal: false,
                tool_call_id: None,
            });
            app.view.transcript.push(TranscriptItem::ModelUsage {
                id: "passive-usage".into(),
                step: 1,
                usage: openide_core::ModelTokenUsage {
                    input_tokens: 20,
                    output_tokens: 10,
                    total_tokens: 30,
                    reasoning_tokens: Some(5),
                    cached_input_tokens: None,
                },
                status: crate::transcript::ModelStreamStatus::Receiving,
            });
            app.tree.scroll_to_end("transcript");
            let mut frame = native_frame(&mut app);
            if !follow_main {
                let rect = transcript_rect(&frame);
                app.tree.handle_event(UiEvent::ScrollDirect {
                    position: Point::new(rect.x + 2.0, rect.y + 2.0),
                    delta_y: 60.0,
                });
                frame = native_frame(&mut app);
                assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
            }
            if !follow_body {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == body)
                    .unwrap()
                    .rect;
                let outer = transcript_rect(&frame);
                app.tree.handle_event(UiEvent::ScrollDirect {
                    position: Point::new(rect.x + 8.0, (rect.y + 8.0).max(outer.y + 8.0)),
                    delta_y: 120.0,
                });
                native_frame(&mut app);
                assert!(!app.tree.is_scroll_at_end(&body, 24.0));
            }
            let previous_body = app.tree.scroll_offset(&body);
            let previous_main = app.tree.scroll_offset("transcript");
            append_reasoning_lines(&mut app);
            assert!(!app.sync_stream_presentation(Instant::now()));
            app.layout_workspace_frame(Size::new(960.0, 620.0));
            assert!((app.tree.scroll_offset("transcript") - previous_main).abs() < 0.1);
            if follow_body {
                assert!(app.tree.is_scroll_at_end(&body, 0.1));
                assert!(app.tree.scroll_offset(&body) > previous_body + 160.0);
            } else {
                assert!((app.tree.scroll_offset(&body) - previous_body).abs() < 0.1);
                assert!(!app.tree.is_scroll_at_end(&body, 24.0));
            }
        }
    }
}

#[test]
fn reasoning_follow_requires_an_open_receiving_tail_and_current_layout_owner() {
    for guard in ["completed", "collapsed", "superseded", "owner"] {
        let (_root, mut app, body) = reasoning_scroll_fixture();
        app.view
            .conversation_details
            .set_expanded("reason-live", guard != "collapsed");
        match guard {
            "completed" => {
                let TranscriptItem::Reasoning { status, .. } = &mut app.view.transcript[1] else {
                    unreachable!();
                };
                *status = crate::transcript::ModelStreamStatus::Committed;
            }
            "superseded" => app.view.transcript.push(TranscriptItem::Assistant {
                content: "La respuesta ya empezó.".into(),
                streaming: false,
            }),
            "owner" => app.transcript_layout_session = Some("previous-owner".into()),
            _ => {}
        }
        let previous = app.tree.scroll_offset(&body);
        append_reasoning_lines(&mut app);
        native_frame(&mut app);
        if guard == "collapsed" {
            assert!(
                !app.tree
                    .frame()
                    .regions
                    .iter()
                    .any(|region| region.id == body)
            );
            app.view
                .conversation_details
                .set_expanded("reason-live", true);
            app.tree.scroll_to_end("transcript");
            native_frame(&mut app);
        }
        assert!(
            (app.tree.scroll_offset(&body) - previous).abs() < 0.1,
            "{guard}"
        );
        assert!(!app.tree.is_scroll_at_end(&body, 24.0), "{guard}");
    }
}

#[test]
fn streaming_http_composer_growth_and_resize_preserve_tail_or_manual_reading() {
    for reduced_motion in [true, false] {
        for follow in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let first = format!(
                "{}VISIBLE_RETAINED_TAIL ñ🦀",
                (0..36)
                    .map(|index| format!("Fila retenida {index}: texto incremental ñ🦀.\n\n"))
                    .collect::<String>()
            );
            let fixture = StreamingHttpFixture::new(first.clone(), SECOND_VISIBLE.into());
            let (mut app, session_id) = app_for_stream(root.path(), &fixture);
            app.tree.set_reduced_motion(reduced_motion);
            app.settings_preferences.reduced_motion = reduced_motion;
            app.reduced_motion = reduced_motion;
            wait_app(&mut app, |app| {
                assistant_rows(app) == vec![(first.as_str(), true)]
            });
            let viewport = Size::new(960.0, 620.0);
            let now = Instant::now();
            native_frame_at(&mut app, viewport, now);
            if !reduced_motion {
                assert!(app.view.stream_presentation.has_pending());
                let visible_bytes = app
                    .view
                    .stream_presentation
                    .visible_bytes(app.view.transcript.len() - 1, &first);
                assert!(visible_bytes > 0 && visible_bytes < first.len());
                assert!(first.is_char_boundary(visible_bytes));
            }
            let after_reveal = now + Duration::from_millis(250);
            let frame = native_frame_at(&mut app, viewport, after_reveal);
            assert!(!app.view.stream_presentation.has_pending());
            assert!(visible(&frame, "VISIBLE_RETAINED_TAIL"));
            assert!(app.tree.is_scroll_at_end("transcript", 0.1));
            if !follow {
                scroll_up_from_tail(&mut app, &frame);
                native_frame_at(&mut app, viewport, after_reveal);
            }
            let previous_offset = app.tree.scroll_offset("transcript");
            let previous_height = transcript_rect(&frame).height;
            let draft = (0..12)
                .map(|index| format!("Instrucción adicional {index}\n"))
                .collect::<String>();
            app.tree.set_input_value("prompt", &draft);
            app.apply_action(
                UiAction::InputChanged {
                    id: "prompt".into(),
                    value: draft,
                },
                &egui::Context::default(),
            );
            let grown = native_frame_at(&mut app, viewport, after_reveal);
            assert!(transcript_rect(&grown).height < previous_height - 50.0);
            if follow {
                assert!(app.tree.is_scroll_at_end("transcript", 0.1));
                assert!(app.tree.scroll_offset("transcript") > previous_offset + 50.0);
                assert!(visible(&grown, "VISIBLE_RETAINED_TAIL"));
            } else {
                assert!((app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
                assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
                assert!(!visible(&grown, "VISIBLE_RETAINED_TAIL"));
            }
            let resized = native_frame_at(
                &mut app,
                Size::new(viewport.width, viewport.height - 80.0),
                after_reveal,
            );
            assert!(transcript_rect(&resized).height < transcript_rect(&grown).height - 50.0);
            if follow {
                assert!(app.tree.is_scroll_at_end("transcript", 0.1));
                assert!(visible(&resized, "VISIBLE_RETAINED_TAIL"));
            } else {
                assert!((app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
                assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
                assert!(!visible(&resized, "VISIBLE_RETAINED_TAIL"));
            }
            assert!(!fixture.second_flushed.load(Ordering::Acquire));
            assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
            assert_no_committed_response(&app, &session_id);
            fixture.next_delta();
            fixture.terminal();
            wait_app(&mut app, |app| app.active_run.is_none());
            fixture.assert_one_native_request();
        }
    }
}

#[test]
fn streaming_http_live_plan_snapshots_update_without_moving_a_manual_reader() {
    for follow in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let first = (0..36)
            .map(|index| format!("Fila de trabajo {index}: respuesta parcial.\n\n"))
            .collect::<String>();
        let fixture = StreamingHttpFixture::new(first.clone(), SECOND_VISIBLE.into());
        let (mut app, session_id) = app_for_stream(root.path(), &fixture);
        wait_app(&mut app, |app| {
            assistant_rows(app) == vec![(first.as_str(), true)]
        });
        let frame = native_frame(&mut app);
        if !follow {
            scroll_up_from_tail(&mut app, &frame);
            native_frame(&mut app);
        }
        let previous_offset = app.tree.scroll_offset("transcript");
        let previous_rect = transcript_rect(&frame);
        let proposal = openide_core::PlanSnapshot::from_plan_response("1. LIVE_INITIAL_PLAN");
        assert!(app.process_event(AgentEvent::new(
            &session_id,
            app.last_sequence + 1,
            AgentEventKind::PlanProposed {
                plan: proposal.clone(),
            },
        )));
        app.publish_transcript();
        assert_eq!(app.view.session_plans[&session_id].snapshot, proposal);
        assert!(!app.view.session_plans[&session_id].can_build);
        let plan = openide_core::PlanSnapshot::from_plan_response(
            &(0..12)
                .map(|index| format!("{}. LIVE_PLAN_STEP_{index}\n", index + 1))
                .collect::<String>(),
        );
        let event = AgentEvent::new(
            &session_id,
            app.last_sequence + 1,
            AgentEventKind::PlanUpdated {
                tool_call_id: "live_plan_call".into(),
                plan: plan.clone(),
            },
        );
        // Typed publications append immutable timeline snapshots. Final
        // journal replay still owns durable state and Build authorization.
        assert!(app.process_event(event.clone()));
        app.publish_transcript();
        assert_eq!(assistant_rows(&app), vec![(first.as_str(), true)]);
        assert_eq!(app.view.session_plans[&session_id].snapshot, plan);
        assert!(!app.view.session_plans[&session_id].can_build);
        assert!(!app.process_event(event));
        // Live snapshots reveal their steps as soon as they are published,
        // without a synthetic disclosure click or granting Build permission.
        let plan_frame = native_frame(&mut app);
        assert!(!plan_frame.regions.iter().any(|region| {
            region
                .id
                .starts_with(&format!("conversation_detail:{session_id}:plan:"))
        }));
        assert_eq!(transcript_rect(&plan_frame), previous_rect);
        assert!(
            !plan_frame
                .regions
                .iter()
                .any(|region| region.id.starts_with("session_build_plan:"))
        );
        if follow {
            assert!(app.tree.is_scroll_at_end("transcript", 0.1));
            assert!(app.tree.scroll_offset("transcript") > previous_offset + 50.0);
            assert!(visible(&plan_frame, "LIVE_PLAN_STEP_11"));
        } else {
            assert!((app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
            assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
            assert!(!visible(&plan_frame, "LIVE_PLAN_STEP_11"));
        }
        let mut invalid = plan.clone();
        invalid.plan.clear();
        let last_valid_sequence = app.last_sequence;
        let snapshot_count = app
            .view
            .transcript
            .iter()
            .filter(|item| matches!(item, TranscriptItem::PlanSnapshot { .. }))
            .count();
        assert!(!app.process_event(AgentEvent::new(
            &session_id,
            app.last_sequence + 1,
            AgentEventKind::PlanProposed { plan: invalid },
        )));
        assert!(!app.process_event(AgentEvent::new(
            "foreign_session",
            app.last_sequence + 1,
            AgentEventKind::PlanProposed { plan: plan.clone() },
        )));
        assert_eq!(app.last_sequence, last_valid_sequence);
        app.publish_transcript();
        assert_eq!(
            app.view
                .transcript
                .iter()
                .filter(|item| matches!(item, TranscriptItem::PlanSnapshot { .. }))
                .count(),
            snapshot_count
        );
        assert_eq!(app.view.session_plans.len(), 1);
        assert_eq!(app.view.session_plans[&session_id].snapshot, plan);
        assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
        assert_no_committed_response(&app, &session_id);
        fixture.next_delta();
        fixture.terminal();
        wait_app(&mut app, |app| app.active_run.is_none());
        // The raw publication did not grant durable state or Build authority.
        assert!(!app.view.session_plans.contains_key(&session_id));
        assert!(!app.process_event(AgentEvent::new(
            &session_id,
            app.last_sequence + 1,
            AgentEventKind::PlanProposed { plan },
        )));
        assert!(!app.view.session_plans.contains_key(&session_id));
        fixture.assert_one_native_request();
    }
}

#[test]
fn streaming_layout_does_not_inherit_tail_across_owners_and_keeps_its_own_after_a_turn() {
    let root = tempfile::tempdir().unwrap();
    let first = (0..36)
        .map(|index| format!("Fila retenida {index}: conversación original.\n\n"))
        .collect::<String>();
    let fixture = StreamingHttpFixture::new(first.clone(), SECOND_VISIBLE.into());
    let (mut app, session_id) = app_for_stream(root.path(), &fixture);
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(first.as_str(), true)]
    });
    let viewport = Size::new(960.0, 620.0);
    native_frame(&mut app);
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    app.tree.set_reduced_motion(false);
    app.settings_preferences.reduced_motion = false;
    app.reduced_motion = false;
    fixture.next_delta();
    let expected = format!("{first}{SECOND_VISIBLE}");
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(expected.as_str(), true)]
    });
    let reveal_at = Instant::now();
    native_frame_at(&mut app, viewport, reveal_at);
    assert!(app.view.stream_presentation.has_pending());
    let previous_offset = app.tree.scroll_offset("transcript");
    app.view.selected_session = Some("different_visible_owner".into());
    native_frame_at(
        &mut app,
        Size::new(viewport.width, viewport.height - 100.0),
        reveal_at,
    );
    assert!((app.tree.scroll_offset("transcript") - previous_offset).abs() < 0.1);
    assert!(!app.tree.is_scroll_at_end("transcript", 24.0));
    // Once this owner has its own end position, it keeps its own end.
    app.tree.scroll_to_end("transcript");
    native_frame_at(
        &mut app,
        Size::new(viewport.width, viewport.height - 100.0),
        Instant::now(),
    );
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    native_frame_at(
        &mut app,
        Size::new(viewport.width, viewport.height - 200.0),
        Instant::now(),
    );
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    app.view.selected_session = Some(session_id);
    native_frame(&mut app);
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    let terminal_frame = native_frame(&mut app);
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    assert!(visible(&terminal_frame, SECOND_VISIBLE));
    // An ended turn read at its end stays there when the view narrows.
    native_frame_at(
        &mut app,
        Size::new(viewport.width, viewport.height - 100.0),
        Instant::now(),
    );
    assert!(app.tree.is_scroll_at_end("transcript", 0.1));
    fixture.assert_one_native_request();
}

#[test]
fn streaming_http_burst_keeps_terminal_behind_live_ui_backlog_without_losing_deltas() {
    let root = tempfile::tempdir().unwrap();
    let burst: Vec<String> = (0..96).map(|index| format!(" [{index:02}]ñ")).collect();
    let expected = format!("{FIRST_VISIBLE}{}", burst.concat());
    let fixture = StreamingHttpFixture::with_second_deltas(FIRST_VISIBLE.into(), burst);
    let (mut app, session_id) = app_for_stream(root.path(), &fixture);
    wait_app(&mut app, |app| {
        assistant_rows(app) == vec![(FIRST_VISIBLE, true)]
    });
    assert!(visible(&native_frame(&mut app), FIRST_VISIBLE));
    assert_no_committed_response(&app, &session_id);
    fixture.next_delta();
    // Deliberately stop polling the host while the real runner fills its
    // bounded UI channel. It must stop publishing before the terminal,
    // rather than journaling an unbounded burst behind a frozen consumer.
    wait_observation(|| fixture.second_flushed.load(Ordering::Acquire));
    fixture.terminal();
    wait_observation(|| fixture.terminal_flushed.load(Ordering::Acquire));
    wait_observation(|| {
        app.store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap()
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::AssistantDelta { .. }))
            .count()
            > MAX_RUN_UPDATES_PER_FRAME
    });
    assert!(fixture.terminal_flushed.load(Ordering::Acquire));
    let saturated = app
        .store
        .as_ref()
        .unwrap()
        .load(&session_id)
        .unwrap()
        .unwrap();
    assert!(
        !saturated.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelResponseCommitted { .. } | AgentEventKind::TurnCompleted { .. }
        )),
        "a bounded UI backlog must backpressure the response and terminal commit"
    );
    assert_eq!(assistant_rows(&app), vec![(FIRST_VISIBLE, true)]);
    app.poll_run();
    let partial = assistant_rows(&app)[0].0.to_owned();
    assert!(partial.starts_with(FIRST_VISIBLE));
    assert!(partial.len() > FIRST_VISIBLE.len());
    assert!(
        partial.len() < expected.len(),
        "one poll consumed the entire burst"
    );
    assert_eq!(assistant_rows(&app), vec![(partial.as_str(), true)]);
    assert!(
        app.view.running,
        "a later terminal must not overtake queued deltas"
    );
    assert!(app.active_run.is_some());
    assert!(app.history.is_empty());
    native_frame(&mut app);
    wait_app(&mut app, |app| app.active_run.is_none());
    assert_eq!(assistant_rows(&app), vec![(expected.as_str(), false)]);
    let stored = app
        .store
        .as_ref()
        .unwrap()
        .load(&session_id)
        .unwrap()
        .unwrap();
    assert_one_admission_without_tools(&stored.events);
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::AssistantDelta { .. }))
            .count(),
        97
    );
    let history = vec![
        AgentMessage::user(PROMPT),
        AgentMessage::assistant(&expected, Vec::new()),
    ];
    assert_eq!(app.history, history);
    assert_eq!(
        replay_stored_native(&app.data_root, &stored)
            .unwrap()
            .history,
        history
    );
    fixture.assert_one_native_request();
}

#[test]
fn streaming_http_eof_and_truncated_body_preserve_visible_prefix_without_commit_or_retry_on_reopen()
{
    for clean_http in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
        let (mut app, session_id) = app_for_stream(root.path(), &fixture);
        wait_app(&mut app, |app| {
            assistant_rows(app) == vec![(FIRST_VISIBLE, true)]
        });
        assert!(visible(&native_frame(&mut app), FIRST_VISIBLE));
        assert_no_committed_response(&app, &session_id);
        fixture.truncate(clean_http);
        wait_app(&mut app, |app| app.active_run.is_none());
        assert!(!fixture.second_flushed.load(Ordering::Acquire));
        assert!(!fixture.terminal_flushed.load(Ordering::Acquire));
        assert!(!app.view.running);
        assert_eq!(assistant_rows(&app), vec![(FIRST_VISIBLE, false)]);
        assert!(app.history.is_empty());
        let stored = app
            .store
            .as_ref()
            .unwrap()
            .load(&session_id)
            .unwrap()
            .unwrap();
        assert_one_admission_without_tools(&stored.events);
        let failures: Vec<_> = stored
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::TurnFailed { error } => Some(error.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(failures.len(), 1);
        if clean_http {
            assert!(
                failures[0].contains("streaming response ended before [DONE]"),
                "{}",
                failures[0]
            );
        } else {
            assert!(failures[0].contains("response body"), "{}", failures[0]);
        }
        assert!(!stored.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelResponseCommitted { .. }
                | AgentEventKind::AssistantMessage { .. }
                | AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnCancelled
        )));
        let failed_frame = native_frame(&mut app);
        assert!(visible(&failed_frame, FIRST_VISIBLE));
        assert!(visible(&failed_frame, "Error en la conversación"));
        assert!(
            failed_frame
                .text_layouts()
                .iter()
                .any(|block| block.source == failures[0])
        );
        assert!(!visible(&failed_frame, "Respondiendo"));
        let replay = replay_stored_native(&app.data_root, &stored).unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert!(replay.history.is_empty());
        let original_events = stored.events;
        drop(app);
        let mut reopened = AgentsWindow::with_data_root(root.path().join("data"));
        reopened.open_session(&session_id);
        reopened.view.sidebar_collapsed = true;
        reopened.view.show_side_pane = false;
        reopened.tree.set_reduced_motion(true);
        let reopened_frame = native_frame(&mut reopened);
        assert!(visible(&reopened_frame, FIRST_VISIBLE));
        assert!(visible(&reopened_frame, "Error en la conversación"));
        assert!(!visible(&reopened_frame, "Respondiendo"));
        assert_eq!(assistant_rows(&reopened), vec![(FIRST_VISIBLE, false)]);
        assert!(reopened.history.is_empty());
        assert!(reopened.active_run.is_none());
        assert!(reopened.active_resume.is_none());
        reopened.poll_run();
        assert_eq!(
            reopened
                .store
                .as_ref()
                .unwrap()
                .load(&session_id)
                .unwrap()
                .unwrap()
                .events,
            original_events
        );
        fixture.assert_one_native_request();
    }
}

/// Starts a turn like `app_for_stream`, with Claude Code hooks enabled first.
#[cfg(unix)]
fn app_with_hooks(root: &Path, fixture: &StreamingHttpFixture, hooks: Value) -> (AgentsWindow, String) {
    let source = root.join("claude-settings.json");
    std::fs::write(&source, json!({ "hooks": hooks }).to_string()).unwrap();
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.compat_hook_source = Some(source);
    app.view.workspace = root.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root, "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);
    // Every listed hook is switched on the way the user would do it.
    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Hooks.action_id().into() }, &ctx);
    for index in 0..app.view.compat_hooks.len() {
        app.apply_action(UiAction::Clicked { id: format!("compat_hook:{index}") }, &ctx);
    }
    assert!(app.view.compat_hooks.iter().all(|hook| hook.enabled), "{:?}", app.view.compat_hooks);
    app.view.show_settings = false;
    (app, session.id)
}

#[cfg(unix)]
fn command_hook(command: &str) -> Value {
    json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }])
}

#[cfg(unix)]
#[test]
fn hooks_e2e_session_and_prompt_context_reach_the_model_and_a_stop_hook_asks_to_continue() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let counter = root.path().join("session-starts");
    let (mut app, session_id) = app_with_hooks(
        root.path(),
        &fixture,
        json!({
            "SessionStart": command_hook(&format!(
                "cat >/dev/null; echo run >> '{}'; printf 'REGLAS_DE_SESION_ACTIVAS'",
                counter.display()
            )),
            "UserPromptSubmit": command_hook(
                "cat >/dev/null; printf '{\"hookSpecificOutput\":{\"additionalContext\":\"MODO_CAMBIADO\"}}'"
            ),
            "Stop": command_hook("cat >/dev/null; echo 'faltan tests para el cambio' >&2; exit 2"),
        }),
    );
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());

    // What the model actually received.
    fixture.assert_one_native_request();
    let body = fixture.requests.lock().unwrap()[0].1.clone();
    let messages = body["messages"].as_array().unwrap();
    let text_of = |role: &str| {
        messages
            .iter()
            .filter(|message| message["role"] == role)
            .map(|message| message["content"].to_string())
            .collect::<String>()
    };
    let system = text_of("system");
    assert!(system.contains("Contexto de hooks de inicio de sesión") && system.contains("REGLAS_DE_SESION_ACTIVAS"), "{system}");
    let user = text_of("user");
    assert!(user.contains(PROMPT) && user.contains("MODO_CAMBIADO"), "{user}");
    assert!(!system.contains("MODO_CAMBIADO") && !user.contains("REGLAS_DE_SESION_ACTIVAS"));

    // The stop hook ran after the turn and asked to continue: a notice, no new turn.
    let deadline = Instant::now() + DEADLINE;
    while app.turn_check_run.is_some() {
        app.poll_turn_check();
        assert!(Instant::now() < deadline, "stop hook did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    let failure = app.view.turn_check_failure.clone().expect("stop hook block was not shown");
    assert!(failure.contains("faltan tests para el cambio"), "{failure}");
    assert!(app.active_run.is_none() && !app.view.running);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1, "nothing may start a turn by itself");

    // The journal holds the prompt with its hook context, so a reopened
    // conversation replays exactly what the model saw.
    let stored = app.store.as_ref().unwrap().load(&session_id).unwrap().unwrap();
    assert!(stored.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::TurnAdmitted { prompt, .. } if prompt.contains("MODO_CAMBIADO")
    )));
    assert_eq!(std::fs::read_to_string(&counter).unwrap().lines().count(), 1);
}

#[cfg(unix)]
#[test]
fn hooks_e2e_a_prompt_hook_refusal_keeps_the_draft_and_starts_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, session_id) = app_with_hooks(
        root.path(),
        &fixture,
        json!({ "UserPromptSubmit": command_hook("cat >/dev/null; echo 'contiene un secreto' >&2; exit 2") }),
    );
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(app.active_run.is_none());
    assert_eq!(app.view.prompt, PROMPT);
    let error = app.view.composer_error.clone().unwrap();
    assert!(error.contains("contiene un secreto"), "{error}");
    assert!(fixture.requests.lock().unwrap().is_empty());
    let stored = app.store.as_ref().unwrap().load(&session_id).unwrap().unwrap();
    assert!(!stored.events.iter().any(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. })));
}

#[cfg(unix)]
#[test]
fn hooks_e2e_an_enabled_plugins_session_hook_runs_from_its_own_folder_and_reaches_the_model() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    // A Claude Code config folder with one installed, enabled plugin. Its hook
    // reads a file that only exists inside the plugin's own folder.
    let config = root.path().join("claude");
    let plugin = config.join("plugins/cache/market/rules/1.0.0");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin.join("hooks")).unwrap();
    std::fs::write(plugin.join("rules.txt"), "REGLAS_DEL_PLUGIN").unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"rules","version":"1.0.0","hooks":"./hooks/session.json"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("hooks/session.json"),
        json!({"hooks":{"SessionStart":[{"matcher":"startup|resume|clear|compact","hooks":[{
            "type":"command","timeout":5,
            "command":"cat >/dev/null; cat \"$CLAUDE_PLUGIN_ROOT/rules.txt\""
        }]}]}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        config.join("plugins/installed_plugins.json"),
        json!({"version":2,"plugins":{"rules@market":[{"scope":"user","installPath":plugin,"version":"1.0.0"}]}}).to_string(),
    )
    .unwrap();
    let settings = config.join("settings.json");
    std::fs::write(&settings, r#"{"enabledPlugins":{"rules@market":true}}"#).unwrap();

    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.compat_hook_source = Some(settings);
    app.view.workspace = root.path().to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);

    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Hooks.action_id().into() }, &ctx);
    assert_eq!(app.view.compat_hooks.len(), 1);
    assert_eq!(app.view.compat_hooks[0].origin, "rules@market");
    assert!(!app.view.compat_hooks[0].enabled, "a plugin hook must start switched off");

    // Switched off: the turn carries nothing from the plugin.
    app.view.show_settings = false;
    app.view.prompt = PROMPT.into();
    app.start_run();
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    let before = fixture.requests.lock().unwrap()[0].1.to_string();
    assert!(!before.contains("REGLAS_DEL_PLUGIN"));
    drop(fixture);

    // Switched on: the next conversation's first request has the plugin's text.
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    app.view.endpoint = fixture.endpoint.clone();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: "compat_hook:0".into() }, &ctx);
    assert!(app.view.compat_hooks[0].enabled);
    app.view.show_settings = false;
    let second = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&second.id);
    native_frame(&mut app);
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    let body = fixture.requests.lock().unwrap()[0].1.clone();
    let system: String = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .map(|message| message["content"].to_string())
        .collect();
    assert!(system.contains("REGLAS_DEL_PLUGIN"), "{system}");
}

#[cfg(unix)]
#[test]
fn skills_e2e_an_enabled_plugins_skills_are_indexed_for_the_model_without_their_bodies() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let config = root.path().join("claude");
    let plugin = config.join("plugins/cache/market/flow/2.0.0");
    let skill = plugin.join("skills/spec-first");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(plugin.join(".claude-plugin/plugin.json"), r#"{"name":"flow","skills":"./skills"}"#).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: spec-first\ndescription: Use when a feature has no written spec yet.\n---\nCUERPO_PRIVADO_DE_LA_SKILL\n",
    )
    .unwrap();
    std::fs::write(
        config.join("plugins/installed_plugins.json"),
        json!({"version":2,"plugins":{"flow@market":[{"installPath":plugin}]}}).to_string(),
    )
    .unwrap();
    let settings = config.join("settings.json");
    std::fs::write(&settings, r#"{"enabledPlugins":{"flow@market":true}}"#).unwrap();

    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.compat_hook_source = Some(settings);
    app.view.workspace = root.path().to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);

    // Off by default: no read access to the plugin and nothing in the index.
    let workspace = root.path().to_path_buf();
    let granted = |app: &AgentsWindow| {
        crate::marketplace::active_skill_directories(&app.data_root, &workspace).unwrap()
    };
    assert!(granted(&app).is_empty());

    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Skills.action_id().into() }, &ctx);
    assert_eq!(app.view.plugin_skills.len(), 1);
    assert_eq!((app.view.plugin_skills[0].id.as_str(), app.view.plugin_skills[0].skill_count), ("flow@market", 1));
    assert!(!app.view.plugin_skills[0].enabled);
    app.apply_action(UiAction::Clicked { id: "plugin_skills:0".into() }, &ctx);
    assert!(app.view.plugin_skills[0].enabled, "{:?}", app.view.settings_notice);
    app.view.show_settings = false;
    assert_eq!(granted(&app), vec![skill.canonicalize().unwrap()]);

    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());

    let body = fixture.requests.lock().unwrap()[0].1.clone();
    let system: String = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str().map(str::to_owned))
        .collect();
    // The model learns the skill exists, how commands name it and where to read it.
    assert!(system.contains("flow:spec-first"), "{system}");
    assert!(system.contains("Use when a feature has no written spec yet."));
    assert!(system.contains(&skill.canonicalize().unwrap().join("SKILL.md").display().to_string()));
    // Its instructions are loaded on demand, not sent with every turn.
    assert!(!system.contains("CUERPO_PRIVADO_DE_LA_SKILL"));
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[cfg(unix)]
#[test]
fn review_e2e_runs_in_its_own_read_only_conversation_with_the_change_and_keeps_the_draft() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("parser.rs"), "fn parse() {}\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "base"]);

    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    // Writes and commands are allowed, so only the review's own mode can
    // keep them away from the reviewer.
    let ctx = egui::Context::default();
    app.apply_action(UiAction::Clicked { id: "write_mode".into() }, &ctx);
    assert!(app.view.workspace_writes_enabled);
    let working = app.store.as_ref().unwrap().create(&project, "native").unwrap();
    app.open_session(&working.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);

    // Nothing to review: refused, nothing sent, the user stays where they were.
    app.start_isolated_review();
    assert!(app.view.composer_error.as_deref().is_some_and(|error| error.contains("No hay cambios")));
    assert_eq!(app.view.selected_session.as_deref(), Some(working.id.as_str()));
    assert!(app.active_run.is_none() && fixture.requests.lock().unwrap().is_empty());

    std::fs::write(project.join("parser.rs"), "fn parse() { todo!() } // CAMBIO_A_REVISAR\n").unwrap();
    app.view.composer_error = None;
    app.start_isolated_review();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    let review_session = app.view.selected_session.clone().unwrap();
    assert_ne!(review_session, working.id, "the review must not run inside the working conversation");
    assert_eq!(app.view.turn_mode, TurnMode::Plan);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());

    // What the reviewer's model received: the change, and no way to alter it.
    fixture.assert_one_native_request();
    let body = fixture.requests.lock().unwrap()[0].1.clone();
    let user: String = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].to_string())
        .collect();
    assert!(user.contains("CAMBIO_A_REVISAR") && user.contains("parser.rs"), "{user}");
    assert!(user.contains("Do not change anything"));
    let tools: Vec<String> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().or(tool["name"].as_str()).map(str::to_owned))
        .collect();
    assert!(tools.iter().any(|name| name == "read_file"), "{tools:?}");
    for forbidden in ["write_file", "run_command", "apply_patch", "edit_file", "spawn_agent"] {
        assert!(!tools.iter().any(|name| name == forbidden), "{forbidden} offered to the reviewer: {tools:?}");
    }

    // The journal agrees, and the working conversation is untouched.
    let store = app.store.as_ref().unwrap();
    let reviewed = store.load(&review_session).unwrap().unwrap();
    assert!(reviewed.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::TurnAdmitted { mode: TurnMode::Plan, prompt, .. } if prompt.contains("CAMBIO_A_REVISAR")
    )));
    let untouched = store.load(&working.id).unwrap().unwrap();
    assert!(!untouched.events.iter().any(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. })));
    assert_eq!(std::fs::read_to_string(project.join("parser.rs")).unwrap(), "fn parse() { todo!() } // CAMBIO_A_REVISAR\n");
}

#[cfg(unix)]
#[test]
fn review_e2e_drafts_on_both_sides_survive_a_review_and_none_of_them_is_sent() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("a.txt"), "ARCHIVO_NUEVO\n").unwrap();

    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;

    // A draft is already waiting in the not-yet-created conversation...
    app.new_session();
    app.view.prompt = "BORRADOR_DE_CONVERSACION_NUEVA".into();
    app.save_draft();
    // ...and another in the conversation the user is working in.
    let working = app.store.as_ref().unwrap().create(&project, "native").unwrap();
    app.open_session(&working.id);
    native_frame(&mut app);
    app.view.prompt = "BORRADOR_DE_TRABAJO".into();
    app.save_draft();

    app.start_isolated_review();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    let review_session = app.view.selected_session.clone().unwrap();
    assert_ne!(review_session, working.id);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());

    // The reviewer got the change and neither draft.
    let sent = fixture.requests.lock().unwrap()[0].1.to_string();
    assert!(sent.contains("a.txt"), "{sent}");
    assert!(!sent.contains("BORRADOR_DE_CONVERSACION_NUEVA") && !sent.contains("BORRADOR_DE_TRABAJO"));

    // Both drafts are where the user left them, also after the app restarts.
    app.open_session(&working.id);
    assert_eq!(app.view.prompt, "BORRADOR_DE_TRABAJO");
    app.new_session();
    assert_eq!(app.view.prompt, "BORRADOR_DE_CONVERSACION_NUEVA");
    let store = app.store.as_ref().unwrap();
    assert_eq!(store.load_draft(&project, None).unwrap().as_deref(), Some("BORRADOR_DE_CONVERSACION_NUEVA"));
    assert_eq!(store.load_draft(&project, Some(&working.id)).unwrap().as_deref(), Some("BORRADOR_DE_TRABAJO"));
}

#[cfg(unix)]
#[test]
fn review_e2e_a_review_that_cannot_start_gives_the_waiting_draft_back() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("a.txt"), "x\n").unwrap();
    // No model is configured, so nothing can be sent.
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    app.new_session();
    app.view.prompt = "BORRADOR_DE_CONVERSACION_NUEVA".into();
    app.save_draft();

    app.start_isolated_review();
    assert!(app.active_run.is_none());
    assert_eq!(app.view.prompt, "BORRADOR_DE_CONVERSACION_NUEVA");
    assert_eq!(app.view.turn_mode, TurnMode::Build);
    assert!(app.view.composer_error.is_some(), "the user must be told why nothing started");
}

/// A model that answers by what it is asked, because a parent and the child
/// it spawned call it concurrently. Every request body is kept.
#[cfg(unix)]
fn spawning_model_server(child_prompt: &'static str) -> (String, Arc<Mutex<Vec<Value>>>, std::thread::JoinHandle<()>) {
    use super::native_child_commands_app_tests::{request, response, tool};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let server = std::thread::spawn(move || {
        // Parent asks, child asks, parent asks again with the spawn result.
        for _ in 0..3 {
            let deadline = Instant::now() + DEADLINE;
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            let body = request(&mut socket);
            let text = body["messages"].to_string();
            let is_child = text.contains(child_prompt) && !text.contains(PROMPT);
            let has_tool_result = body["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"));
            seen.lock().unwrap().push(body);
            if is_child {
                response(&mut socket, json!({"content":"hallazgos del hijo"}), "stop");
            } else if has_tool_result {
                response(&mut socket, json!({"content":"padre terminado"}), "stop");
            } else {
                response(
                    &mut socket,
                    tool("spawn-1", "spawn_agent", json!({"prompt": child_prompt, "role": "explorer"})),
                    "tool_calls",
                );
            }
        }
    });
    (endpoint, requests, server)
}

#[cfg(unix)]
#[test]
fn hooks_e2e_a_spawned_child_receives_session_rules_and_subagent_context_from_real_hooks() {
    const CHILD_PROMPT: &str = "localiza el parser del proyecto";
    let root = tempfile::tempdir().unwrap();
    let (endpoint, requests, server) = spawning_model_server(CHILD_PROMPT);
    let source = root.path().join("claude-settings.json");
    std::fs::write(
        &source,
        json!({"hooks":{
            "SessionStart": command_hook("cat >/dev/null; printf 'REGLAS_DE_SESION'"),
            "SubagentStart": command_hook(
                "cat >/dev/null; printf '{\"hookSpecificOutput\":{\"additionalContext\":\"SOLO_PARA_HIJOS\"}}'"
            ),
        }})
        .to_string(),
    )
    .unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.compat_hook_source = Some(source);
    app.view.workspace = root.path().to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = endpoint;
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);
    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Hooks.action_id().into() }, &ctx);
    for index in 0..app.view.compat_hooks.len() {
        app.apply_action(UiAction::Clicked { id: format!("compat_hook:{index}") }, &ctx);
    }
    assert!(app.view.compat_hooks.iter().all(|hook| hook.enabled) && app.view.compat_hooks.len() == 2);
    app.view.show_settings = false;

    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    // The spawn may ask for approval; grant whatever the parent asks for.
    let deadline = Instant::now() + DEADLINE;
    while requests.lock().unwrap().len() < 3 {
        app.poll_run();
        for permission in app.view.pending_permissions.clone() {
            app.apply_action(UiAction::Clicked { id: format!("permission_allow:{}", permission.request_id) }, &ctx);
        }
        assert!(Instant::now() < deadline, "model saw {} requests: {}", requests.lock().unwrap().len(), app.view.status);
        std::thread::sleep(Duration::from_millis(5));
    }
    server.join().unwrap();
    wait_app(&mut app, |app| app.active_run.is_none());

    let requests = requests.lock().unwrap().clone();
    let system_of = |body: &Value| -> String {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .map(|message| message["content"].to_string())
            .collect()
    };
    let parent = requests.iter().find(|body| body["messages"].to_string().contains(PROMPT)).unwrap();
    let child = requests
        .iter()
        .find(|body| {
            let text = body["messages"].to_string();
            text.contains(CHILD_PROMPT) && !text.contains(PROMPT)
        })
        .expect("the child never called the model");

    // The parent has the session rules and nothing meant for children.
    assert!(system_of(parent).contains("REGLAS_DE_SESION"));
    assert!(!system_of(parent).contains("SOLO_PARA_HIJOS"));
    // The child inherits the session rules and gets the subagent hook's text.
    let child_system = system_of(child);
    assert!(child_system.contains("REGLAS_DE_SESION"), "{child_system}");
    assert!(child_system.contains("SOLO_PARA_HIJOS"), "{child_system}");
    // And it really is the narrowed child the role asked for.
    assert!(child["messages"].to_string().contains("You are an explorer subagent"));
    let child_tools = child["tools"].to_string();
    assert!(child_tools.contains("read_file") && !child_tools.contains("write_file") && !child_tools.contains("run_command"));
}

#[cfg(unix)]
#[test]
fn plugin_e2e_a_command_expands_in_the_composer_and_a_persona_is_offered_to_the_model() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let config = root.path().join("claude");
    let plugin = config.join("plugins/cache/market/flow/2.0.0");
    for sub in [".claude-plugin", "skills/spec-first", ".claude/commands", "agents"] {
        std::fs::create_dir_all(plugin.join(sub)).unwrap();
    }
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"flow","skills":"./skills","commands":["./.claude/commands"]}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join("skills/spec-first/SKILL.md"),
        "---\nname: spec-first\ndescription: Use when there is no spec.\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join(".claude/commands/spec.md"),
        "---\ndescription: Write a spec before code\n---\n\nInvoke the flow:spec-first skill.\n\n$ARGUMENTS\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("agents/code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews a change for defects\n---\nINSTRUCCIONES_PRIVADAS_DE_LA_PERSONA\n",
    )
    .unwrap();
    std::fs::write(
        config.join("plugins/installed_plugins.json"),
        json!({"version":2,"plugins":{"flow@market":[{"installPath":plugin}]}}).to_string(),
    )
    .unwrap();
    let settings = config.join("settings.json");
    std::fs::write(&settings, r#"{"enabledPlugins":{"flow@market":true}}"#).unwrap();

    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.compat_hook_source = Some(settings);
    app.view.workspace = root.path().to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = fixture.endpoint.clone();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    app.view.turn_mode = TurnMode::Build;
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    native_frame(&mut app);
    let ctx = egui::Context::default();
    let workspace = root.path().to_path_buf();

    // Looks up `/flo` the way the composer does and returns the labels offered.
    let lookup = |app: &mut AgentsWindow| -> Vec<String> {
        app.view.prompt = "/flo".into();
        app.tree.set_input_value("prompt", "/flo");
        let trigger = crate::composer_suggestions::trigger_at_cursor("/flo", 4);
        app.view.composer_suggestions.sync(&workspace, trigger);
        app.view.composer_suggestions.plugin_commands =
            crate::compat_hook_settings::enabled_plugin_roots(&app.data_root)
                .iter()
                .flat_map(|(id, root)| crate::compat_hook_settings::plugin_commands(id, root))
                .collect();
        let skills = app.view.native_skills.clone();
        app.view.composer_suggestions.populate_commands(&skills);
        app.view.composer_suggestions.items.iter().map(|item| item.label.clone()).collect()
    };

    // The plugin is off: none of it is offered. (Not dismissed here: the
    // composer keeps a dismissed token closed until the text changes.)
    assert!(lookup(&mut app).is_empty());

    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Skills.action_id().into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "plugin_skills:0".into() }, &ctx);
    assert!(app.view.plugin_skills[0].enabled, "{:?}", app.view.settings_notice);
    app.view.show_settings = false;

    let labels = lookup(&mut app);
    assert_eq!(labels, vec!["/flow:spec".to_owned()]);
    let generation = app.view.composer_suggestions.generation;
    app.accept_composer_suggestion(generation, 0, &ctx);
    // The command's prompt is in the composer, with its placeholder gone, and nothing was sent.
    assert_eq!(app.view.prompt, "Invoke the flow:spec-first skill.\n\n");
    assert!(app.active_run.is_none() && fixture.requests.lock().unwrap().is_empty());

    app.view.prompt.push_str("un exportador CSV");
    app.tree.set_input_value("prompt", app.view.prompt.clone());
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());

    let body = fixture.requests.lock().unwrap()[0].1.clone();
    let text_of = |role: &str| -> String {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == role)
            .filter_map(|message| message["content"].as_str().map(str::to_owned))
            .collect()
    };
    let user = text_of("user");
    assert!(user.contains("Invoke the flow:spec-first skill.") && user.contains("un exportador CSV"), "{user}");
    let system = text_of("system");
    // The skill the command names is in the index under that same name.
    assert!(system.contains("flow:spec-first"), "{system}");
    // The persona is offered by name and file, with how to delegate to it, without its body.
    assert!(system.contains("agent_persona") && system.contains("flow:code-reviewer"));
    assert!(system.contains(&plugin.canonicalize().unwrap().join("agents/code-reviewer.md").display().to_string()));
    assert!(system.contains("call spawn_agent"));
    assert!(!system.contains("INSTRUCCIONES_PRIVADAS_DE_LA_PERSONA"));
    // And the agent may read it: the personas folder is one of its granted roots.
    let granted = crate::marketplace::active_skill_directories(&app.data_root, &workspace).unwrap();
    assert!(granted.contains(&plugin.canonicalize().unwrap().join("agents")), "{granted:?}");
}

#[cfg(unix)]
#[test]
fn plugin_e2e_an_update_is_followed_on_the_next_turn_and_disabling_in_claude_code_switches_it_off() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("claude");
    let version = |number: &str, marker: &str| {
        let plugin = config.join("plugins/cache/market/flow").join(number);
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(plugin.join("skills/spec-first")).unwrap();
        std::fs::write(plugin.join(".claude-plugin/plugin.json"), r#"{"name":"flow"}"#).unwrap();
        std::fs::write(
            plugin.join("skills/spec-first/SKILL.md"),
            format!("---\nname: spec-first\ndescription: {marker}\n---\nbody\n"),
        )
        .unwrap();
        plugin
    };
    let install = |plugin: &Path| {
        std::fs::write(
            config.join("plugins/installed_plugins.json"),
            json!({"version":2,"plugins":{"flow@market":[{"installPath":plugin}]}}).to_string(),
        )
        .unwrap();
    };
    let first = version("1.0.0", "DESCRIPCION_V1");
    install(&first);
    let settings = config.join("settings.json");
    std::fs::write(&settings, r#"{"enabledPlugins":{"flow@market":true}}"#).unwrap();

    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.compat_hook_source = Some(settings.clone());
    app.view.workspace = root.path().to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.composer_effort = "default".into();
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Skills.action_id().into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "plugin_skills:0".into() }, &ctx);
    assert!(app.view.plugin_skills[0].enabled);
    app.view.show_settings = false;

    // Runs one turn in a new conversation and returns the model's system text.
    let turn = |app: &mut AgentsWindow| -> String {
        let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
        app.view.endpoint = fixture.endpoint.clone();
        assert!(app.persist_native_settings(), "{}", app.view.status);
        let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
        app.open_session(&session.id);
        app.view.turn_mode = TurnMode::Build;
        native_frame(app);
        app.view.prompt = PROMPT.into();
        app.start_run();
        assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
        wait_app(app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
        fixture.next_delta();
        fixture.terminal();
        wait_app(app, |app| app.active_run.is_none());
        let body = fixture.requests.lock().unwrap()[0].1.clone();
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .filter_map(|message| message["content"].as_str().map(str::to_owned))
            .collect()
    };
    assert!(turn(&mut app).contains("DESCRIPCION_V1"));

    // Claude Code updates the plugin into a new folder. Nobody opens Settings.
    let second = version("2.0.0", "DESCRIPCION_V2");
    install(&second);
    let after_update = turn(&mut app);
    assert!(after_update.contains("DESCRIPCION_V2") && !after_update.contains("DESCRIPCION_V1"), "{after_update}");
    assert!(after_update.contains(&second.canonicalize().unwrap().display().to_string()));

    // The user disables the plugin in Claude Code. Its files are still on disk.
    std::fs::write(&settings, r#"{"enabledPlugins":{"flow@market":false}}"#).unwrap();
    let after_disable = turn(&mut app);
    assert!(!after_disable.contains("DESCRIPCION_V2") && !after_disable.contains("flow:spec-first"), "{after_disable}");
    assert!(crate::compat_hook_settings::enabled_plugin_roots(&app.data_root).is_empty());

    // Enabling it again in Claude Code does not switch it back on here by itself.
    std::fs::write(&settings, r#"{"enabledPlugins":{"flow@market":true}}"#).unwrap();
    assert!(!turn(&mut app).contains("flow:spec-first"));
}

/// A project with one uncommitted change and an app pointed at `endpoint`
/// with `MODEL` selected.
#[cfg(unix)]
fn review_model_app(root: &Path, endpoint: &str) -> (AgentsWindow, PathBuf) {
    let project = root.join("project");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("a.txt"), "cambio\n").unwrap();
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = endpoint.into();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    (app, project)
}

/// Runs whatever turn is active to completion and returns the model named
/// in the request the stub server received.
#[cfg(unix)]
fn finish_turn(app: &mut AgentsWindow, fixture: &StreamingHttpFixture) -> String {
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(app, |app| app.active_run.is_none());
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    requests[0].1["model"].as_str().unwrap().to_owned()
}

#[cfg(unix)]
#[test]
fn review_model_e2e_manual_choice_runs_the_review_and_leaves_the_users_selection_alone() {
    const REVIEW_MODEL: &str = "streaming/model:reviewer";
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, project) = review_model_app(root.path(), &fixture.endpoint);
    let ctx = egui::Context::default();

    // The user picks the review model the way they pick any model, saves it
    // as the manual choice, and goes back to their working model.
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);
    app.view.model = REVIEW_MODEL.into();
    assert!(app.persist_native_settings());
    app.apply_action(UiAction::Clicked { id: "review_model_capture".into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "settings_choice_toggle:review_model".into() }, &ctx);
    assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some("review_model"));
    app.apply_action(UiAction::Clicked { id: "review_model:manual".into() }, &ctx);
    assert_eq!(app.view.settings_choice_menu_open, None);
    assert_eq!(app.view.review_model.mode, crate::review_model::ReviewModelMode::Manual);
    assert_eq!(app.view.review_model.manual, format!("custom · {REVIEW_MODEL}"));
    app.view.model = MODEL.into();
    assert!(app.persist_native_settings());
    app.view.show_settings = false;

    let working = app.store.as_ref().unwrap().create(&project, "native").unwrap();
    app.open_session(&working.id);
    native_frame(&mut app);
    app.start_isolated_review();
    assert_eq!(finish_turn(&mut app, &fixture), REVIEW_MODEL, "the review must run on the saved model");
    let review_session = app.view.selected_session.clone().unwrap();
    drop(fixture);

    // The selection, in memory and on disk, is still the working model.
    assert_eq!(app.view.model, MODEL);
    assert!(app.turn_model_override.is_none());
    assert_eq!(
        crate::native_provider::NativeProviderSettings::load(&app.data_root).model_id,
        MODEL
    );
    // The journal names the model that really ran, which is what resuming checks.
    let reviewed = app.store.as_ref().unwrap().load(&review_session).unwrap().unwrap();
    assert!(reviewed.events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::TurnAdmitted { model, .. } if model == REVIEW_MODEL
    )));

    // The next ordinary turn uses the working model: nothing leaked.
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    app.view.endpoint = fixture.endpoint.clone();
    assert!(app.persist_native_settings());
    app.open_session(&working.id);
    native_frame(&mut app);
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert_eq!(finish_turn(&mut app, &fixture), MODEL);
}

#[cfg(unix)]
#[test]
fn review_model_e2e_automatic_uses_the_catalogs_step_down_and_same_mode_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    let ctx = egui::Context::default();
    let priced = |id: &str, input: f64, output: f64| {
        json!({"id": id, "name": id, "tool_call": true, "tool_call_declared": true, "cost": {"input": input, "output": output}})
    };
    app.view.catalog = Some(Arc::new(
        serde_json::from_value(json!({"fetched_at_ms": 0, "providers": {"custom": {
            "id": "custom", "name": "Custom",
            "models": {
                MODEL: priced(MODEL, 10.0, 30.0),
                "streaming/model:mid": priced("streaming/model:mid", 4.0, 12.0),
                "streaming/model:tiny": priced("streaming/model:tiny", 0.1, 0.1),
            }
        }}}))
        .unwrap(),
    ));

    // Default mode: the review runs on the selected model.
    app.start_isolated_review();
    assert_eq!(finish_turn(&mut app, &fixture), MODEL);
    drop(fixture);

    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "review_model:automatic".into() }, &ctx);
    // Settings says what it would use before the user relies on it.
    assert_eq!(app.view.review_model.automatic, "custom · streaming/model:mid");
    app.view.show_settings = false;

    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    app.view.endpoint = fixture.endpoint.clone();
    assert!(app.persist_native_settings());
    app.start_isolated_review();
    assert_eq!(finish_turn(&mut app, &fixture), "streaming/model:mid");
    assert_eq!(app.view.model, MODEL);
}

#[cfg(unix)]
#[test]
fn review_model_e2e_manual_with_nothing_saved_starts_nothing_and_no_override_survives() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "review_model:manual".into() }, &ctx);
    app.view.show_settings = false;

    app.new_session();
    app.view.prompt = "BORRADOR_EN_ESPERA".into();
    app.save_draft();
    app.start_isolated_review();
    assert!(app.active_run.is_none());
    assert!(app.view.composer_error.as_deref().is_some_and(|error| error.contains("modelo de revisión")));
    assert_eq!(app.view.prompt, "BORRADOR_EN_ESPERA");
    assert_eq!(app.view.turn_mode, TurnMode::Build);
    assert!(app.turn_model_override.is_none());
    assert!(fixture.requests.lock().unwrap().is_empty());

    // An override left behind by anything is discarded by a turn that does not start...
    app.turn_model_override = Some(crate::native_provider::NativeProviderSettings {
        provider_id: "custom".into(),
        model_id: "streaming/model:stale".into(),
        custom_endpoint: fixture.endpoint.clone(),
    });
    app.view.prompt.clear();
    app.start_run();
    assert!(app.active_run.is_none() && app.turn_model_override.is_none());
    // ...so the next real turn runs on the selected model.
    app.view.composer_error = None;
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert_eq!(finish_turn(&mut app, &fixture), MODEL);
}

/// Selects `model` as the manual review model the way the user does, then
/// goes back to the working model.
#[cfg(unix)]
fn save_manual_review_model(app: &mut AgentsWindow, model: &str) {
    let ctx = egui::Context::default();
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);
    app.view.model = model.into();
    assert!(app.persist_native_settings());
    app.apply_action(UiAction::Clicked { id: "review_model_capture".into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "review_model:manual".into() }, &ctx);
    app.view.model = MODEL.into();
    assert!(app.persist_native_settings());
    app.view.show_settings = false;
}

/// Situation 1: an interrupted review resumes on the model it ran on, with
/// the working model still selected, and really asks that model.
#[cfg(unix)]
#[test]
fn review_situation_resuming_an_interrupted_review_asks_its_own_model_whatever_is_selected() {
    const REVIEW_MODEL: &str = "streaming/model:reviewer";
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    save_manual_review_model(&mut app, REVIEW_MODEL);
    app.start_isolated_review();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    let review_session = app.view.selected_session.clone().unwrap();
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    assert_eq!(fixture.requests.lock().unwrap()[0].1["model"], REVIEW_MODEL);
    let data_root = app.data_root.clone();
    drop(app);

    // What a process killed while waiting for the model leaves behind: the
    // journal up to the request, and no answer.
    let journal = data_root.join("sessions").join(format!("{review_session}.jsonl"));
    let text = std::fs::read_to_string(&journal).unwrap();
    let mut kept = String::new();
    for line in text.lines() {
        kept.push_str(line);
        kept.push('\n');
        if serde_json::from_str::<Value>(line).unwrap()["kind"]["type"] == "model_request_started" {
            break;
        }
    }
    assert!(kept.lines().count() < text.lines().count());
    std::fs::write(&journal, kept).unwrap();

    let mut reopened = AgentsWindow::with_data_root(data_root);
    reopened.tree.set_reduced_motion(true);
    reopened.reduced_motion = true;
    assert_eq!(reopened.view.model, MODEL, "the working model is still the selection after reopening");
    reopened.open_session(&review_session);

    // The working model stays selected, and the resume still asks the model
    // the interrupted turn was admitted on: the selection is for the next turn.
    reopened.start_resume();
    assert!(reopened.active_resume.is_some(), "{}", reopened.view.status);
    wait_observation(|| fixture.requests.lock().unwrap().len() == 2);
    assert_eq!(fixture.requests.lock().unwrap()[1].1["model"], REVIEW_MODEL);
    assert_eq!(reopened.view.model, MODEL);
}

/// Situation 3: the saved manual model belongs to a provider that is not
/// connected any more. The review must fail visibly, not move to another model.
#[cfg(unix)]
#[test]
fn review_situation_a_manual_model_on_a_disconnected_provider_fails_visibly_and_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    let ctx = egui::Context::default();
    // Saved earlier, when that provider was connected. No key exists for it here.
    let mut settings = crate::review_model::ReviewModelSettings::default();
    settings.mode = crate::review_model::ReviewModelMode::Manual;
    for (provider, model) in [("codex", "gpt-6-luna"), ("openai", "some-openai-model")] {
        // Saved earlier, when that provider was connected. Nothing for it exists here.
        let mut settings = crate::review_model::ReviewModelSettings::default();
        settings.mode = crate::review_model::ReviewModelMode::Manual;
        settings.manual = Some(crate::native_provider::NativeProviderSettings {
            provider_id: provider.into(),
            model_id: model.into(),
            custom_endpoint: String::new(),
        });
        settings.save(&app.data_root).unwrap();
        app.review_model_settings = settings;
        app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);

        app.new_session();
        app.view.composer_error = None;
        app.view.prompt = "BORRADOR_EN_ESPERA".into();
        app.save_draft();
        app.start_isolated_review();

        // Refused before a turn exists, so nothing can have been sent anywhere.
        assert!(app.active_run.is_none(), "{provider}: {}", app.view.status);
        let error = app.view.composer_error.clone().expect("the user must be told why");
        assert!(!error.is_empty(), "{provider}");
        assert_eq!(app.view.prompt, "BORRADOR_EN_ESPERA", "{provider}");
        let sessions = app.store.as_ref().unwrap().list().unwrap();
        assert!(sessions.is_empty(), "{provider}: a conversation was created for a review that never ran");
    }
    assert_eq!(app.view.prompt, "BORRADOR_EN_ESPERA");
    assert_eq!(app.view.turn_mode, TurnMode::Build);
    // Not sent to the working model instead, and the selection is untouched.
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert_eq!((app.view.native_provider_id.as_str(), app.view.model.as_str()), ("custom", MODEL));
    assert!(app.turn_model_override.is_none());
}

/// Situations 4 and 5: what the review conversation shows and inherits after
/// running on another model.
#[cfg(unix)]
#[test]
fn review_situation_context_indicator_and_effort_follow_the_selection_not_the_review_model() {
    const REVIEW_MODEL: &str = "streaming/model:reviewer";
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    save_manual_review_model(&mut app, REVIEW_MODEL);
    app.view.composer_effort = "default".into();
    app.start_isolated_review();
    assert_eq!(finish_turn(&mut app, &fixture), REVIEW_MODEL);

    // The effort the user had in the composer is the one the review used and
    // it is still there afterwards; there is no separate review effort.
    assert_eq!(app.view.composer_effort, "default");
    // The context indicator of the review conversation describes the selected
    // model, although the turn ran on the review model.
    app.refresh_composer_context_percent();
    assert_eq!(app.view.native_context.model, MODEL);
    assert_ne!(app.view.native_context.model, REVIEW_MODEL);
}

/// The picker in Settings: what it offers, what it refuses and what it saves.
/// Running a review on the saved manual model is covered by
/// `review_model_e2e_manual_choice_runs_the_review_and_leaves_the_users_selection_alone`;
/// the picker writes the same setting that test reads.
#[cfg(unix)]
#[test]
fn review_picker_e2e_offers_only_connected_providers_and_saves_a_usable_choice() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, _project) = review_model_app(root.path(), &fixture.endpoint);
    let ctx = egui::Context::default();
    let listed = |id: &str, index: usize| {
        json!({"id": id, "name": format!("Nombre {id}"), "listing_index": index, "tool_call": true, "tool_call_declared": true})
    };
    app.view.catalog = Some(Arc::new(
        serde_json::from_value(json!({"fetched_at_ms": 0, "providers": {
            "codex": {"id": "codex", "name": "Codex", "models": {
                // Listing order is the provider's, not alphabetical.
                "model-zulu": listed("model-zulu", 0),
                "model-alpha": listed("model-alpha", 1),
                "model-retired": {"id": "model-retired", "name": "Retirado", "status": "deprecated"},
            }},
            // In the catalog, but with no account in this data root.
            "openrouter": {"id": "openrouter", "name": "OpenRouter", "models": {"or-x": listed("or-x", 0)}},
            // A user's own endpoint has no catalog the Providers page selects from.
            "custom": {"id": "custom", "name": "Mi endpoint", "models": {MODEL: listed(MODEL, 0)}},
        }}))
        .unwrap(),
    ));
    // The app reloads connection state from disk when conversations change;
    // this stands in for a Codex account being connected each time it does.
    let connect_codex = |app: &mut AgentsWindow| {
        app.view.provider_connections.insert(
            "codex".into(),
            crate::provider_connections::ConnectionSummary { configured: true, oauth: true, ..Default::default() },
        );
    };
    connect_codex(&mut app);
    app.view.show_settings = true;
    app.apply_action(UiAction::Clicked { id: SettingsPage::Agent.action_id().into() }, &ctx);
    connect_codex(&mut app);

    // Only a connected provider is offered, with its usable models in its own order.
    let choices = crate::settings_ui::connected_model_choices(&app.view);
    let offered: Vec<(&str, Vec<&str>)> = choices
        .iter()
        .map(|provider| (provider.id.as_str(), provider.models.iter().map(|(id, _)| id.as_str()).collect()))
        .collect();
    assert_eq!(offered, vec![("codex", vec!["model-zulu", "model-alpha"])]);
    drop(choices);

    // Not connected, not in the list, or a model before any provider: refused, nothing saved.
    for refused in ["review_provider:openrouter", "review_provider:custom", "review_provider:nope", "review_pick:model-alpha"] {
        app.apply_action(UiAction::Clicked { id: refused.into() }, &ctx);
        assert!(app.view.settings_notice_error, "{refused}");
        assert!(app.review_model_settings.manual.is_none(), "{refused}");
    }

    // The menus open on this page and close on a pick.
    app.apply_action(UiAction::Clicked { id: "settings_choice_toggle:review_provider".into() }, &ctx);
    assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some("review_provider"));
    app.apply_action(UiAction::Clicked { id: "review_provider:codex".into() }, &ctx);
    assert_eq!(app.view.settings_choice_menu_open, None);
    assert_eq!((app.view.review_model.manual_provider.as_str(), app.view.review_model.manual_model.as_str()), ("codex", ""));
    assert_eq!(app.view.review_model.manual, "", "a provider alone is not shown as a saved model");

    // A provider without a model is not a usable manual choice: the review does not start.
    app.apply_action(UiAction::Clicked { id: "review_model:manual".into() }, &ctx);
    app.view.show_settings = false;
    app.start_isolated_review();
    assert!(app.active_run.is_none());
    assert!(app.view.composer_error.as_deref().is_some_and(|error| error.contains("modelo de revisión")));

    // Starting a conversation reloaded the connections from disk, where there
    // is no account: the picker now offers nothing and refuses a pick. It
    // follows what is connected at that moment, not what was when it opened.
    app.view.show_settings = true;
    assert!(crate::settings_ui::connected_model_choices(&app.view).is_empty());
    app.apply_action(UiAction::Clicked { id: "review_pick:model-alpha".into() }, &ctx);
    assert!(app.view.settings_notice_error);
    assert_eq!(app.view.review_model.manual_model, "");
    connect_codex(&mut app);

    // A retired model, or one the provider does not list, is refused.
    for unavailable in ["model-retired", "or-x", "not-a-model"] {
        app.apply_action(UiAction::Clicked { id: format!("review_pick:{unavailable}") }, &ctx);
        assert!(app.view.settings_notice_error, "{unavailable}");
        assert_eq!(app.view.review_model.manual_model, "");
    }
    app.apply_action(UiAction::Clicked { id: "settings_choice_toggle:review_pick".into() }, &ctx);
    assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some("review_pick"));
    app.apply_action(UiAction::Clicked { id: "review_pick:model-alpha".into() }, &ctx);
    assert!(!app.view.settings_notice_error);
    assert_eq!(app.view.settings_choice_menu_open, None);
    assert_eq!(app.view.review_model.manual, "codex · model-alpha");

    // What was saved is what a review would be handed, and it survives a restart.
    let selected = app.native_settings();
    let handed = app.review_model_settings.resolve(&selected, app.view.catalog.as_deref()).unwrap().unwrap();
    assert_eq!((handed.provider_id.as_str(), handed.model_id.as_str()), ("codex", "model-alpha"));
    let reopened = AgentsWindow::with_data_root(app.data_root.clone());
    assert_eq!(reopened.review_model_settings.manual, Some(handed));
    assert_eq!(reopened.review_model_settings.mode, crate::review_model::ReviewModelMode::Manual);

    // Changing provider drops the model, which belonged to the old one.
    connect_codex(&mut app);
    app.view.provider_connections.insert(
        "openrouter".into(),
        crate::provider_connections::ConnectionSummary { configured: true, ..Default::default() },
    );
    app.apply_action(UiAction::Clicked { id: "review_provider:openrouter".into() }, &ctx);
    assert_eq!((app.view.review_model.manual_provider.as_str(), app.view.review_model.manual_model.as_str()), ("openrouter", ""));
    // Picking the same provider again keeps the model that is already chosen.
    app.apply_action(UiAction::Clicked { id: "review_pick:or-x".into() }, &ctx);
    app.apply_action(UiAction::Clicked { id: "review_provider:openrouter".into() }, &ctx);
    assert_eq!(app.view.review_model.manual_model, "or-x");

    // The working model was never touched, and nothing was sent anywhere.
    assert_eq!((app.view.native_provider_id.as_str(), app.view.model.as_str()), ("custom", MODEL));
    assert!(fixture.requests.lock().unwrap().is_empty());
}

/// One real review against the user's own Codex account, to check that a
/// turn carrying its own model is accepted by the real provider. Never part
/// of a normal test run: it is ignored and also needs both variables.
///
/// It runs on the real data folder because the account's token is renewed in
/// place there; a copy would leave the real app signed out. It does not save
/// any setting, uses a throwaway project, and deletes its conversation.
#[cfg(unix)]
#[test]
#[ignore = "spends real model usage; set OPENIDE_LIVE_CODEX_REVIEW=1 and OPENIDE_LIVE_DATA_ROOT"]
fn live_codex_review_runs_on_its_own_model_and_leaves_the_selection_alone() {
    if std::env::var("OPENIDE_LIVE_CODEX_REVIEW").as_deref() != Ok("1") {
        return;
    }
    let data_root = PathBuf::from(std::env::var_os("OPENIDE_LIVE_DATA_ROOT").expect("OPENIDE_LIVE_DATA_ROOT"));
    // `manual` (needs OPENIDE_LIVE_REVIEW_MODEL), `same` or `automatic`.
    let mode = std::env::var("OPENIDE_LIVE_REVIEW_MODE").unwrap_or_else(|_| "manual".into());
    let selection_file = data_root.join("native-provider.json");
    let selection_before = std::fs::read(&selection_file).unwrap();

    let scratch = tempfile::tempdir().unwrap();
    let project = scratch.path().join("project");
    std::fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    std::fs::write(project.join("sum.rs"), "pub fn sum(a: i32, b: i32) -> i32 { a + b }\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-q", "-m", "base"]);
    std::fs::write(project.join("sum.rs"), "pub fn sum(a: i32, b: i32) -> i32 { a - b }\n").unwrap();

    let mut app = AgentsWindow::with_data_root(data_root.clone());
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    assert_eq!(app.view.native_provider_id, "codex", "this check is for a Codex selection");
    let selected_model = app.view.model.clone();
    app.view.workspace = project.to_string_lossy().into_owned();
    // In memory only: the user's saved review settings are not written.
    let review_model = match mode.as_str() {
        "manual" => {
            let review_model = std::env::var("OPENIDE_LIVE_REVIEW_MODEL").expect("OPENIDE_LIVE_REVIEW_MODEL");
            assert_ne!(selected_model, review_model, "pick a review model other than the selected one");
            app.review_model_settings.mode = crate::review_model::ReviewModelMode::Manual;
            app.review_model_settings.manual = Some(crate::native_provider::NativeProviderSettings {
                model_id: review_model.clone(),
                ..app.native_settings()
            });
            review_model
        }
        "same" => {
            app.review_model_settings.mode = crate::review_model::ReviewModelMode::Same;
            selected_model.clone()
        }
        "automatic" => {
            app.review_model_settings.mode = crate::review_model::ReviewModelMode::Automatic;
            // With the catalog the installed app really has. Whatever it
            // decides is what the review must run on, and Settings must say
            // the same thing beforehand.
            let catalog = app.view.catalog.clone().expect("the installed catalog did not load");
            assert!(catalog.providers.contains_key("codex"));
            let decided = crate::review_model::cheaper_model(&app.native_settings(), &catalog);
            app.refresh_review_model();
            eprintln!("LIVE automatic decided: {:?}; Settings shows: {:?}", decided.as_ref().map(|model| &model.model_id), app.view.review_model.automatic);
            assert_eq!(app.view.review_model.automatic.is_empty(), decided.is_none());
            decided.map_or_else(|| selected_model.clone(), |model| model.model_id)
        }
        other => panic!("unknown OPENIDE_LIVE_REVIEW_MODE {other}"),
    };
    eprintln!("LIVE mode {mode}: selected {selected_model}, review expected on {review_model}");

    app.start_isolated_review();
    let started = matches!(app.active_run, Some(ActiveRun::Native(_)));
    let review_session = app.view.selected_session.clone();
    let cleanup = |app: &mut AgentsWindow| {
        if let (Some(id), Some(store)) = (&review_session, app.store.as_ref()) {
            let _ = store.delete_native_tree(id);
        }
    };
    if !started {
        let why = format!("{:?} / {}", app.view.composer_error, app.view.status);
        cleanup(&mut app);
        panic!("the live review did not start: {why}");
    }
    let deadline = Instant::now() + Duration::from_secs(240);
    while app.active_run.is_some() {
        app.poll_run();
        if Instant::now() >= deadline {
            cleanup(&mut app);
            panic!("the live review did not finish in time: {}", app.view.status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let id = review_session.clone().unwrap();
    let stored = app.store.as_ref().unwrap().load(&id).unwrap().unwrap();
    let admitted: Vec<(String, TurnMode)> = stored
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::TurnAdmitted { model, mode, .. } => Some((model.clone(), *mode)),
            _ => None,
        })
        .collect();
    let failure = stored.events.iter().find_map(|event| match &event.kind {
        AgentEventKind::TurnFailed { error } => Some(error.clone()),
        _ => None,
    });
    let answer = stored.events.iter().rev().find_map(|event| match &event.kind {
        AgentEventKind::TurnCompleted { response, .. } => Some(response.clone()),
        _ => None,
    });
    let tools_used: Vec<String> = stored
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } => Some(tool_call.name.clone()),
            _ => None,
        })
        .collect();
    let selection_after = std::fs::read(&selection_file).unwrap();
    let selected_after = app.view.model.clone();
    cleanup(&mut app);

    eprintln!("LIVE admitted: {admitted:?}");
    eprintln!("LIVE tools used: {tools_used:?}");
    eprintln!("LIVE failure: {failure:?}");
    eprintln!("LIVE answer (first 600 chars): {}", answer.as_deref().unwrap_or("").chars().take(600).collect::<String>());
    assert_eq!(admitted, vec![(review_model.clone(), TurnMode::Plan)]);
    assert!(failure.is_none(), "the provider refused the turn: {failure:?}");
    assert!(answer.is_some_and(|answer| !answer.trim().is_empty()), "no answer came back");
    assert!(tools_used.iter().all(|name| !crate::turn_check::tool_has_effects(name)), "{tools_used:?}");
    assert_eq!(selected_after, selected_model);
    assert_eq!(selection_after, selection_before, "the saved selection changed");
    assert_eq!(std::fs::read_to_string(project.join("sum.rs")).unwrap(), "pub fn sum(a: i32, b: i32) -> i32 { a - b }\n");
}

/// Resuming an interrupted turn on the user's real Codex account while its
/// access token is expired: the case the old key check refused. Two phases,
/// because the first one's request renews the token and the second must wait
/// for it to expire by itself. Ignored, and needs explicit variables.
///
/// `OPENIDE_LIVE_RESUME_PHASE=prepare` runs one short read-only turn in
/// `OPENIDE_LIVE_PROJECT` and leaves it interrupted; `resume` resumes it and
/// deletes the conversation.
#[cfg(unix)]
#[test]
#[ignore = "spends real model usage; set OPENIDE_LIVE_CODEX_RESUME=1 and the phase variables"]
fn live_codex_resume_with_an_expired_token_is_not_refused_for_a_missing_key() {
    if std::env::var("OPENIDE_LIVE_CODEX_RESUME").as_deref() != Ok("1") {
        return;
    }
    let data_root = PathBuf::from(std::env::var_os("OPENIDE_LIVE_DATA_ROOT").expect("OPENIDE_LIVE_DATA_ROOT"));
    let project = PathBuf::from(std::env::var_os("OPENIDE_LIVE_PROJECT").expect("OPENIDE_LIVE_PROJECT"));
    let phase = std::env::var("OPENIDE_LIVE_RESUME_PHASE").expect("OPENIDE_LIVE_RESUME_PHASE");
    let marker = project.join(".openide-live-session");
    let selection_file = data_root.join("native-provider.json");
    let selection_before = std::fs::read(&selection_file).unwrap();

    let mut app = AgentsWindow::with_data_root(data_root.clone());
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    assert_eq!(app.view.native_provider_id, "codex", "this check is for a Codex selection");
    app.view.workspace = project.to_string_lossy().into_owned();
    let journal = |id: &str| data_root.join("sessions").join(format!("{id}.jsonl"));

    if phase == "prepare" {
        app.new_session();
        app.view.turn_mode = TurnMode::Plan;
        app.view.prompt = "Reply with the single word OK and nothing else. Do not use any tool.".into();
        app.start_run();
        assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{:?} / {}", app.view.composer_error, app.view.status);
        let id = app.view.selected_session.clone().unwrap();
        std::fs::write(&marker, &id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        while app.active_run.is_some() {
            app.poll_run();
            assert!(Instant::now() < deadline, "the prepare turn did not finish: {}", app.view.status);
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(app);
        // What a process killed while waiting for the model leaves behind.
        let text = std::fs::read_to_string(journal(&id)).unwrap();
        let mut kept = String::new();
        for line in text.lines() {
            kept.push_str(line);
            kept.push('\n');
            if serde_json::from_str::<Value>(line).unwrap()["kind"]["type"] == "model_request_started" {
                break;
            }
        }
        assert!(kept.lines().count() < text.lines().count(), "the turn never reached the model");
        std::fs::write(journal(&id), kept).unwrap();
        eprintln!("LIVE prepared interrupted session {id}");
        assert_eq!(std::fs::read(&selection_file).unwrap(), selection_before);
        return;
    }

    assert_eq!(phase, "resume");
    let id = std::fs::read_to_string(&marker).unwrap();
    // The precondition that makes this run mean something: right now the
    // stored key reads as absent because the token has expired.
    let connection = app.native_settings().resolve_at(&data_root).unwrap();
    let key_visible = connection.api_key.is_some();
    eprintln!("LIVE resume: stored key visible before resuming = {key_visible}");
    assert!(!key_visible, "the token has not expired yet; this run would not exercise the case");

    app.open_session(&id);
    app.start_resume();
    let status = app.view.status.clone();
    eprintln!("LIVE resume: status after start = {status:?}; started = {}", app.active_resume.is_some());
    let cleanup = |app: &mut AgentsWindow| {
        if let Some(store) = app.store.as_ref() {
            let _ = store.delete_native_tree(&id);
        }
        let _ = std::fs::remove_file(&marker);
    };
    if status.contains("Falta la clave") || app.active_resume.is_none() {
        cleanup(&mut app);
        panic!("the resume was refused: {status}");
    }
    let deadline = Instant::now() + Duration::from_secs(240);
    while app.active_resume.is_some() {
        app.poll_resume();
        if Instant::now() >= deadline {
            let status = app.view.status.clone();
            cleanup(&mut app);
            panic!("the resume did not finish: {status}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stored = app.store.as_ref().unwrap().load(&id).unwrap().unwrap();
    let completed = stored.events.iter().rev().find_map(|event| match &event.kind {
        AgentEventKind::TurnCompleted { response, .. } => Some(response.clone()),
        _ => None,
    });
    let failed = stored.events.iter().find_map(|event| match &event.kind {
        AgentEventKind::TurnFailed { error } => Some(error.clone()),
        _ => None,
    });
    let key_visible_after = app.native_settings().resolve_at(&data_root).unwrap().api_key.is_some();
    let final_status = app.view.status.clone();
    cleanup(&mut app);
    eprintln!("LIVE resume: completed = {completed:?}; failed = {failed:?}; status = {final_status:?}; key visible after = {key_visible_after}");
    assert!(failed.is_none(), "{failed:?}");
    assert!(completed.is_some_and(|answer| !answer.trim().is_empty()), "the resumed turn has no answer: {final_status}");
    assert!(key_visible_after, "the token was not renewed by the resumed request");
    assert_eq!(std::fs::read(&selection_file).unwrap(), selection_before);
}

#[cfg(unix)]
#[test]
fn notifications_e2e_a_turn_that_ends_unwatched_is_recorded_and_opens_its_task() {
    use crate::notifications::{Event, Severity, Target};
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let (mut app, session_id) = app_with_hooks(root.path(), &fixture, json!({}));
    let ctx = egui::Context::default();

    // The task on screen in a focused window needs no notification.
    app.window_focused = true;
    app.notify_session(Event::TurnCompleted, "Turno completado", &session_id);
    assert!(app.view.notifications.items().is_empty());

    // The same turn ending while the window is in the background is recorded.
    app.view.prompt = PROMPT.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(&mut app, |app| assistant_rows(app) == vec![(FIRST_VISIBLE, true)]);
    app.window_focused = false;
    fixture.next_delta();
    fixture.terminal();
    wait_app(&mut app, |app| app.active_run.is_none());
    let recorded = app.view.notifications.items().to_vec();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].title, "Turno completado");
    assert_eq!(recorded[0].severity, Severity::Info);
    assert_eq!(recorded[0].target, Some(Target::Session(session_id.clone())));
    assert!(!recorded[0].detail.is_empty());
    assert_eq!(app.view.notifications.unread(), 1);

    // A task that is not on screen is recorded even with the window focused.
    app.window_focused = true;
    app.notify_session(Event::TurnFailed, "Turno fallido", "otra-tarea");
    assert_eq!(app.view.notifications.items()[0].detail, "Tarea");
    assert_eq!(app.view.notifications.unread(), 2);

    // Opening the list and choosing the entry goes to its task and removes it.
    app.apply_action(UiAction::Clicked { id: "notifications_toggle".into() }, &ctx);
    assert!(app.view.notifications.open);
    app.apply_action(
        UiAction::Clicked { id: format!("notification_open:{}", recorded[0].id) },
        &ctx,
    );
    assert!(!app.view.notifications.open);
    assert_eq!(app.view.selected_session.as_deref(), Some(session_id.as_str()));
    let left = app.view.notifications.items().to_vec();
    assert_eq!(left.len(), 1);
    assert_eq!((left[0].title.as_str(), left[0].read), ("Turno fallido", true));

    app.apply_action(
        UiAction::Clicked { id: format!("notification_remove:{}", left[0].id) },
        &ctx,
    );
    assert!(app.view.notifications.items().is_empty());
    app.notify_session(Event::TurnCompleted, "Turno completado", "otra-tarea");
    app.apply_action(UiAction::Clicked { id: "notifications_clear".into() }, &ctx);
    assert!(app.view.notifications.items().is_empty());

    // A kind that is switched off is not recorded, and the choice is saved.
    app.apply_action(
        UiAction::Clicked { id: "notification_setting:turn_completed".into() },
        &ctx,
    );
    app.notify_session(Event::TurnCompleted, "Turno completado", "otra-tarea");
    assert!(app.view.notifications.items().is_empty());
    app.notify_session(Event::TurnFailed, "Turno fallido", "otra-tarea");
    assert_eq!(app.view.notifications.items().len(), 1);
    let failed = app.view.notifications.toasts(Instant::now())[0].id;
    app.apply_action(UiAction::Clicked { id: format!("notification_toast_dismiss:{failed}") }, &ctx);
    assert!(app.view.notifications.toasts(Instant::now()).is_empty());
    app.apply_action(
        UiAction::Clicked { id: "notification_setting:do_not_disturb".into() },
        &ctx,
    );
    app.notify_session(Event::Approval, "Permiso pendiente: run_command", "otra-tarea");
    assert_eq!(app.view.notifications.items().len(), 2);
    assert!(app.view.notifications.toasts(Instant::now()).is_empty());
    let saved = crate::notifications::NotificationSettings::load(&app.data_root);
    assert!(!saved.turn_completed && saved.do_not_disturb && saved.turn_failed);
    let reopened = AgentsWindow::with_data_root(app.data_root.clone());
    assert_eq!(reopened.view.notifications.settings, saved);
}

#[cfg(unix)]
#[path = "model_switch_app_tests.rs"]
mod model_switch_app_tests;

#[cfg(unix)]
#[path = "background_resume_app_tests.rs"]
mod background_resume_app_tests;
#[path = "remote_app_tests.rs"]
mod remote_app_tests;
