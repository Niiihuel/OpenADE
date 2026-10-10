//! Local HTTP/SSE acceptance through the desktop automation dispatcher and
//! explicit recovery UI. These are fixture providers, not account acceptance.
use super::*;
#[cfg(unix)]
use crate::native_hook_settings::NativeHookSettings;
use crate::{
    automations::{AutomationDraft, AutomationRunStatus, AutomationSchedule},
    mcp_inspection::tests::HttpFixture as McpFixture,
    native_automation::FrozenNativeAutomationAuthorityV1,
};
use openide_harness::LocalTool;
#[cfg(unix)]
use openide_harness::{CommandHookConfig, HookPhase};
use openide_ui::{HitKind, Point, Size, UiEvent, UiTree};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{atomic::AtomicBool, Mutex},
};

const MODEL: &str = "automation-app/exact-model";
const PROMPT: &str = "Read report.txt using read_file and summarize it.";
const PROJECT: &str = "Project-specific trusted instructions ñ\nKeep the original authority.";
const REPORT: &str = "EXACT_REPORT_FROM_REAL_READ_FILE ñ🦀";
const RESPONSE: &str = "AUTOMATION_READ_ONLY_COMPLETE";
const DEADLINE: Duration = Duration::from_secs(12);

struct Provider {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    release: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Provider {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let release = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let (observed, unblock, stop) = (requests.clone(), release.clone(), stopped.clone());
        let worker = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("automation provider accept: {error}"),
                };
                let request = read_request(&mut socket);
                let streamed = request["stream"] == true;
                let index = {
                    let mut requests = observed.lock().unwrap();
                    requests.push(request);
                    requests.len()
                };
                assert!(index <= 3, "automation launched an unexpected new request");
                if index == 1 {
                    let deadline = Instant::now() + DEADLINE;
                    while !unblock.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
                        assert!(
                            Instant::now() < deadline,
                            "automation HTTP barrier not released"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                }
                if !streamed {
                    assert_eq!(index, 3, "automation used a non-streamed model turn");
                    let summary=json!({"objective":PROMPT,"decisions":["Keep read-only authority"],"files":[{"path":"report.txt","state":"read"}],"current_state":"ORIGINAL_FROZEN_COMPACT_SUMMARY: report inspected","constraints":["No writes or commands"],"remaining_work":[]}).to_string();
                    let body=serde_json::to_string(&json!({"id":"automation-summary","choices":[{"index":0,"message":{"role":"assistant","content":summary},"finish_reason":"stop"}]})).unwrap();
                    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    socket.flush().unwrap();
                    continue;
                }
                let delta = if index == 1 {
                    json!({"id":"automation-read", "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"read-report","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"report.txt\"}"}}]},"finish_reason":null}]})
                } else {
                    json!({"id":"automation-final", "choices":[{"index":0,"delta":{"content":RESPONSE},"finish_reason":null}]})
                };
                let terminal = json!({"id":"automation-terminal", "choices":[{"index":0,"delta":{},"finish_reason":if index==1 { "tool_calls" } else { "stop" }}]});
                let body = format!("data: {delta}\n\ndata: {terminal}\n\ndata: [DONE]\n\n");
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                socket.flush().unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            release,
            stopped,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.release.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn read_request(socket: &mut TcpStream) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let (header_end, length) = loop {
        let mut chunk = [0u8; 4096];
        let count = socket.read(&mut chunk).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            assert_eq!(
                headers.lines().next(),
                Some("POST /v1/chat/completions HTTP/1.1")
            );
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap();
            assert!(length <= 1024 * 1024);
            break (end + 4, length);
        }
    };
    while bytes.len() < header_end + length {
        let mut chunk = [0u8; 4096];
        let count = socket.read(&mut chunk).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}

fn prepare(root: &Path, workspace: &Path, provider: &Provider) -> AgentsWindow {
    fs::create_dir_all(workspace).unwrap();
    fs::write(workspace.join("AGENTS.md"), PROJECT).unwrap();
    fs::write(workspace.join("report.txt"), REPORT).unwrap();
    let mut app = AgentsWindow::with_data_root(root.to_path_buf());
    app.change_workspace(workspace.display().to_string());
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.composer_model = MODEL.into();
    app.view.composer_effort = "default".into();
    app.view.endpoint = provider.endpoint.clone();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    native_provider::save_output_limit(
        root,
        "custom",
        MODEL,
        Some(native_provider::NativeOutputLimit {
            provider_id: "custom".into(),
            model_id: MODEL.into(),
            wire_api: NativeWireApi::ChatCompletions,
            field: "max_tokens".into(),
            tokens: 512,
        }),
    )
    .unwrap();
    let mut permissions = AgentToolSettings::default();
    permissions.set(LocalTool::WriteFile, true);
    permissions.set(LocalTool::RunCommand, true);
    assert!(app.commit_agent_tool_settings(permissions, "Host allows interactive effects"));
    app
}

fn claim(app: &mut AgentsWindow) -> (usize, AutomationDispatch) {
    let index = app.current_automation_store_index().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let native_provider = app.native_settings();
    app.automation_stores[index]
        .create(
            AutomationDraft {
                title: "Native read-only acceptance".into(),
                prompt: PROMPT.into(),
                backend: "native".into(),
                native_provider: Some(native_provider),
                schedule: AutomationSchedule::Once { at_ms: now + 1 },
            },
            now,
        )
        .unwrap();
    let dispatch = app.automation_stores[index]
        .claim_due(now + 1)
        .unwrap()
        .pop()
        .unwrap();
    assert!(!dispatch.manual);
    assert_eq!(dispatch.prompt, PROMPT);
    (index, dispatch)
}

fn wait_for_request(app: &mut AgentsWindow, provider: &Provider) {
    let deadline = Instant::now() + DEADLINE;
    while provider.requests().is_empty() {
        app.poll_automations();
        app.poll_run();
        assert!(
            Instant::now() < deadline,
            "automation did not reach HTTP: {} / {:?}",
            app.view.status,
            app.view.automations.error
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn finish(app: &mut AgentsWindow, provider: &Provider) {
    provider.release.store(true, Ordering::Release);
    let deadline = Instant::now() + DEADLINE;
    while app.active_run.is_some()
        || app.active_resume.is_some()
        || !app.background_automation_runs.is_empty()
    {
        app.poll_run();
        app.poll_resume();
        app.poll_automations();
        assert!(
            Instant::now() < deadline,
            "automation did not complete: {} / {:?}",
            app.view.status,
            app.view.automations.error
        );
        std::thread::sleep(Duration::from_millis(3));
    }
    assert_eq!(provider.requests().len(), 2);
}

fn assert_requests(provider: &Provider, instructions: &str) {
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["model"], MODEL);
        assert_eq!(request["stream"], true);
        assert_eq!(request["max_tokens"], 512);
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "system")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(
            system.contains(instructions),
            "frozen instructions not sent: {system}"
        );
        let names: Vec<_> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 4);
        for name in ["read_file", "list_files", "search_files", "update_plan"] {
            assert!(names.contains(&name));
        }
    }
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["tool_call_id"] == "read-report"
                && message["content"]
                    .as_str()
                    .is_some_and(|text| text.contains(REPORT))),
        "{:?}",
        requests[1]["messages"]
    );
}

fn assert_completed(store: &SessionStore, id: &str) -> StoredSession {
    let stored = store.load(id).unwrap().unwrap();
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        stored
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
            .count(),
        1
    );
    assert!(stored.events.iter().any(|event|matches!(&event.kind,AgentEventKind::ToolFinished { tool_call_id,output,is_error } if tool_call_id=="read-report"&&!is_error&&output.contains(REPORT))));
    assert!(!stored.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::PermissionRequested { .. }
            | AgentEventKind::HookStarted { .. }
            | AgentEventKind::TurnFailed { .. }
    )));
    stored
}

#[cfg(unix)]
fn install_hook_canary(root: &Path, workspace: &Path) -> PathBuf {
    let canary = workspace.join("hook-must-not-run");
    NativeHookSettings {
        hooks: vec![CommandHookConfig {
            id: "automation-current-read-hook".into(),
            phase: HookPhase::PreToolUse,
            tool_name: "read_file".into(),
            executable: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                "printf side-effect > \"$1\"".into(),
                "hook".into(),
                canary.display().to_string(),
            ],
            cwd: workspace.canonicalize().unwrap(),
            timeout_ms: 3_000,
            max_output_bytes: 1_024,
        }],
    }
    .save(root)
    .unwrap();
    canary
}

#[test]
fn app_dispatch_uses_native_readonly_authority_and_one_durable_admission() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("data");
    let workspace = directory.path().join("project");
    let provider = Provider::new();
    let mcp = McpFixture::with_unsupported_notifications(true);
    let mut app = prepare(&root, &workspace, &provider);
    McpSettings {
        servers: vec![mcp.server.clone()],
    }
    .save(&root, &workspace)
    .unwrap();
    #[cfg(unix)]
    let canary = install_hook_canary(&root, &workspace);
    let (index, dispatch) = claim(&mut app);
    let run_id = dispatch.run_id.clone();
    app.dispatch_automation(index, dispatch.clone());
    wait_for_request(&mut app, &provider);
    let session_id = app.automation_stores[index]
        .runs()
        .iter()
        .find(|run| run.run_id == run_id)
        .unwrap()
        .session_id
        .clone()
        .unwrap();
    let store = app.store.clone().unwrap();
    let stored = store.load(&session_id).unwrap().unwrap();
    let frozen = FrozenNativeAutomationAuthorityV1::from_session(&stored.session)
        .unwrap()
        .unwrap();
    assert!(frozen.instructions().contains(PROJECT));
    assert_eq!(frozen.prompt(), PROMPT);
    assert!(stored.session.native_automation.is_some());
    assert!(stored
        .events
        .iter()
        .any(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. })));
    assert!(!stored
        .events
        .iter()
        .any(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. })));
    assert!(
        store.try_acquire_native_lease(&session_id).is_err(),
        "native automation did not own its lease"
    );
    finish(&mut app, &provider);
    assert_requests(&provider, frozen.instructions());
    let before = assert_completed(&store, &session_id);
    assert!(
        mcp.methods().is_empty(),
        "automation initialized current MCP"
    );
    #[cfg(unix)]
    assert!(
        !canary.exists(),
        "automation executed current interactive hook"
    );
    assert!(app.native_command_sessions.manager(&session_id).is_none());
    let run = app.automation_stores[index]
        .runs()
        .iter()
        .find(|run| run.run_id == run_id)
        .unwrap();
    assert_eq!(run.status, AutomationRunStatus::Dispatched);
    // Retrying an already admitted occurrence must not admit or launch again.
    fs::write(
        root.join("provider-registry.json"),
        b"{broken registry after completion",
    )
    .unwrap();
    fs::write(
        root.join("native-output-limits.json"),
        b"{broken limits after completion",
    )
    .unwrap();
    fs::write(workspace.join("AGENTS.md"), [0xff, 0xfe]).unwrap();
    app.dispatch_automation(index, dispatch);
    assert_eq!(store.load(&session_id).unwrap().unwrap(), before);
    assert_eq!(provider.requests().len(), 2);
    assert!(app.background_automation_runs.is_empty());
    assert_eq!(
        app.automation_stores[index]
            .runs()
            .iter()
            .find(|run| run.run_id == run_id)
            .unwrap()
            .status,
        AutomationRunStatus::Dispatched
    );
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(root.clone());
    reopened.open_session(&session_id);
    assert_completed(reopened.store.as_ref().unwrap(), &session_id);
    assert!(!reopened.view.session_recovery.contains_key(&session_id));
}

#[test]
fn app_recovers_registered_metadata_before_admission_with_frozen_route_and_instructions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("data");
    let workspace = directory.path().join("project");
    let provider = Provider::new();
    let mcp = McpFixture::with_unsupported_notifications(true);
    let mut app = prepare(&root, &workspace, &provider);
    let (index, dispatch) = claim(&mut app);
    let instructions = native_workspace_instructions(&root, &workspace).unwrap();
    let frozen = FrozenNativeAutomationAuthorityV1::freeze(
        app.native_settings().resolve_at(&root).unwrap(),
        "custom",
        &root,
        &workspace,
        None,
        &[],
        None,
        None,
        instructions.clone(),
        dispatch.prompt.clone(),
        &dispatch.run_id,
    )
    .unwrap();
    let origin = frozen
        .to_origin(
            &dispatch.automation_id,
            &dispatch.run_id,
            dispatch.scheduled_at,
            dispatch.manual,
        )
        .unwrap();
    let store = app.store.clone().unwrap();
    let session = store.create_native_automation(&workspace, origin).unwrap();
    assert!(store.load(&session.id).unwrap().unwrap().events.is_empty());
    // This is the exact durable prefix of a cut after metadata publication and
    // before the first admission; no fake model or lifecycle events are added.
    assert!(app.automation_stores[index]
        .runs()
        .iter()
        .find(|run| run.run_id == dispatch.run_id)
        .unwrap()
        .session_id
        .is_none());
    drop(app);
    drop(store);
    NativeProviderSettings {
        provider_id: "custom".into(),
        model_id: "mutable-other-model".into(),
        custom_endpoint: "http://127.0.0.1:9/v1".into(),
    }
    .save(&root)
    .unwrap();
    fs::write(
        root.join("provider-registry.json"),
        b"{broken current registry",
    )
    .unwrap();
    fs::write(
        root.join("native-output-limits.json"),
        b"{broken current limits",
    )
    .unwrap();
    fs::write(workspace.join("AGENTS.md"), [0xff, 0xfe, 0]).unwrap();
    McpSettings {
        servers: vec![mcp.server.clone()],
    }
    .save(&root, &workspace)
    .unwrap();
    #[cfg(unix)]
    let canary = install_hook_canary(&root, &workspace);
    let mut reopened = AgentsWindow::with_data_root(root.clone());
    reopened.open_session(&session.id);
    assert_eq!(
        reopened.view.session_recovery.get(&session.id),
        Some(&SessionRecoveryView::StartAutomation)
    );
    // A recovered automation's Context must describe its frozen route, never
    // the currently selected route or newly invalid project instructions.
    reopened.refresh_native_context();
    assert_eq!(reopened.view.native_context.provider, "custom");
    assert_eq!(reopened.view.native_context.model, MODEL);
    assert_eq!(
        reopened.view.native_context.workspace.as_deref(),
        Some(workspace.canonicalize().unwrap().as_path())
    );
    assert!(
        reopened.view.native_context.error.is_none(),
        "{:?}",
        reopened.view.native_context.error
    );
    let ctx = egui::Context::default();
    let id = format!("session_recover:{}", session.id);
    let mut tree = UiTree::new(build_view(&reopened.view, 900.0));
    tree.set_reduced_motion(true);
    let frame = tree.layout(Size::new(900.0, 800.0));
    let region = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap();
    let point = Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(id.as_str()));
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert!(actions
        .iter()
        .any(|action| matches!(action,UiAction::Clicked { id:clicked } if clicked==&id)));
    for action in actions {
        reopened.apply_action(action, &ctx);
    }
    assert!(
        matches!(reopened.active_run, Some(ActiveRun::Native(_))),
        "{}",
        reopened.view.status
    );
    wait_for_request(&mut reopened, &provider);
    finish(&mut reopened, &provider);
    assert_requests(&provider, &instructions);
    let store = reopened.store.clone().unwrap();
    let before = assert_completed(&store, &session.id);
    assert_eq!(
        FrozenNativeAutomationAuthorityV1::from_session(&before.session).unwrap(),
        Some(frozen.clone())
    );
    assert!(mcp.methods().is_empty());
    #[cfg(unix)]
    assert!(!canary.exists());
    assert!(reopened
        .native_command_sessions
        .manager(&session.id)
        .is_none());
    assert!(store.try_acquire_native_lease(&session.id).is_ok());
    reopened.start_resume();
    assert!(reopened.active_resume.is_none());
    assert!(reopened.active_run.is_none());
    assert_eq!(store.load(&session.id).unwrap().unwrap(), before);
    assert_eq!(provider.requests().len(), 2);
    reopened.start_compact();
    assert!(
        reopened.active_compact.is_some(),
        "frozen compaction failed to start: {}",
        reopened.view.status
    );
    let deadline = Instant::now() + DEADLINE;
    while reopened.active_compact.is_some() {
        reopened.poll_compact();
        assert!(
            Instant::now() < deadline,
            "frozen compaction timed out: {}",
            reopened.view.status
        );
        std::thread::sleep(Duration::from_millis(3));
    }
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["model"], MODEL);
    assert_eq!(requests[2]["max_tokens"], 512);
    assert!(
        requests[2].get("tools").is_none(),
        "compaction exposed execution tools"
    );
    let compacted = store.load(&session.id).unwrap().unwrap();
    let reference = compacted
        .events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::CheckpointCommitted { reference } => Some(reference),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "compaction did not commit a checkpoint: {}",
                reopened.view.status
            )
        });
    let checkpoints = CheckpointFileStore::new(root.join("sessions")).unwrap();
    let checkpoint = checkpoints
        .load_after_commit(Some(reference))
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.compatibility, frozen.compatibility());
    assert!(checkpoint
        .summary
        .contains("ORIGINAL_FROZEN_COMPACT_SUMMARY"));
    assert!(mcp.methods().is_empty());
    #[cfg(unix)]
    assert!(!canary.exists());
}
