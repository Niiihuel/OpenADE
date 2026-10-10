//! Host acceptance for fresh, untrusted instruction files. Requests go through
//! AgentsWindow::start_run, the normal native worker, MCP and an actual TCP/SSE
//! provider; the fixture never implements the instruction-validation decision.
//! Its legacy MCP notification GET returns the supported optional 405 response.
//! This suite covers initialization, paginated tools, request and shutdown, not
//! notification delivery or resumable MCP runtime identity.

use super::*;
use crate::mcp_inspection::tests::HttpFixture;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MODEL: &str = "initial-instructions-exact-model";
const DRAFT: &str = "  Continue the project ñ 🦀\nKeep this draft intact.  ";
const PROJECT: &str = "Trusted project instructions ñ\nRetain exact workspace boundaries.";
const RESPONSE: &str = "Native initial instructions accepted";

struct ProviderFixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProviderFixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let observations = requests.clone();
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(3));
                        continue;
                    }
                    Err(error) => panic!("provider fixture accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                assert_eq!(request["model"], MODEL);
                assert_eq!(request["stream"], true);
                observations.lock().unwrap().push(request);
                let delta = json!({"id":"initial-acceptance", "choices":[{"index":0,"delta":{"content":RESPONSE},"finish_reason":null}]});
                let terminal = json!({"id":"initial-acceptance", "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                let body = format!("data: {delta}\n\ndata: {terminal}\n\ndata: [DONE]\n\n");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                stream.flush().unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ProviderFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let (header_end, length) = loop {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "provider request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 1024 * 1024, "provider fixture request limit");
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
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "provider request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}

fn prepare_app(
    data: &Path,
    workspace: &Path,
    mcp: &HttpFixture,
    provider: &ProviderFixture,
) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(data.to_path_buf());
    app.change_workspace(workspace.display().to_string());
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.composer_model = MODEL.into();
    app.view.composer_effort = "default".into();
    app.view.endpoint = provider.endpoint.clone();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    McpSettings {
        servers: vec![mcp.server.clone()],
    }
    .save(data, workspace)
    .unwrap();
    app.refresh_mcp_settings();
    app.view.turn_mode = TurnMode::Build;
    app.view.prompt = DRAFT.into();
    app.save_draft();
    assert!(app.store.as_ref().unwrap().list().unwrap().is_empty());
    assert!(app.view.selected_session.is_none());
    app
}

fn assert_rejected_before_admission(
    app: &mut AgentsWindow,
    workspace: &Path,
    mcp: &HttpFixture,
    provider: &ProviderFixture,
    expected_error: &str,
) {
    app.start_run();
    assert!(
        app.view
            .composer_error
            .as_deref()
            .is_some_and(|error| error.to_lowercase().contains(expected_error)),
        "expected instruction rejection, got {:?} / {}",
        app.view.composer_error,
        app.view.status
    );
    assert_eq!(app.view.prompt, DRAFT);
    assert!(!app.view.running);
    assert!(app.view.selected_session.is_none());
    assert!(app.active_run.is_none());
    assert!(app.active_resume.is_none());
    assert!(app.active_compact.is_none());
    assert!(app.active_mcp_inspection.is_none());
    assert!(app.pending_prompt.is_none());
    assert_eq!(app.last_sequence, 0);
    let store = app.store.as_ref().unwrap();
    assert!(
        store.list().unwrap().is_empty(),
        "invalid input created a session"
    );
    assert_eq!(
        store.load_draft(workspace, None).unwrap().as_deref(),
        Some(DRAFT)
    );
    // Reopen the actual store; these checks do not depend on the App's caches.
    let reopened = SessionStore::open(store.storage_root()).unwrap();
    assert!(reopened.list().unwrap().is_empty());
    assert_eq!(
        reopened.load_draft(workspace, None).unwrap().as_deref(),
        Some(DRAFT)
    );
    assert!(
        mcp.methods().is_empty(),
        "invalid instructions contacted MCP"
    );
    assert!(
        provider.requests().is_empty(),
        "invalid instructions contacted the provider"
    );
}

fn assert_repair_reaches_real_transports(
    app: &mut AgentsWindow,
    mcp: &HttpFixture,
    provider: &ProviderFixture,
    expected_skill: Option<&str>,
) {
    assert_eq!(app.view.prompt, DRAFT);
    app.start_run();
    assert!(
        matches!(app.active_run, Some(ActiveRun::Native(_))),
        "repaired configuration did not start: {}",
        app.view.status
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.active_run.is_some() {
        app.poll_run();
        assert!(
            Instant::now() < deadline,
            "native acceptance timed out: {}",
            app.view.status
        );
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(!app.view.running);
    assert!(
        app.view.composer_error.is_none(),
        "{:?}",
        app.view.composer_error
    );
    let methods = mcp.methods();
    assert!(
        methods.iter().any(|method| method == "initialize"),
        "{methods:?}"
    );
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "tools/list")
            .count(),
        2,
        "MCP pagination was not exercised: {methods:?}"
    );
    assert!(
        mcp.deleted.load(Ordering::Acquire),
        "prepared MCP session did not shut down"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 1, "expected one real native request");
    let request = &requests[0];
    assert_eq!(request["model"], MODEL);
    let system = request["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains(PROJECT),
        "repaired AGENTS.md not used by native request"
    );
    if let Some(skill) = expected_skill {
        assert!(
            system.contains(skill),
            "original selected skill was not revalidated"
        );
    }
    assert_eq!(
        request["messages"].as_array().unwrap().last().unwrap()["content"],
        DRAFT.trim()
    );
    assert!(
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"]
                .as_str()
                .is_some_and(|name| name.ends_with("inspect_echo"))),
        "actual MCP catalog did not reach the provider"
    );
    let store = app.store.as_ref().unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
    let id = app.view.selected_session.as_deref().unwrap();
    let record = SessionStore::open(store.storage_root())
        .unwrap()
        .load(id)
        .unwrap()
        .unwrap();
    assert_eq!(
        record
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
            .count(),
        1
    );
    assert!(record.events.iter().any(|event| matches!(&event.kind, AgentEventKind::SystemInstructionsCommitted { content, .. } if content.contains(PROJECT))));
    assert!(record
        .events
        .iter()
        .any(|event| matches!(event.kind, AgentEventKind::ToolCatalogCommitted { .. })));
    assert!(record.events.iter().any(|event| matches!(&event.kind, AgentEventKind::TurnCompleted { response, .. } if response == RESPONSE)), "repaired turn did not complete: {:?}", record.events);
    assert_eq!(
        store
            .load_draft(Path::new(&app.view.workspace), None)
            .unwrap(),
        None
    );
}

#[test]
fn invalid_utf8_and_oversized_agents_preserve_draft_before_admission_then_repair_reaches_http_mcp_and_provider(
) {
    for bytes in [
        vec![0xff, 0xfe, 0x00],
        vec![b'x'; openide_harness::ProjectInstructionLimits::default().max_file_bytes + 1],
    ] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let data = root.path().join("data");
        let agents = workspace.join("AGENTS.md");
        std::fs::write(&agents, PROJECT).unwrap();
        let mcp = HttpFixture::with_unsupported_notifications(true);
        let provider = ProviderFixture::new();
        let mut app = prepare_app(&data, &workspace, &mcp, &provider);
        std::fs::write(&agents, bytes).unwrap();
        assert_rejected_before_admission(&mut app, &workspace, &mcp, &provider, "agents.md");
        std::fs::write(&agents, PROJECT).unwrap();
        assert_repair_reaches_real_transports(&mut app, &mcp, &provider, None);
    }
}

#[cfg(unix)]
#[test]
fn linked_agents_is_rejected_before_admission_and_repaired_file_uses_same_native_transports() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let data = root.path().join("data");
    let agents = workspace.join("AGENTS.md");
    std::fs::write(&agents, PROJECT).unwrap();
    let external = root.path().join("outside-agents.md");
    std::fs::write(&external, "Untrusted outside content must never be loaded").unwrap();
    let mcp = HttpFixture::with_unsupported_notifications(true);
    let provider = ProviderFixture::new();
    let mut app = prepare_app(&data, &workspace, &mcp, &provider);
    std::fs::remove_file(&agents).unwrap();
    std::os::unix::fs::symlink(&external, &agents).unwrap();
    assert_rejected_before_admission(&mut app, &workspace, &mcp, &provider, "agents.md");
    assert_eq!(
        std::fs::read_to_string(&external).unwrap(),
        "Untrusted outside content must never be loaded"
    );
    std::fs::remove_file(&agents).unwrap();
    std::fs::write(&agents, PROJECT).unwrap();
    assert_repair_reaches_real_transports(&mut app, &mcp, &provider, None);
}

#[test]
fn changed_selected_skill_preserves_selection_and_draft_then_exact_restoration_reaches_real_native_request(
) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let data = root.path().join("data");
    std::fs::write(workspace.join("AGENTS.md"), PROJECT).unwrap();
    let skill_dir = workspace.join(".agents/skills/initial-test");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_document = skill_dir.join("SKILL.md");
    let original = "---\nname: initial-test\ndescription: Initial native instructions acceptance\n---\nRequire exact selected skill content ñ 🦀.\n";
    std::fs::write(&skill_document, original).unwrap();
    let settings = NativeSkillsSettings::load(&data, &workspace).unwrap();
    let selected = settings.discover(&data).unwrap().remove(0).selection();
    settings
        .with_selections(vec![selected.clone()])
        .save(&data)
        .unwrap();
    let saved_settings = std::fs::read(data.join("native-local-skills.json")).unwrap();
    let mcp = HttpFixture::with_unsupported_notifications(true);
    let provider = ProviderFixture::new();
    let mut app = prepare_app(&data, &workspace, &mcp, &provider);
    std::fs::write(
        &skill_document,
        format!("{original}Changed content requires explicit selection.\n"),
    )
    .unwrap();
    assert_rejected_before_admission(&mut app, &workspace, &mcp, &provider, "skill");
    assert_eq!(
        std::fs::read(data.join("native-local-skills.json")).unwrap(),
        saved_settings
    );
    assert_eq!(
        NativeSkillsSettings::load(&data, &workspace)
            .unwrap()
            .selections,
        [selected]
    );
    std::fs::write(&skill_document, original).unwrap();
    assert_repair_reaches_real_transports(
        &mut app,
        &mcp,
        &provider,
        Some("Require exact selected skill content ñ 🦀."),
    );
    assert_eq!(
        std::fs::read(data.join("native-local-skills.json")).unwrap(),
        saved_settings
    );
}
