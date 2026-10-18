//! Real transport and process-cut acceptance for frozen read-only automations.

use super::*;
use crate::native_automation::FrozenNativeAutomationAuthorityV1;
use crate::native_provider::{
    DiscoveredModel, NativeWireApi, ProviderModelDiscovery, ProviderRegistry, RuntimeConnection,
};
use openide_core::{
    AgentEventKind, AgentRunRequest, NativeEventJournal, Session, StoreError, TurnMode,
};
use openide_harness::{AgentRunner, CommandHookConfig, HookPhase, LocalTools, PermissionMode};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread::JoinHandle,
    time::Duration,
};

const MODEL: &str = "automation-resume/exact-model-ñ";
const PROVIDER: &str = "automation-resume-fixture";
const INSTRUCTIONS: &str = "  Original authority ñ 🦀\r\nOnly inspect payload.txt.  \n";
const PROMPT: &str = "  Read payload.txt and report its content.\n  ";
const PAYLOAD: &str = "Original file ñ🙂\r\nsecond line 終わり\n";
const FINAL: &str = "Read-only automation completed ñ";
const CALL_ID: &str = "automation-read-call";
const FIXTURE_KEY: &str = "automation-local-fixture-key";
const CUT_EXIT: i32 = 86;
const CUT_ROOT: &str = "OPENIDE_AUTOMATION_RESUME_CUT_ROOT";
const CUT_SESSION: &str = "OPENIDE_AUTOMATION_RESUME_CUT_SESSION";
const CUT_POINT: &str = "OPENIDE_AUTOMATION_RESUME_CUT_POINT";

#[derive(Clone, Debug)]
struct ObservedRequest {
    body: Value,
    authorization: Option<String>,
}

struct ReadFileServer {
    endpoint: String,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

fn read_http_request(stream: &mut TcpStream) -> ObservedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let (body_start, length) = loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "request ended before HTTP headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(header.starts_with("POST /v1/chat/completions "));
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                })
                .unwrap();
            assert!(length <= 1024 * 1024);
            break (end + 4, length);
        }
    };
    while bytes.len() < body_start + length {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "request ended before HTTP body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    let header = std::str::from_utf8(&bytes[..body_start - 4]).unwrap();
    ObservedRequest {
        body: serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap(),
        authorization: header.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim().to_owned())
        }),
    }
}

impl ReadFileServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("automation fixture accept: {error}"),
                };
                let request = read_http_request(&mut stream);
                let has_result = request.body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "tool" && message["tool_call_id"] == CALL_ID);
                observed.lock().unwrap().push(request);
                let delta = if has_result {
                    json!({"content":FINAL})
                } else {
                    json!({"tool_calls":[{"index":0,"id":CALL_ID,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"payload.txt\"}"}}]})
                };
                let finish = if has_result { "stop" } else { "tool_calls" };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"automation-resume-response","choices":[{"index":0,"delta":delta}]}),
                    json!({"id":"automation-resume-response","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":100,"completion_tokens":12,"total_tokens":112}}),
                );
                // Streaming transport is real, including a split UTF-8 scalar.
                let split = body.find('ñ').map_or(body.len() / 2, |index| index + 1);
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                // A process cut after a committed response may close the socket;
                // no further data is needed for recovery in that case.
                let _ = stream.write_all(&body.as_bytes()[..split]);
                let _ = stream.flush();
                let _ = stream.write_all(&body.as_bytes()[split..]);
                let _ = stream.flush();
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<ObservedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ReadFileServer {
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

struct AutomationFixture {
    root: tempfile::TempDir,
    session: Session,
    frozen: FrozenNativeAutomationAuthorityV1,
}

impl AutomationFixture {
    fn new(endpoint: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let data_root = root.path().join("data");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        fs::write(workspace.join("payload.txt"), PAYLOAD).unwrap();
        fs::write(workspace.join("AGENTS.md"), INSTRUCTIONS).unwrap();
        let automation_id = uuid::Uuid::new_v4().to_string();
        let scheduled_at_ms = 123_456;
        let run_id = format!("{automation_id}:{scheduled_at_ms}");
        let env_name = format!(
            "OPENIDE_AUTOMATION_RESUME_ABSENT_{}",
            uuid::Uuid::new_v4().simple()
        )
        .to_ascii_uppercase();
        assert!(std::env::var_os(&env_name).is_none());
        let mut adapter = ProviderRegistry::builtin()
            .adapter("custom")
            .unwrap()
            .clone();
        adapter.id = PROVIDER.into();
        adapter.name = "Real local automation recovery fixture".into();
        adapter.endpoint = endpoint.into();
        adapter.key_env = env_name;
        adapter.key_optional = false;
        adapter.user_endpoint = false;
        adapter.wire_api = NativeWireApi::ChatCompletions;
        adapter.reasoning_effort_field = None;
        adapter.chat_request_fields = BTreeMap::new();
        fs::write(
            data_root.join("provider-registry.json"),
            serde_json::to_vec(&json!({"version":1,"providers":[adapter]})).unwrap(),
        )
        .unwrap();
        let discoveries = vec![ProviderModelDiscovery {
            provider_id: PROVIDER.into(),
            models: BTreeMap::from([(
                MODEL.into(),
                DiscoveredModel {
                    listing_index: None,
                    limits_declared: false,
                    efforts_declared: false,
                    capabilities_declared: false,
                    max_input_tokens: None,
                    declared_capabilities: Default::default(),
                    id: MODEL.into(),
                    name: "Exact recovery fixture".into(),
                    provider_reports_tools: true,
                    tool_call_declared: Some(true),
                    text_input_output: true,
                    supported_efforts: vec![],
                    context_window_tokens: Some(32_000),
                    max_output_tokens: Some(4096),
                },
            )]),
        }];
        let frozen = FrozenNativeAutomationAuthorityV1::freeze(
            RuntimeConnection {
                auth: Default::default(),
                headers: Default::default(),
                endpoint: endpoint.into(),
                api_key: None,
                model_id: MODEL.into(),
                wire_api: NativeWireApi::ChatCompletions,
            },
            PROVIDER,
            &data_root,
            &workspace,
            None,
            &discoveries,
            None,
            None,
            INSTRUCTIONS.into(),
            PROMPT.into(),
            &run_id,
            false,
        )
        .unwrap();
        let sessions = SessionStore::open(root.path().join("sessions")).unwrap();
        let origin = frozen
            .to_origin(&automation_id, &run_id, scheduled_at_ms, false)
            .unwrap();
        let session = sessions
            .create_native_automation(&workspace, origin)
            .unwrap();
        sessions
            .admit_native_turn(
                &session.id,
                frozen.first_turn_id(),
                frozen.prompt(),
                MODEL,
                TurnMode::Build,
            )
            .unwrap();
        Self {
            root,
            session,
            frozen,
        }
    }

    fn sessions(&self) -> Arc<SessionStore> {
        Arc::new(SessionStore::open(self.root.path().join("sessions")).unwrap())
    }

    fn request(&self) -> ResumeWorkerRequest {
        let frozen = self.frozen.clone();
        ResumeWorkerRequest {
            session_id: self.session.id.clone(),
            native_automation: Some(frozen.clone()),
            subagent_settings: Default::default(),
            provider: frozen.provider_id().into(),
            endpoint: frozen.endpoint().into(),
            sessions: self.sessions(),
            checkpoints: Arc::new(
                CheckpointFileStore::new(self.root.path().join("sessions")).unwrap(),
            ),
            compatibility: frozen.compatibility(),
            model: frozen
                .fixture_model_with_key(Some(FIXTURE_KEY.into()))
                .unwrap(),
            base_tools: frozen.tools(),
            child_tools: frozen.tools(),
            child_command_profile: None,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: frozen.policy(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: frozen.budget(),
            child_identity_budget: None,
            system_instructions: frozen.instructions().into(),
            pre_context_instructions: None,
            limits: frozen.limits(),
        }
    }

    fn journal_bytes(&self) -> Vec<u8> {
        fs::read(
            self.root
                .path()
                .join("sessions")
                .join(format!("{}.jsonl", self.session.id)),
        )
        .unwrap()
    }

    fn change_mutable_authority(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let paths = [
            self.root.path().join("data/native-provider.json"),
            self.root.path().join("data/agent-tool-settings.json"),
            self.root.path().join("data/provider-registry.json"),
            self.root.path().join("data/native-command-hooks.json"),
            self.root.path().join("data/mcp-servers.json"),
            self.root
                .path()
                .join("sessions")
                .join(format!("{}.native-goals.json", self.session.id)),
            self.session.workspace.join("AGENTS.md"),
        ];
        paths
            .into_iter()
            .map(|path| {
                let bytes = b"MUTATED current authority: invalid JSON and UTF-8 \xff".to_vec();
                fs::write(&path, &bytes).unwrap();
                (path, bytes)
            })
            .collect()
    }
}

fn cut_name(event: &AgentEventKind) -> &'static str {
    match event {
        AgentEventKind::ToolCatalogCommitted { .. } => "catalog",
        AgentEventKind::ExecPolicyCommitted { .. } => "policy",
        AgentEventKind::SystemInstructionsCommitted { .. } => "instructions",
        AgentEventKind::ModelContextCommitted { .. } => "context",
        AgentEventKind::ModelRouteCommitted { .. } => "route",
        AgentEventKind::ModelRequestStarted { .. } => "request",
        AgentEventKind::ModelResponseCommitted { .. } => "response",
        AgentEventKind::ToolStarted { .. } => "tool_started",
        AgentEventKind::ToolFinished { .. } => "tool_finished",
        _ => "",
    }
}

struct ProcessCutJournal {
    sessions: Arc<SessionStore>,
    point: String,
}

impl NativeEventJournal for ProcessCutJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.sessions.append_native_event(event)?;
        if cut_name(&event.kind) == self.point {
            std::process::exit(CUT_EXIT);
        }
        Ok(())
    }
}

#[test]
#[ignore = "subprocess fixture: exits without destructors after a real persisted boundary"]
fn automation_process_cut_child() {
    let Ok(root) = std::env::var(CUT_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let session_id = std::env::var(CUT_SESSION).unwrap();
    let point = std::env::var(CUT_POINT).unwrap();
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    assert_eq!(lease.session_id(), session_id);
    let record = sessions.load(&session_id).unwrap().unwrap();
    let frozen = FrozenNativeAutomationAuthorityV1::from_session(&record.session)
        .unwrap()
        .unwrap();
    record
        .session
        .native_automation
        .as_ref()
        .unwrap()
        .validate_session(&record.session)
        .unwrap();
    // Explicit admission retry verifies the original typed payload and owner.
    // with_native_steering requires Arc identity with the journal; this fixture
    // delegates append to the store and therefore validates admission itself.
    let admission = sessions
        .admit_native_turn(
            &session_id,
            frozen.first_turn_id(),
            frozen.prompt(),
            frozen.model_id(),
            TurnMode::Build,
        )
        .unwrap();
    assert_eq!(record.events, vec![admission.clone()]);
    if point == "admitted" {
        std::process::exit(CUT_EXIT);
    }
    let mut runner = AgentRunner::new(
        frozen
            .fixture_model_with_key(Some(FIXTURE_KEY.into()))
            .unwrap(),
        frozen.tools(),
    )
    .with_limits(frozen.limits())
    .with_permission_policy(frozen.policy())
    .with_command_hooks(Arc::new(CommandHooks::default()))
    .with_system_instructions(frozen.instructions())
    .with_native_runtime_identity(frozen.provider_id(), frozen.endpoint())
    .with_event_journal(Arc::new(ProcessCutJournal {
        sessions: sessions.clone(),
        point: point.clone(),
    }));
    if let Some(budget) = frozen.budget() {
        runner = runner.with_token_budget_policy(budget);
    }
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session_id),
            sequence_start: admission.sequence,
            workspace: record.session.workspace,
            history: vec![],
            prompt: frozen.prompt().into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    while let Ok(event) = handle.events.recv_timeout(Duration::from_secs(10)) {
        assert!(
            !matches!(event.kind, AgentEventKind::PermissionRequested { .. }),
            "read-only fixture asked for permission"
        );
        assert!(
            !matches!(event.kind, AgentEventKind::TurnFailed { .. }),
            "runner failed before {point}: {:?}",
            event.kind
        );
    }
    panic!("runner did not reach {point}");
}

fn run_cut(fixture: &AutomationFixture, point: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "resume_worker::automation_tests::automation_process_cut_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CUT_ROOT, fixture.root.path())
        .env(CUT_SESSION, &fixture.session.id)
        .env(CUT_POINT, point)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(CUT_EXIT),
        "cut {point}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn frozen_readonly_automation_resumes_after_real_process_cuts() {
    for point in [
        "admitted",
        "catalog",
        "policy",
        "instructions",
        "context",
        "route",
        "request",
        "response",
        "tool_started",
        "tool_finished",
    ] {
        let server = ReadFileServer::new();
        let fixture = AutomationFixture::new(&server.endpoint);
        run_cut(&fixture, point);
        assert_eq!(
            server.requests().len(),
            usize::from(matches!(
                point,
                "response" | "tool_started" | "tool_finished"
            )),
            "{point}: inference crossed the interruption boundary"
        );
        let sessions = fixture.sessions();
        let interrupted = sessions.load(&fixture.session.id).unwrap().unwrap();
        assert_eq!(
            interrupted.session.native_automation, fixture.session.native_automation,
            "{point}"
        );
        assert_eq!(
            FrozenNativeAutomationAuthorityV1::from_session(&interrupted.session).unwrap(),
            Some(fixture.frozen.clone())
        );
        let before = fixture.journal_bytes();
        if point != "admitted" {
            assert_eq!(cut_name(&interrupted.events.last().unwrap().kind), point);
        }
        let corrupted = fixture.change_mutable_authority();
        // Reopen both stores and reconstruct the DTO from metadata, not settings.
        let mut request = fixture.request();
        request.native_automation = FrozenNativeAutomationAuthorityV1::from_session(
            &request
                .sessions
                .load(&fixture.session.id)
                .unwrap()
                .unwrap()
                .session,
        )
        .unwrap();
        let handle = ResumeWorkerHandle::start(request).unwrap();
        let mut verification_count = 0;
        let outcome = loop {
            match handle
                .updates
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                ResumeUpdate::Events(events) => {
                    assert_eq!(events.len(), 1);
                    for event in events {
                        let saved = sessions.load(&fixture.session.id).unwrap().unwrap();
                        assert_eq!(
                            saved.events.get(event.sequence as usize - 1),
                            Some(&event),
                            "UI observed an event before its append"
                        );
                    }
                }
                ResumeUpdate::NeedsVerification {
                    tool_call,
                    executor_error,
                    goal_receipt,
                } => {
                    assert_eq!(point, "tool_started");
                    verification_count += 1;
                    assert_eq!(tool_call.id, CALL_ID);
                    assert_eq!(tool_call.name, "read_file");
                    assert_eq!(tool_call.arguments, json!({"path":"payload.txt"}));
                    assert!(executor_error.is_none());
                    assert!(goal_receipt.is_none());
                    assert_eq!(server.requests().len(), 1);
                    assert_eq!(fixture.journal_bytes(), before);
                    assert!(matches!(
                        handle.updates.recv_timeout(Duration::from_millis(50)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ));
                    assert_eq!(
                        fixture.journal_bytes(),
                        before,
                        "an uncertain read was automatically retried"
                    );
                    let independently_verified =
                        fs::read_to_string(fixture.session.workspace.join("payload.txt")).unwrap();
                    assert!(handle.record_verified_tool_outcome(
                        CALL_ID,
                        ToolOutput::success(independently_verified)
                    ));
                }
                ResumeUpdate::NeedsPermission { tool_call, .. } => {
                    panic!("unexpected read-only permission: {tool_call:?}")
                }
                ResumeUpdate::Finished(outcome) => break outcome,
            }
        };
        assert!(
            matches!(outcome, Ok(ResumeDisposition::Complete)),
            "{point}: {outcome:?}"
        );
        assert_eq!(verification_count, usize::from(point == "tool_started"));
        drop(handle);
        let final_record = fixture
            .sessions()
            .load(&fixture.session.id)
            .unwrap()
            .unwrap();
        let replay = replay_native(
            &fixture.session.id,
            &final_record.events,
            Some(&CheckpointFileStore::new(fixture.root.path().join("sessions")).unwrap()),
            &fixture.frozen.compatibility(),
        )
        .unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle, "{point}");
        assert_eq!(
            final_record.session.native_automation,
            fixture.session.native_automation
        );
        assert!(
            fixture.journal_bytes().starts_with(&before),
            "{point}: stored prefix was replaced"
        );
        assert_eq!(
            final_record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
                .count(),
            1
        );
        assert_eq!(
            final_record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            final_record
                .events
                .iter()
                .filter(|event| matches!(event.kind, AgentEventKind::ToolFinished { .. }))
                .count(),
            1
        );
        assert!(final_record.events.iter().any(|event| matches!(&event.kind, AgentEventKind::ToolFinished {output,is_error:false,..} if output == PAYLOAD)), "{point}: tool outcomes {:?}", final_record.events.iter().filter(|event| matches!(event.kind, AgentEventKind::ToolFinished {..})).collect::<Vec<_>>());
        assert!(final_record.events.iter().any(|event| matches!(&event.kind, AgentEventKind::TurnCompleted {response,..} if response == FINAL)));
        assert!(final_record.events.iter().any(|event| matches!(&event.kind, AgentEventKind::SystemInstructionsCommitted {content,..} if content == INSTRUCTIONS)));
        let requests = server.requests();
        assert_eq!(
            requests.len(),
            2,
            "{point}: unexpected inference repetition"
        );
        let expected_tools: Vec<Value> = fixture.frozen.catalog().unwrap().definitions().iter().map(|definition| json!({"type":"function","function":{"name":definition.name,"description":definition.description,"parameters":definition.parameters}})).collect();
        for request in &requests {
            assert_eq!(
                request.authorization.as_deref(),
                Some(format!("Bearer {FIXTURE_KEY}").as_str())
            );
            assert_eq!(request.body["model"], MODEL);
            assert_eq!(request.body["tools"], json!(expected_tools));
            assert!(request.body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains(INSTRUCTIONS));
            assert!(request.body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "user" && message["content"] == PROMPT));
            assert!(!request.body.to_string().contains("MUTATED"));
        }
        assert_eq!(
            requests[1].body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "tool")
                .unwrap()["content"],
            PAYLOAD
        );
        for (path, bytes) in corrupted {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert!(!final_record
            .session
            .native_automation
            .as_ref()
            .unwrap()
            .authority_snapshot_json
            .contains(FIXTURE_KEY));
        // Exact admission ACK remains idempotent after the recovered turn.
        let saved = fixture.journal_bytes();
        fixture
            .sessions()
            .admit_native_turn(
                &fixture.session.id,
                fixture.frozen.first_turn_id(),
                PROMPT,
                MODEL,
                TurnMode::Build,
            )
            .unwrap();
        assert_eq!(fixture.journal_bytes(), saved);
    }
}

struct CanaryPreparation(Arc<AtomicUsize>);

#[async_trait]
impl ToolPreparation for CanaryPreparation {
    async fn prepare(
        &self,
        _workspace: &Path,
        _tools: Arc<dyn ToolExecutor>,
        _cancel: &CancellationToken,
    ) -> Result<Box<dyn PreparedToolSession>, AgentError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Tool("canary preparation must never run".into()))
    }
}

#[test]
fn automation_resume_rejects_mixed_authority_before_preparation_request_or_append() {
    let server = ReadFileServer::new();
    let fixture = AutomationFixture::new(&server.endpoint);
    let other = AutomationFixture::new(&server.endpoint);
    let prep_calls = Arc::new(AtomicUsize::new(0));
    let mut cases = vec![
        "missing_origin",
        "foreign_origin",
        "provider",
        "endpoint",
        "compatibility",
        "model_profile",
        "preparation",
        "tools",
        "policy",
        "instructions",
        "pre_context",
        "limits",
        "budget",
    ];
    #[cfg(unix)]
    cases.push("hooks");
    for case in cases {
        let mut request = fixture.request();
        match case {
            "missing_origin" => request.native_automation = None,
            "foreign_origin" => request.native_automation = Some(other.frozen.clone()),
            "provider" => request.provider = "current-settings-provider".into(),
            "endpoint" => request.endpoint = "http://127.0.0.1:9/v1".into(),
            "compatibility" => request.compatibility.model = "current-settings-model".into(),
            "model_profile" => {
                request.model = Arc::new(
                    openide_harness::OpenAiCompatibleBackend::new(&server.endpoint, None, MODEL)
                        .with_request_field("temperature", json!(0.37))
                        .unwrap(),
                )
            }
            "preparation" => {
                request.tool_preparation = Some(Arc::new(CanaryPreparation(prep_calls.clone())))
            }
            "tools" => request.base_tools = Arc::new(LocalTools::new().with_write_access(true)),
            "policy" => {
                request.permission_policy =
                    PermissionPolicy::new().with_tool("write_file", PermissionMode::Allow)
            }
            "instructions" => request.system_instructions = "current settings instructions".into(),
            "pre_context" => {
                request.pre_context_instructions = Some(("current AGENTS".into(), None))
            }
            "limits" => request.limits.max_model_steps += 1,
            "budget" => {
                request.budget_policy = Some(TokenBudgetPolicy {
                    input_limit_tokens: None,
                    context_window_tokens: 32_000,
                    output_reserve_tokens: 1000,
                    tool_schema_reserve_tokens: 1000,
                    auto_compact_token_limit: None,
                })
            }
            #[cfg(unix)]
            "hooks" => {
                request.command_hooks = Arc::new(
                    CommandHooks::new(vec![CommandHookConfig {
                        id: "current-settings-hook".into(),
                        phase: HookPhase::PreToolUse,
                        tool_name: "read_file".into(),
                        executable: "/bin/sh".into(),
                        args: vec!["-c".into(), "printf effect > forbidden-hook-effect".into()],
                        cwd: fixture.session.workspace.clone(),
                        timeout_ms: 1000,
                        max_output_bytes: 1024,
                    }])
                    .unwrap(),
                )
            }
            _ => unreachable!(),
        }
        let before = fixture.journal_bytes();
        let handle = ResumeWorkerHandle::start(request).unwrap();
        match handle.updates.recv_timeout(Duration::from_secs(5)).unwrap() {
            ResumeUpdate::Finished(Err(error)) => assert!(!error.is_empty(), "{case}"),
            update => panic!("{case} bypassed frozen authority: {update:?}"),
        }
        drop(handle);
        assert_eq!(
            fixture.journal_bytes(),
            before,
            "{case}: rejection changed the journal"
        );
        assert!(
            server.requests().is_empty(),
            "{case}: rejection contacted a provider"
        );
        assert_eq!(prep_calls.load(Ordering::SeqCst), 0, "{case}");
        assert!(!fixture
            .session
            .workspace
            .join("forbidden-hook-effect")
            .exists());
    }
    // A valid request from the same admitted state completes through the real
    // transport; rejection evidence cannot be explained by a disconnected API.
    let handle = ResumeWorkerHandle::start(fixture.request()).unwrap();
    loop {
        match handle
            .updates
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            ResumeUpdate::Events(_) => {}
            ResumeUpdate::Finished(outcome) => {
                assert!(
                    matches!(outcome, Ok(ResumeDisposition::Complete)),
                    "{outcome:?}"
                );
                break;
            }
            update => panic!("valid automation paused unexpectedly: {update:?}"),
        }
    }
    assert_eq!(server.requests().len(), 2);
}
