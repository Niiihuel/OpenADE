//! Observable initial validation before real MCP HTTP, provider HTTP, hooks
//! and approved stdin, including authority failures and reopened receipts.
#![cfg(unix)]

use async_trait::async_trait;
use openide_core::{
    AgentEvent, AgentEventKind, AgentRunRequest, NativeCommandOperationAction,
    NativeCommandOperationRequest, NativeCommandOperationState, NativeCommandOperationStore,
    PlanSnapshot, SessionStore, TurnMode, MAX_SYSTEM_INSTRUCTIONS_BYTES,
};
use openide_harness::{
    AgentError, AgentRunHandle, AgentRunner, CommandHookConfig, CommandHooks, CommandIsolation,
    HookPhase, LocalTools, McpCompositeTools, McpHttpClient, McpHttpConfig, McpServerBinding,
    ModelRequestCapabilities, NativeManualCommandActor, NativeManualCommandPolicy,
    OpenAiCompatibleBackend, PermissionPolicy, PersistentCommandManager, PreparedToolSession,
    ToolExecutor, ToolPreparation,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "exact-initial-preflight-model";
const PROMPT: &str = "Check the initially admitted request";

struct HttpProbe {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HttpProbe {
    fn new(mcp: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!(
            "http://{}/{}",
            listener.local_addr().unwrap(),
            if mcp { "mcp" } else { "v1" }
        );
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("HTTP probe accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                assert!(length <= 2 * 1024 * 1024);
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                observed.lock().unwrap().push(request.clone());
                let (status, content_type, body) = if mcp && request["method"] == "server/discover"
                {
                    ("200 OK", "application/json", serde_json::to_vec(&json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"This fixture exposes the legacy initialize handshake"}})).unwrap())
                } else if mcp {
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => Some(
                            json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"preflight-http","version":"1"}}),
                        ),
                        "notifications/initialized" => None,
                        "tools/list" => Some(json!({"tools":[]})),
                        other => panic!("Unexpected MCP effect {other}"),
                    };
                    match result {
                        Some(result) => (
                            "200 OK",
                            "application/json",
                            serde_json::to_vec(
                                &json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                            )
                            .unwrap(),
                        ),
                        None => ("202 Accepted", "application/json", Vec::new()),
                    }
                } else {
                    ("200 OK", "text/event-stream", b"data: {\"choices\":[{\"delta\":{\"content\":\"validated response\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec())
                };
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
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

impl Drop for HttpProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

struct HttpPreparation {
    endpoint: String,
    preparations: Arc<AtomicUsize>,
}
struct HttpSession {
    client: Arc<McpHttpClient>,
    tools: Arc<dyn ToolExecutor>,
}

#[async_trait]
impl ToolPreparation for HttpPreparation {
    async fn prepare(
        &self,
        _: &Path,
        local: Arc<dyn ToolExecutor>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        let mut config = McpHttpConfig::new(&self.endpoint);
        config.request_timeout = Duration::from_secs(3);
        let client = Arc::new(
            McpHttpClient::connect(config, cancel)
                .await
                .map_err(|error| AgentError::Runtime(error.to_string()))?,
        );
        let binding = McpServerBinding::discover("preflight", client.clone(), cancel)
            .await
            .map_err(|error| AgentError::Runtime(error.to_string()))?;
        let tools = Arc::new(
            McpCompositeTools::new(local, vec![binding])
                .map_err(|error| AgentError::Runtime(error.to_string()))?,
        );
        Ok(Box::new(HttpSession { client, tools }))
    }
}

#[async_trait]
impl PreparedToolSession for HttpSession {
    fn tools(&self) -> Arc<dyn ToolExecutor> {
        self.tools.clone()
    }
    fn ask_tool_names(&self) -> &[String] {
        &[]
    }
    async fn shutdown(&self) -> Result<(), AgentError> {
        self.client
            .shutdown()
            .await
            .map_err(|error| AgentError::Runtime(error.to_string()))
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    sessions_root: PathBuf,
    sessions: Arc<SessionStore>,
    owner: String,
    manager: Arc<PersistentCommandManager>,
    actor: Arc<NativeManualCommandActor>,
    command: String,
    hooks: Arc<CommandHooks>,
    preparations: Arc<AtomicUsize>,
    mcp: HttpProbe,
    provider: HttpProbe,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sessions_root = root.path().join("sessions");
        let sessions = Arc::new(SessionStore::open(&sessions_root).unwrap());
        let owner = sessions.create(&workspace, "native").unwrap().id;
        let manager = Arc::new(
            PersistentCommandManager::new(
                workspace.clone(),
                CommandIsolation::HostUnrestricted,
                Default::default(),
            )
            .unwrap()
            .with_native_session_owner(&owner)
            .unwrap(),
        );
        let command = manager
            .start(
                "/bin/sh",
                &["-c".into(), "exec cat > stdin-effect".into()],
                &CancellationToken::new(),
            )
            .unwrap()
            .id;
        wait_file(&workspace.join("stdin-effect")).await;
        let hooks = Arc::new(CommandHooks::new(vec![CommandHookConfig {
            id: "initial-pre-hook".into(), phase: HookPhase::PreToolUse, tool_name: "write_stdin".into(), executable: "/bin/sh".into(),
            args: vec!["-c".into(), "cat > hook-invocation; printf observed > hook-effect; printf '%s' '{\"decision\":\"allow\"}'".into()],
            cwd: workspace.clone(), timeout_ms: 3_000, max_output_bytes: 4_096,
        }]).unwrap());
        let policy_hooks = hooks.clone();
        let actor = Arc::new(
            NativeManualCommandActor::new(
                sessions.clone(),
                owner.clone(),
                Some(manager.clone()),
                Arc::new(move || {
                    Ok(NativeManualCommandPolicy {
                        permissions: PermissionPolicy::new(),
                        run_command_enabled: true,
                        isolation: CommandIsolation::HostUnrestricted,
                        hooks: policy_hooks.clone(),
                    })
                }),
            )
            .unwrap(),
        );
        Self {
            _root: root,
            workspace,
            sessions_root,
            sessions,
            owner,
            manager,
            actor,
            command,
            hooks,
            preparations: Arc::new(AtomicUsize::new(0)),
            mcp: HttpProbe::new(true),
            provider: HttpProbe::new(false),
        }
    }
    fn request(&self, turn: &str) -> AgentRunRequest {
        let admission = self
            .sessions
            .admit_native_turn(&self.owner, turn, PROMPT, MODEL, TurnMode::Build)
            .unwrap();
        AgentRunRequest {
            session_id: Some(self.owner.clone()),
            sequence_start: admission.sequence,
            workspace: self.workspace.clone(),
            history: vec![],
            prompt: PROMPT.into(),
            mode: TurnMode::Build,
        }
    }
    fn queue(&self, input: &str) -> NativeCommandOperationRequest {
        let request = self
            .actor
            .prepare_request(
                &self.command,
                NativeCommandOperationAction::Stdin {
                    input: input.into(),
                    close_after: false,
                },
            )
            .unwrap();
        self.actor.enqueue_confirmed(request.clone()).unwrap();
        request
    }
    fn runner(&self) -> AgentRunner {
        let model = OpenAiCompatibleBackend::new(&self.provider.endpoint, None, MODEL)
            .with_timeout(Duration::from_secs(3))
            .with_capabilities(ModelRequestCapabilities {
                tool_calls: true,
                ..Default::default()
            })
            .unwrap();
        AgentRunner::new(Arc::new(model), Arc::new(LocalTools::new()))
            .with_event_journal(self.sessions.clone())
            .with_native_runtime_identity("exact-local", &self.provider.endpoint)
            .with_manual_command_actor(self.actor.clone())
            .with_command_hooks(self.hooks.clone())
            .with_tool_preparation(Arc::new(HttpPreparation {
                endpoint: self.mcp.endpoint.clone(),
                preparations: self.preparations.clone(),
            }))
    }
    fn no_effects(&self) {
        assert_eq!(self.preparations.load(Ordering::SeqCst), 0);
        assert!(self.mcp.requests().is_empty());
        assert!(self.provider.requests().is_empty());
        assert!(!self.workspace.join("hook-effect").exists());
        assert!(!self.workspace.join("hook-invocation").exists());
        assert!(std::fs::read(self.workspace.join("stdin-effect"))
            .unwrap()
            .is_empty());
        assert!(self
            .manager
            .control_snapshot(&self.command)
            .unwrap()
            .stdin_open());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.stop_all();
    }
}

async fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "Real process did not open {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn finish(handle: &AgentRunHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        match handle.events.recv_timeout(Duration::from_secs(8)) {
            Ok(event) => events.push(event),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return events,
            Err(error) => panic!("Worker failed to release its lease/channel: {error}"),
        }
    }
}

#[tokio::test]
async fn initial_invalid_instructions_or_plan_reject_queue_before_real_mcp_hooks_stdin_and_provider(
) {
    for invalid_plan in [false, true] {
        let fixture = Fixture::new().await;
        let request = fixture.request("invalid-initial-turn");
        let queued = fixture.queue("must never be dispatched\r\n");
        let runner = if invalid_plan {
            fixture.runner().with_plan_snapshot(Some(PlanSnapshot {
                explanation: None,
                plan: Vec::new(),
            }))
        } else {
            fixture
                .runner()
                .with_system_instructions("ñ".repeat(MAX_SYSTEM_INSTRUCTIONS_BYTES / 2 + 1))
        };
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.owner)
            .unwrap();
        let handle = runner.start_with_lease(request, lease);
        let events = finish(&handle);
        assert!(matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnFailed { .. }
        ));
        assert!(handle.journal_error().is_none());
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::SystemInstructionsCommitted { .. }
                | AgentEventKind::ToolCatalogCommitted { .. }
                | AgentEventKind::HookStarted { .. }
                | AgentEventKind::ModelRequestStarted { .. }
        )));
        fixture.no_effects();
        let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
        let operation = NativeCommandOperationStore::new(reopened.clone())
            .snapshot(&fixture.owner, &queued.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(operation.request, queued);
        assert_eq!(operation.revision, 1);
        assert!(matches!(
            operation.state,
            NativeCommandOperationState::Rejected { .. }
        ));
        assert!(operation.effect_result.is_none() && operation.phase_diagnostics().is_empty());
        assert!(matches!(
            reopened
                .load(&fixture.owner)
                .unwrap()
                .unwrap()
                .events
                .last()
                .unwrap()
                .kind,
            AgentEventKind::TurnFailed { .. }
        ));
        drop(reopened.try_acquire_native_lease(&fixture.owner).unwrap());
        fixture.no_effects();
    }
}

#[tokio::test]
async fn initial_exact_instruction_limit_keeps_valid_effects_and_stale_retry_cannot_consume_later_queue(
) {
    let fixture = Fixture::new().await;
    let request = fixture.request("valid-initial-turn");
    let queued = fixture.queue("valid explicitly confirmed input\n");
    let runner = fixture
        .runner()
        .with_system_instructions("x".repeat(MAX_SYSTEM_INSTRUCTIONS_BYTES));
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let handle = runner.start_with_lease(request.clone(), lease);
    let events = finish(&handle);
    assert!(
        matches!(
            events.last().unwrap().kind,
            AgentEventKind::TurnCompleted { .. }
        ),
        "{events:?}"
    );
    assert!(handle.journal_error().is_none());
    assert_eq!(fixture.preparations.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .mcp
            .requests()
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "server/discover",
            "initialize",
            "notifications/initialized",
            "tools/list"
        ]
    );
    assert_eq!(fixture.provider.requests().len(), 1);
    assert!(std::fs::read(fixture.workspace.join("hook-effect")).unwrap() == b"observed");
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::fs::read(fixture.workspace.join("stdin-effect")).unwrap()
        != b"valid explicitly confirmed input\n"
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let store = NativeCommandOperationStore::new(fixture.sessions.clone());
    assert!(matches!(
        store
            .snapshot(&fixture.owner, &queued.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeCommandOperationState::Finished { .. }
    ));
    fixture.request("later-admitted-turn");
    let later = fixture.queue("a different turn must not consume this\n");
    let before = fixture
        .sessions
        .load(&fixture.owner)
        .unwrap()
        .unwrap()
        .events;
    let later_before = store
        .snapshot(&fixture.owner, &later.operation_id)
        .unwrap()
        .unwrap();
    let lease = fixture
        .sessions
        .try_acquire_native_lease(&fixture.owner)
        .unwrap();
    let stale = fixture.runner().start_with_lease(request, lease);
    assert!(finish(&stale).is_empty());
    assert!(stale.journal_error().is_some());
    assert_eq!(
        fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events,
        before
    );
    assert_eq!(
        store
            .snapshot(&fixture.owner, &later.operation_id)
            .unwrap()
            .unwrap(),
        later_before
    );
    assert_eq!(fixture.preparations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.requests().len(), 1);
    assert_eq!(
        std::fs::read(fixture.workspace.join("stdin-effect")).unwrap(),
        b"valid explicitly confirmed input\n"
    );
    assert!(matches!(
        later_before.state,
        NativeCommandOperationState::Requested
    ));
    drop(
        fixture
            .sessions
            .try_acquire_native_lease(&fixture.owner)
            .unwrap(),
    );
}

#[tokio::test]
async fn initial_unvalidated_foreign_journal_and_started_receipt_remain_unchanged_without_effects()
{
    for uncertain in [false, true] {
        let fixture = Fixture::new().await;
        let request = fixture.request("authority-preflight-turn");
        let foreign_path = fixture._root.path().join("foreign-sessions");
        std::fs::create_dir(&foreign_path).unwrap();
        // Copy the same native identity/admission before queueing: a sequence
        // fence alone could otherwise close the different, empty-ledger store.
        for entry in std::fs::read_dir(&fixture.sessions_root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), foreign_path.join(entry.file_name())).unwrap();
            }
        }
        let foreign = Arc::new(SessionStore::open(foreign_path).unwrap());
        let queued = fixture.queue("unknown or foreign authority never enters stdin\n");
        let lease = fixture
            .sessions
            .try_acquire_native_lease(&fixture.owner)
            .unwrap();
        let operations = NativeCommandOperationStore::new(fixture.sessions.clone());
        if uncertain {
            operations
                .preflight_capacity(&lease, &queued.operation_id, 512 * 1024)
                .unwrap();
            operations
                .begin_started(&lease, &queued.operation_id, 0)
                .unwrap();
        }
        let before = fixture
            .sessions
            .load(&fixture.owner)
            .unwrap()
            .unwrap()
            .events;
        let foreign_before = foreign.load(&fixture.owner).unwrap().unwrap().events;
        let receipt = operations
            .snapshot(&fixture.owner, &queued.operation_id)
            .unwrap()
            .unwrap();
        let runner = if uncertain {
            fixture.runner()
        } else {
            fixture.runner().with_event_journal(foreign.clone())
        };
        let handle = runner.start_with_lease(request, lease);
        assert!(finish(&handle).is_empty());
        assert!(handle.journal_error().is_some());
        fixture.no_effects();
        assert_eq!(
            fixture
                .sessions
                .load(&fixture.owner)
                .unwrap()
                .unwrap()
                .events,
            before
        );
        assert_eq!(
            foreign.load(&fixture.owner).unwrap().unwrap().events,
            foreign_before
        );
        assert_eq!(
            operations
                .snapshot(&fixture.owner, &queued.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
        let reopened = Arc::new(SessionStore::open(&fixture.sessions_root).unwrap());
        assert_eq!(
            NativeCommandOperationStore::new(reopened.clone())
                .snapshot(&fixture.owner, &queued.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
        assert!(if uncertain {
            receipt.state.requires_verification()
        } else {
            matches!(receipt.state, NativeCommandOperationState::Requested)
        });
        drop(reopened.try_acquire_native_lease(&fixture.owner).unwrap());
    }
}
