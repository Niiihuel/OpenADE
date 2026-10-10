//! Direct Anthropic Messages protocol acceptance using a loopback fixture.
//! Protocol reference: https://platform.claude.com/docs/en/build-with-claude/streaming
//! No account credentials or third-party agent processes are used.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentRunRequest, CheckpointBudgetSnapshot,
    CheckpointCompatibility, CheckpointFileStore, NativeEventJournal, ReplayRecovery, SessionStore,
    StoreError, ToolCall, TurnMode, UserImage, UserImageMime,
};
use openide_harness::{
    AgentError, AgentRunner, AnthropicMessagesBackend, CheckpointCoordinator, LocalTools,
    ModelBackend, PermissionPolicy, ResumeDisposition, ResumeEngine, RunLimits, TokenBudgetPolicy,
    ToolCatalog, ToolContext, ToolDefinition, ToolExecutor, ToolOutput,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "fixture-exact-anthropic-model";
const PROVIDER: &str = "fixture-anthropic-provider";
const KEY: &str = "loopback-fixture-key";
const INSTRUCTIONS: &str =
    "Original project instructions: read payload.txt once; preserve opaque provider blocks.";
const PLAN_PROMPT: &str = "Plan a single verified read of payload.txt";
const BUILD_PROMPT: &str = "Build the agreed plan by reading payload.txt";
const IMAGE_PROMPT: &str = "Inspect the attached fixture image";
const PLAN_TEXT: &str = "Read payload.txt once and report its exact Unicode contents.";
const FINAL_TEXT: &str = "Verified payload résumé 🌱 without repeating the read.";
const PAYLOAD: &str = "payload résumé 🌱\nline two\n";
const CALL_ID: &str = "toolu_opaque_read_01";
const MESSAGE_ID: &str = "msg_opaque_build_01";
const CUT_EXIT: i32 = 86;

fn compatibility() -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: PROVIDER.into(),
        model: MODEL.into(),
        context_format_key: "anthropic-messages-v1".into(),
    }
}

fn configured_backend(endpoint: &str, model: &str) -> AnthropicMessagesBackend {
    AnthropicMessagesBackend::new(endpoint, Some(KEY.into()), model)
        .with_request_fields(BTreeMap::from([(
            "thinking".into(),
            json!({"type":"enabled","budget_tokens":1024}),
        )]))
        .unwrap()
}

fn backend(endpoint: &str) -> Arc<AnthropicMessagesBackend> {
    Arc::new(configured_backend(endpoint, MODEL))
}

fn opaque_build_blocks() -> Vec<Value> {
    vec![
        json!({"type":"thinking","thinking":"Opaque résumé 🌱 reasoning; do not reinterpret.","signature":"signature_opaque_+/=01"}),
        json!({"type":"redacted_thinking","data":"opaque_encrypted_+/=02"}),
        json!({"type":"text","text":"Reading the agreed file."}),
        json!({"type":"tool_use","id":CALL_ID,"name":"read_file","input":{"path":"payload.txt"}}),
    ]
}

fn opaque_build_items(endpoint: &str) -> Vec<Value> {
    vec![
        json!({"type":"openide_anthropic_message_v1","model_id":MODEL,
        "endpoint_digest":openide_core::native_model_endpoint_digest(&format!("{endpoint}/messages")).unwrap(),
        "message_id":MESSAGE_ID,"content":opaque_build_blocks()}),
    ]
}

fn text_blocks(text: &str) -> Vec<Value> {
    vec![json!({"type":"text","text":text})]
}

fn message_json(id: &str, blocks: &[Value], stop: &str) -> Value {
    json!({"id":id,"type":"message","role":"assistant","model":MODEL,
        "content":blocks,"stop_reason":stop,"stop_sequence":null,
        "usage":{"input_tokens":123,"output_tokens":17}})
}

fn streaming_events(id: &str, blocks: &[Value], stop: &str) -> Vec<Value> {
    let mut events = vec![json!({"type":"message_start","message":{
        "id":id,"type":"message","role":"assistant","model":MODEL,
        "content":[],"stop_reason":null,"stop_sequence":null,
        "usage":{"input_tokens":123,"output_tokens":0}}})];
    events.push(json!({"type":"ping"}));
    for (index, block) in blocks.iter().enumerate() {
        match block["type"].as_str().unwrap() {
            "thinking" => {
                events.push(json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"thinking","thinking":"","signature":""}}));
                events.push(json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"thinking_delta","thinking":block["thinking"]}}));
                events.push(json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"signature_delta","signature":block["signature"]}}));
            }
            "text" => {
                events.push(json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"text","text":""}}));
                events.push(json!({"type":"content_block_delta","index":index,
                    "delta":{"type":"text_delta","text":block["text"]}}));
            }
            "tool_use" => {
                events.push(json!({"type":"content_block_start","index":index,
                    "content_block":{"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}}));
                for fragment in ["{\"path\":\"payload", ".txt\"}"] {
                    events.push(json!({"type":"content_block_delta","index":index,
                        "delta":{"type":"input_json_delta","partial_json":fragment}}));
                }
            }
            "redacted_thinking" => events
                .push(json!({"type":"content_block_start","index":index,"content_block":block})),
            other => panic!("unsupported fixture block {other}"),
        }
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":17}}));
    events.push(json!({"type":"message_stop"}));
    events
}

#[derive(Clone, Debug)]
struct HttpRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn read_request(socket: &TcpStream) -> HttpRequest {
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    assert_eq!(parts.next(), Some("POST"));
    let path = parts.next().unwrap().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers["content-length"].parse().unwrap();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    HttpRequest {
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap(),
    }
}

fn respond(mut socket: TcpStream, content_type: &str, body: String) {
    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    socket.flush().unwrap();
}

fn respond_sse(socket: TcpStream, id: &str, blocks: &[Value], stop: &str) {
    let body = streaming_events(id, blocks, stop)
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    respond(socket, "text/event-stream", body);
}

fn contains_tool_result(body: &Value) -> bool {
    body["messages"].as_array().unwrap().iter().any(|message| {
        message["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
    })
}

fn contains_user_text(body: &Value, text: &str) -> bool {
    body["messages"].as_array().unwrap().iter().any(|message| {
        message["role"] == "user"
            && (message["content"] == text
                || message["content"].as_array().is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block["type"] == "text" && block["text"] == text)
                }))
    })
}

struct HttpFixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !server_stop.load(Ordering::SeqCst) {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("loopback HTTP: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_request(&socket);
                assert_eq!(
                    request.headers.get("x-api-key").map(String::as_str),
                    Some(KEY)
                );
                assert_eq!(
                    request.headers.get("anthropic-version").map(String::as_str),
                    Some("2023-06-01")
                );
                assert_eq!(request.body["model"], MODEL);
                assert!(!request.body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "system"));
                captured.lock().unwrap().push(request.clone());
                if request.path == "/v1/messages/count_tokens" {
                    respond(
                        socket,
                        "application/json",
                        json!({"input_tokens":123}).to_string(),
                    );
                } else {
                    assert_eq!(request.path, "/v1/messages");
                    if request.body["stream"] == false {
                        let summary = json!({"objective":"Read payload once", "decisions":["Plan before Build"],"files":[{"path":"payload.txt","state":"verified"}],"current_state":"Read completed once","constraints":["Preserve opaque Anthropic blocks before compaction"],"remaining_work":[]});
                        respond(
                            socket,
                            "application/json",
                            message_json(
                                "msg_compact",
                                &text_blocks(&summary.to_string()),
                                "end_turn",
                            )
                            .to_string(),
                        );
                    } else if contains_tool_result(&request.body) {
                        respond_sse(socket, "msg_final", &text_blocks(FINAL_TEXT), "end_turn");
                    } else if contains_user_text(&request.body, BUILD_PROMPT) {
                        respond_sse(socket, MESSAGE_ID, &opaque_build_blocks(), "tool_use");
                    } else if contains_user_text(&request.body, IMAGE_PROMPT) {
                        respond_sse(
                            socket,
                            "msg_image",
                            &text_blocks("Image input accepted by loopback protocol fixture."),
                            "end_turn",
                        );
                    } else {
                        assert!(contains_user_text(&request.body, PLAN_PROMPT));
                        respond_sse(socket, "msg_plan", &text_blocks(PLAN_TEXT), "end_turn");
                    }
                }
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn captured(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            if !std::thread::panicking() {
                thread.join().unwrap();
            } else {
                let _ = thread.join();
            }
        }
    }
}

struct ObservedReads {
    local: LocalTools,
    calls: AtomicUsize,
}

impl ObservedReads {
    fn new() -> Self {
        Self {
            local: LocalTools::new().with_write_access(false),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ToolExecutor for ObservedReads {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.local.definitions()
    }
    fn certifies_plan_read(&self, name: &str) -> bool {
        self.local.certifies_plan_read(name)
    }
    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        assert_eq!(call.name, "read_file");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.local.execute(call, context).await
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn run_plan(sessions: Arc<SessionStore>, session_id: &str, workspace: &Path, endpoint: &str) {
    let lease = sessions.try_acquire_native_lease(session_id).unwrap();
    let admission = sessions
        .admit_native_turn(session_id, "plan-1", PLAN_PROMPT, MODEL, TurnMode::Plan)
        .unwrap();
    let handle = AgentRunner::new(backend(endpoint), Arc::new(ObservedReads::new()))
        .with_system_instructions(INSTRUCTIONS)
        .with_native_runtime_identity(PROVIDER, endpoint)
        .with_event_journal(sessions.clone())
        .with_native_steering(sessions)
        .start_with_lease(
            AgentRunRequest {
                session_id: Some(session_id.into()),
                sequence_start: admission.sequence,
                workspace: workspace.into(),
                history: vec![],
                prompt: PLAN_PROMPT.into(),
                mode: TurnMode::Plan,
            },
            lease,
        );
    let mut proposed = false;
    loop {
        let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
        match event.kind {
            AgentEventKind::PlanProposed { .. } => proposed = true,
            AgentEventKind::TurnCompleted { .. } => {
                assert!(proposed);
                break;
            }
            AgentEventKind::TurnFailed { error } => panic!("Plan failed: {error}"),
            AgentEventKind::TurnCancelled => panic!("Plan cancelled"),
            _ => {}
        }
    }
    // The worker sends its terminal before releasing the lease.
    while handle.events.recv_timeout(Duration::from_secs(10)).is_ok() {}
}

struct CutJournal {
    sessions: Arc<SessionStore>,
    point: String,
}
impl NativeEventJournal for CutJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.sessions.append_native_event(event)?;
        let cut = match self.point.as_str() {
            "response_committed" => {
                matches!(&event.kind,AgentEventKind::ModelResponseCommitted {response_id:Some(id),..} if id == MESSAGE_ID)
            }
            "tool_started" => {
                matches!(&event.kind,AgentEventKind::ToolStarted {tool_call} if tool_call.id == CALL_ID)
            }
            _ => panic!("unknown cut point"),
        };
        if cut {
            std::process::exit(CUT_EXIT);
        }
        Ok(())
    }
}

#[test]
#[ignore = "subprocess fixture invoked by Anthropic process-cut acceptance"]
fn anthropic_process_cut_child() {
    let Ok(root) = std::env::var("OPENIDE_ANTHROPIC_CUT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let endpoint = std::env::var("OPENIDE_ANTHROPIC_CUT_ENDPOINT").unwrap();
    let session_id = std::env::var("OPENIDE_ANTHROPIC_CUT_SESSION").unwrap();
    let point = std::env::var("OPENIDE_ANTHROPIC_CUT_POINT").unwrap();
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let lease = sessions.try_acquire_native_lease(&session_id).unwrap();
    let record = sessions.load(&session_id).unwrap().unwrap();
    assert_eq!(
        sessions.native_session_for_lease(&lease).unwrap().id,
        session_id
    );
    let admission = record.events.last().unwrap();
    assert!(
        matches!(&admission.kind,AgentEventKind::TurnAdmitted {prompt,model,mode:TurnMode::Build,..} if prompt == BUILD_PROMPT && model == MODEL)
    );
    let prior = replay_native(
        &session_id,
        &record.events[..record.events.len() - 1],
        None,
        &compatibility(),
    )
    .unwrap();
    let handle = AgentRunner::new(backend(&endpoint), Arc::new(ObservedReads::new()))
        .with_system_instructions(INSTRUCTIONS)
        .with_native_runtime_identity(PROVIDER, &endpoint)
        .with_plan_snapshot(prior.latest_plan)
        // The wrapper preserves the exact store append, but is not the same
        // Arc as SessionStore: no native steering service is attached here.
        .with_event_journal(Arc::new(CutJournal { sessions, point }))
        .with_limits(RunLimits {
            max_model_steps: 3,
            max_tool_calls: 1,
            max_tool_output_chars: 2048,
        })
        .start_with_lease(
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
            "terminal before cut: {:?}",
            event.kind
        );
    }
    panic!("did not reach durable interruption");
}

fn engine(
    sessions: Arc<SessionStore>,
    checkpoints: Arc<CheckpointFileStore>,
    tools: Arc<ObservedReads>,
    model: Arc<dyn ModelBackend>,
    provider: &str,
    endpoint: &str,
) -> ResumeEngine {
    ResumeEngine::new(
        sessions,
        checkpoints,
        compatibility(),
        model,
        tools.clone(),
        ToolCatalog::new(tools.definitions()).unwrap(),
        PermissionPolicy::default(),
        None,
    )
    .with_system_instructions(INSTRUCTIONS)
    .with_native_runtime_identity(provider, endpoint)
}

fn assert_opaque_followup(request: &Value) {
    let messages = request["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message| message["role"] == "assistant"
        && message["content"] == json!(opaque_build_blocks())));
    let result = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["tool_use_id"], CALL_ID);
    assert_eq!(result["content"], PAYLOAD);
    assert!(result.get("is_error").is_none() || result["is_error"] == false);
    assert!(
        !request.to_string().contains("previous_response_id"),
        "Anthropic replay is stateless"
    );
}

#[test]
fn anthropic_plan_build_process_cuts_resume_opaque_tools_and_checkpoint_reopen() {
    for point in ["response_committed", "tool_started"] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("payload.txt"), PAYLOAD).unwrap();
        let fixture = HttpFixture::new();
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let session = sessions.create(&workspace, "native").unwrap();
        run_plan(sessions.clone(), &session.id, &workspace, &fixture.endpoint);
        let plan = replay_native(
            &session.id,
            &sessions.load(&session.id).unwrap().unwrap().events,
            None,
            &compatibility(),
        )
        .unwrap()
        .latest_plan
        .unwrap();
        sessions
            .admit_native_turn(&session.id, "build-1", BUILD_PROMPT, MODEL, TurnMode::Build)
            .unwrap();
        drop(sessions);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "anthropic_process_cut_child",
                "--ignored",
                "--nocapture",
            ])
            .env("OPENIDE_ANTHROPIC_CUT_ROOT", root.path())
            .env("OPENIDE_ANTHROPIC_CUT_SESSION", &session.id)
            .env("OPENIDE_ANTHROPIC_CUT_ENDPOINT", &fixture.endpoint)
            .env("OPENIDE_ANTHROPIC_CUT_POINT", point)
            .output()
            .unwrap();
        assert_eq!(
            child.status.code(),
            Some(CUT_EXIT),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
        let checkpoints =
            Arc::new(CheckpointFileStore::new(root.path().join("checkpoint-store")).unwrap());
        let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
        let record = sessions.load(&session.id).unwrap().unwrap();
        let assistant = record
            .events
            .iter()
            .find_map(|event| match &event.kind {
                AgentEventKind::ModelResponseCommitted {
                    assistant,
                    response_id: Some(id),
                    ..
                } if id == MESSAGE_ID => Some(assistant),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            assistant.provider_output_items,
            opaque_build_items(&fixture.endpoint)
        );
        assert_eq!(assistant.content, "Reading the agreed file.");
        for event in &record.events {
            if let AgentEventKind::AssistantDelta { content } = &event.kind {
                assert!(!content.contains("Opaque résumé"));
                assert!(!content.contains("signature_opaque"));
                assert!(!content.contains("opaque_encrypted"));
            }
        }
        assert_eq!(
            assistant.tool_calls,
            vec![ToolCall {
                id: CALL_ID.into(),
                name: "read_file".into(),
                arguments: json!({"path":"payload.txt"})
            }]
        );
        let before = std::fs::read(
            root.path()
                .join("sessions")
                .join(format!("{}.jsonl", session.id)),
        )
        .unwrap();
        let tools = Arc::new(ObservedReads::new());
        let runtime = runtime();
        // Exact provider, endpoint, model and request profile are all authority
        // inputs. None may change even when the saved response is complete.
        let changed_max = Arc::new(
            configured_backend(&fixture.endpoint, MODEL)
                .with_max_output_tokens(8192)
                .unwrap(),
        );
        let changed_model = Arc::new(configured_backend(&fixture.endpoint, "different-exact-id"));
        for (model, provider, endpoint) in [
            (
                backend(&fixture.endpoint) as Arc<dyn ModelBackend>,
                "changed-provider",
                fixture.endpoint.clone(),
            ),
            (
                backend(&fixture.endpoint) as Arc<dyn ModelBackend>,
                PROVIDER,
                format!("{}/changed", fixture.endpoint),
            ),
            (
                changed_max as Arc<dyn ModelBackend>,
                PROVIDER,
                fixture.endpoint.clone(),
            ),
            (
                changed_model as Arc<dyn ModelBackend>,
                PROVIDER,
                fixture.endpoint.clone(),
            ),
        ] {
            let wrong = engine(
                sessions.clone(),
                checkpoints.clone(),
                tools.clone(),
                model,
                provider,
                &endpoint,
            );
            assert!(runtime
                .block_on(wrong.run_until_pause(&lease, CancellationToken::new()))
                .is_err());
            assert_eq!(
                std::fs::read(
                    root.path()
                        .join("sessions")
                        .join(format!("{}.jsonl", session.id))
                )
                .unwrap(),
                before
            );
            assert_eq!(fixture.captured().len(), 2);
            assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
        }
        let recovered_engine = engine(
            sessions.clone(),
            checkpoints.clone(),
            tools.clone(),
            backend(&fixture.endpoint),
            PROVIDER,
            &fixture.endpoint,
        );
        if point == "tool_started" {
            let paused = runtime
                .block_on(recovered_engine.run_until_pause(&lease, CancellationToken::new()))
                .unwrap();
            assert!(matches!(
                paused.disposition,
                ResumeDisposition::ToolOutcomeUncertain { .. }
            ));
            assert_eq!(fixture.captured().len(), 2);
            assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
            let verified = std::fs::read_to_string(workspace.join("payload.txt")).unwrap();
            recovered_engine
                .record_verified_tool_outcome(&lease, CALL_ID, ToolOutput::success(verified))
                .unwrap();
        }
        let resumed = runtime
            .block_on(recovered_engine.run_until_pause(&lease, CancellationToken::new()))
            .unwrap();
        assert_eq!(resumed.disposition, ResumeDisposition::Complete);
        assert_eq!(
            tools.calls.load(Ordering::SeqCst),
            usize::from(point == "response_committed")
        );
        let requests = fixture.captured();
        assert_eq!(requests.len(), 3);
        assert_opaque_followup(&requests[2].body);
        let completed = sessions.load(&session.id).unwrap().unwrap();
        assert_eq!(completed.events.iter().filter(|event| matches!(&event.kind,AgentEventKind::ToolStarted {tool_call} if tool_call.id == CALL_ID)).count(),1);
        assert_eq!(completed.events.iter().filter(|event| matches!(&event.kind,AgentEventKind::ToolFinished {tool_call_id,..} if tool_call_id == CALL_ID)).count(),1);
        let coordinator = CheckpointCoordinator::new(
            sessions.clone(),
            checkpoints.clone(),
            backend(&fixture.endpoint),
            compatibility(),
            CheckpointBudgetSnapshot::unverified(5000, None),
        );
        let commit = runtime
            .block_on(coordinator.compact(&lease, CancellationToken::new()))
            .unwrap();
        drop(coordinator);
        drop(recovered_engine);
        drop(lease);
        drop(sessions);
        drop(checkpoints);
        let reopened = SessionStore::open(root.path().join("sessions")).unwrap();
        let checkpoints = CheckpointFileStore::new(root.path().join("checkpoint-store")).unwrap();
        let record = reopened.load(&session.id).unwrap().unwrap();
        let replay = replay_native(
            &session.id,
            &record.events,
            Some(&checkpoints),
            &compatibility(),
        )
        .unwrap();
        assert_eq!(replay.recovery, ReplayRecovery::Idle);
        assert_eq!(replay.latest_plan, Some(plan));
        assert!(checkpoints
            .load_after_commit(Some(&commit.reference))
            .unwrap()
            .is_some());
        assert!(std::fs::read(
            root.path()
                .join("sessions")
                .join(format!("{}.jsonl", session.id))
        )
        .unwrap()
        .starts_with(&before));
        assert!(record.events.iter().any(|event| matches!(&event.kind,AgentEventKind::ModelResponseCommitted {assistant,response_id:Some(id),..} if id == MESSAGE_ID && assistant.provider_output_items == opaque_build_items(&fixture.endpoint))));
        let reopened = Arc::new(reopened);
        let checkpoints = Arc::new(checkpoints);
        let reopened_lease = reopened.try_acquire_native_lease(&session.id).unwrap();
        let reopened_tools = Arc::new(ObservedReads::new());
        let reopened_engine = engine(
            reopened.clone(),
            checkpoints,
            reopened_tools.clone(),
            backend(&fixture.endpoint),
            PROVIDER,
            &fixture.endpoint,
        );
        let unchanged = std::fs::read(
            root.path()
                .join("sessions")
                .join(format!("{}.jsonl", session.id)),
        )
        .unwrap();
        let idle_resume = runtime
            .block_on(reopened_engine.run_until_pause(&reopened_lease, CancellationToken::new()))
            .unwrap();
        assert_eq!(idle_resume.disposition, ResumeDisposition::Complete);
        assert!(idle_resume.appended_events.is_empty());
        assert_eq!(reopened_tools.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("sessions")
                    .join(format!("{}.jsonl", session.id))
            )
            .unwrap(),
            unchanged
        );
        assert_eq!(fixture.captured().len(), 4);
        assert_eq!(
            std::fs::read_to_string(workspace.join("payload.txt")).unwrap(),
            PAYLOAD
        );
    }
}

#[test]
fn anthropic_inline_images_and_exact_count_use_the_same_native_wire_context_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let fixture = HttpFixture::new();
    let sessions = Arc::new(SessionStore::open(root.path().join("sessions")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    // Complete 1×1 RGB PNG, including valid chunk checksums and image data.
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC").unwrap();
    let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn_with_images(
            &session.id,
            "image-1",
            IMAGE_PROMPT,
            MODEL,
            TurnMode::Build,
            std::slice::from_ref(&image),
        )
        .unwrap();
    let handle = AgentRunner::new(backend(&fixture.endpoint), Arc::new(LocalTools::new()))
        .with_system_instructions(INSTRUCTIONS)
        .with_native_runtime_identity(PROVIDER, &fixture.endpoint)
        .with_event_journal(sessions.clone())
        .with_native_steering(sessions.clone())
        // The declared input ceiling is independent of output capacity. The
        // 4096-token response reserve must not be subtracted from 200 input.
        .with_token_budget_policy(TokenBudgetPolicy {
            context_window_tokens: 0,
            input_limit_tokens: Some(200),
            output_reserve_tokens: 4096,
            tool_schema_reserve_tokens: 0,
            auto_compact_token_limit: None,
        })
        .start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace: workspace.clone(),
                history: vec![],
                prompt: IMAGE_PROMPT.into(),
                mode: TurnMode::Build,
            },
            lease,
        );
    loop {
        let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
        match event.kind {
            AgentEventKind::TurnCompleted { .. } => break,
            AgentEventKind::TurnFailed { error } => panic!("image turn failed: {error}"),
            _ => {}
        }
    }
    while handle.events.recv_timeout(Duration::from_secs(10)).is_ok() {}
    let captured = fixture.captured();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0].path, "/v1/messages/count_tokens");
    assert_eq!(captured[1].path, "/v1/messages/count_tokens");
    assert_eq!(captured[2].path, "/v1/messages");
    for count in &captured[..2] {
        for field in ["messages", "system", "tools", "thinking"] {
            assert_eq!(
                count.body[field], captured[2].body[field],
                "count/create context field {field}"
            );
        }
    }
    let wire_image = captured[2].body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "image")
        .unwrap();
    assert_eq!(
        wire_image,
        &json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":image.data_base64}})
    );
    drop(sessions);
    let reopened = SessionStore::open(root.path().join("sessions")).unwrap();
    let record = reopened.load(&session.id).unwrap().unwrap();
    let replay = replay_native(&session.id, &record.events, None, &compatibility()).unwrap();
    assert_eq!(replay.recovery, ReplayRecovery::Idle);
    let retained_user = replay
        .history
        .iter()
        .find(|message| !message.images.is_empty())
        .unwrap();
    assert_eq!(retained_user.images, vec![image.clone()]);
    let recount = runtime()
        .block_on(backend(&fixture.endpoint).count_input_tokens(
            &replay.history,
            &[],
            CancellationToken::new(),
        ))
        .unwrap();
    assert_eq!(recount, Some(123));
    assert_eq!(fixture.captured().len(), 4);
    let recounted = fixture.captured().pop().unwrap();
    assert!(recounted.body.to_string().contains("image/png"));

    let reopened = Arc::new(reopened);
    let lease = reopened.try_acquire_native_lease(&session.id).unwrap();
    let admission = reopened
        .admit_native_turn_with_images(
            &session.id,
            "image-over-limit",
            IMAGE_PROMPT,
            MODEL,
            TurnMode::Build,
            std::slice::from_ref(&image),
        )
        .unwrap();
    let handle = AgentRunner::new(backend(&fixture.endpoint), Arc::new(LocalTools::new()))
        .with_system_instructions(INSTRUCTIONS)
        .with_native_runtime_identity(PROVIDER, &fixture.endpoint)
        .with_event_journal(reopened.clone())
        .with_native_steering(reopened.clone())
        .with_token_budget_policy(TokenBudgetPolicy {
            context_window_tokens: 0,
            input_limit_tokens: Some(100),
            output_reserve_tokens: 4096,
            tool_schema_reserve_tokens: 0,
            auto_compact_token_limit: None,
        })
        .start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace,
                history: replay.history,
                prompt: IMAGE_PROMPT.into(),
                mode: TurnMode::Build,
            },
            lease,
        );
    loop {
        let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
        match event.kind {
            AgentEventKind::TurnFailed { error } => {
                assert!(error.contains("compaction"), "{error}");
                break;
            }
            AgentEventKind::ModelRequestStarted { .. }
            | AgentEventKind::ToolStarted { .. }
            | AgentEventKind::TurnCompleted { .. } => {
                panic!("over-limit request was admitted to execution")
            }
            _ => {}
        }
    }
    while handle.events.recv_timeout(Duration::from_secs(10)).is_ok() {}
    let captured = fixture.captured();
    assert_eq!(captured.len(), 5);
    assert_eq!(captured[4].path, "/v1/messages/count_tokens");
    assert_eq!(
        captured
            .iter()
            .filter(|request| request.path == "/v1/messages")
            .count(),
        1
    );
}
