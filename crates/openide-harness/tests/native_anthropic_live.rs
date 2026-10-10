//! Explicit, ignored acceptance against the official Anthropic Messages API.
//! Requires OPENIDE_REAL_PROVIDER_ACCEPTANCE=1, ANTHROPIC_API_KEY, and an exact
//! OPENIDE_ANTHROPIC_TEST_MODEL. There is no default model or endpoint fallback.
//! Can incur provider charges; never prints keys, prompts, responses or events.
//! Contracts: https://platform.claude.com/docs/en/api/messages/create and
//! https://platform.claude.com/docs/en/api/messages/count_tokens.

use async_trait::async_trait;
use openide_core::{
    native_model_endpoint_digest, replay_native, AgentEvent, AgentEventKind, AgentMessage,
    AgentRunRequest, CheckpointCompatibility, CheckpointFileStore, ReplayRecovery, SessionStore,
    TurnMode, MODEL_CONTEXT_VERSION,
};
use openide_harness::{
    AgentError, AgentRunHandle, AgentRunner, AnthropicMessagesBackend, LocalTools, ModelBackend,
    ModelCompletion, ModelOutput, PermissionPolicy, ResumeDisposition, ResumeEngine, RunLimits,
    TokenBudgetPolicy, ToolCatalog, ToolDefinition, ToolExecutor, ToolPermissions,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const ENDPOINT: &str = "https://api.anthropic.com/v1";
const PLAN: &str = "Give a one-sentence plan for reading a local receipt. Do not call any tool.";
const BUILD: &str = "Use read_file to read receipt.txt exactly once. Then report the exact marker from that file in your final answer. Do not use another tool.";
const LIMITS: RunLimits = RunLimits {
    max_model_steps: 3,
    max_tool_calls: 1,
    max_tool_output_chars: 2_000,
};

struct Credentials {
    key: String,
    model: String,
}

fn credentials() -> Credentials {
    assert!(
        std::env::var("OPENIDE_REAL_PROVIDER_ACCEPTANCE").ok().as_deref() == Some("1"),
        "Anthropic acceptance requires explicit OPENIDE_REAL_PROVIDER_ACCEPTANCE=1; no HTTP was requested"
    );
    let key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty() && !value.contains(['\r', '\n']))
        .expect("Anthropic acceptance requires ANTHROPIC_API_KEY; no HTTP was requested");
    let model = std::env::var("OPENIDE_ANTHROPIC_TEST_MODEL").ok()
        .filter(|value| {
            !value.is_empty()
                && value.trim() == value
                && value.len() <= openide_core::MAX_MODEL_ROUTE_MODEL_BYTES
                && !value.chars().any(char::is_control)
        })
        .expect("Anthropic acceptance requires an exact OPENIDE_ANTHROPIC_TEST_MODEL; no HTTP was requested");
    Credentials { key, model }
}

/// Capture the histories supplied to the real adapter, without writing them
/// to stdout. This proves that durable opaque blocks survive into the actual
/// provider continuation; it does not alter protocol or response validation.
struct ObservedBackend {
    inner: AnthropicMessagesBackend,
    model_calls: AtomicUsize,
    count_calls: AtomicUsize,
    histories: Mutex<Vec<Vec<AgentMessage>>>,
}

impl ObservedBackend {
    fn new(credentials: &Credentials) -> Self {
        Self {
            inner: AnthropicMessagesBackend::new(
                ENDPOINT,
                Some(credentials.key.clone()),
                &credentials.model,
            )
            .with_image_input_enabled(false),
            model_calls: AtomicUsize::new(0),
            count_calls: AtomicUsize::new(0),
            histories: Mutex::new(Vec::new()),
        }
    }

    fn observe(&self, messages: &[AgentMessage]) {
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        self.histories
            .lock()
            .expect("private history lock")
            .push(messages.to_vec());
    }
}

#[async_trait]
impl ModelBackend for ObservedBackend {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        self.inner.request_profile_fingerprint()
    }
    fn supports_user_images(&self) -> bool {
        self.inner.supports_user_images()
    }

    async fn count_input_tokens(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<Option<u64>, AgentError> {
        self.count_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.count_input_tokens(messages, tools, cancel).await
    }

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        self.observe(messages);
        self.inner.complete(messages, tools, cancel).await
    }

    async fn stream_complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelOutput, AgentError> {
        self.observe(messages);
        self.inner
            .stream_complete(messages, tools, cancel, deltas)
            .await
    }

    async fn stream_complete_detailed(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        deltas: UnboundedSender<String>,
    ) -> Result<ModelCompletion, AgentError> {
        self.observe(messages);
        self.inner
            .stream_complete_detailed(messages, tools, cancel, deltas)
            .await
    }
}

fn read_tools() -> Arc<LocalTools> {
    Arc::new(LocalTools::new().with_permissions(ToolPermissions {
        read_file: true,
        list_files: false,
        search_files: false,
        write_file: false,
        run_command: false,
    }))
}

fn compatibility(model: &str) -> CheckpointCompatibility {
    CheckpointCompatibility {
        provider: "anthropic".into(),
        model: model.into(),
        context_format_key: "anthropic-messages-v1".into(),
    }
}

fn await_plan(handle: AgentRunHandle) {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut completed = false;
    let mut failed = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            handle.cancel();
            panic!("Anthropic Plan acceptance timed out; no provider output was printed");
        }
        match handle
            .events
            .recv_timeout(remaining.min(Duration::from_secs(30)))
        {
            Ok(event) => match event.kind {
                AgentEventKind::TurnCompleted { .. } => completed = true,
                AgentEventKind::TurnFailed { .. } | AgentEventKind::TurnCancelled => failed = true,
                _ => {}
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        completed && !failed,
        "Anthropic Plan did not complete; provider output remains private"
    );
}

fn validate_provider_history(events: &[AgentEvent], backend: &ObservedBackend) {
    let expected_endpoint = native_model_endpoint_digest(&format!("{ENDPOINT}/messages")).unwrap();
    let expected_route_endpoint = native_model_endpoint_digest(ENDPOINT).unwrap();
    let profile = openide_core::model_request_profile_digest(
        backend
            .request_profile_fingerprint()
            .expect("Messages request profile"),
    );
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ModelContextCommitted {
                version: MODEL_CONTEXT_VERSION,
                ..
            }
        )),
        "native context identity was not committed"
    );
    assert!(events.iter().any(|event| matches!(&event.kind,
        AgentEventKind::ModelRouteCommitted { provider_id, model_id, endpoint_digest, request_profile_digest, .. }
            if provider_id == "anthropic" && model_id == backend.model_id()
                && endpoint_digest == &expected_route_endpoint && request_profile_digest == &profile
    )), "exact Messages route and request policy were not committed");
    for event in events {
        let AgentEventKind::ModelResponseCommitted {
            assistant,
            response_id,
            usage,
            ..
        } = &event.kind
        else {
            continue;
        };
        let [item] = assistant.provider_output_items.as_slice() else {
            panic!("Anthropic response has no unique opaque envelope");
        };
        assert!(
            item["type"] == "openide_anthropic_message_v1",
            "foreign protocol envelope"
        );
        assert!(
            item["model_id"] == backend.model_id(),
            "foreign model envelope"
        );
        assert!(
            item["endpoint_digest"] == expected_endpoint,
            "foreign endpoint envelope"
        );
        assert!(
            item["message_id"].as_str() == response_id.as_deref(),
            "response identity was not preserved"
        );
        assert!(
            response_id.as_ref().is_some_and(|id| !id.is_empty()),
            "missing response identity"
        );
        assert!(usage.is_some(), "provider usage was not preserved");
        let blocks = item["content"]
            .as_array()
            .expect("ordered Messages content blocks");
        let mut text = String::new();
        let mut call_index = 0;
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => text.push_str(block["text"].as_str().expect("text block")),
                Some("thinking") => {
                    assert!(block["thinking"].is_string(), "thinking body was lost");
                    assert!(
                        block["signature"]
                            .as_str()
                            .is_some_and(|value| !value.is_empty()),
                        "thinking signature was lost"
                    );
                }
                Some("redacted_thinking") => {
                    assert!(
                        block["data"]
                            .as_str()
                            .is_some_and(|value| !value.is_empty()),
                        "redacted block was lost"
                    );
                }
                Some("tool_use") => {
                    let call = assistant
                        .tool_calls
                        .get(call_index)
                        .expect("canonical tool call");
                    assert!(
                        block["id"] == call.id
                            && block["name"] == call.name
                            && block["input"] == call.arguments,
                        "ordered tool block differs from canonical tool call"
                    );
                    call_index += 1;
                }
                _ => panic!("unsupported provider block reached durable history"),
            }
        }
        assert!(
            text == assistant.content,
            "opaque thinking was promoted to final text or text was lost"
        );
        assert!(
            call_index == assistant.tool_calls.len(),
            "canonical calls lost original blocks"
        );
    }
}

#[test]
#[ignore = "requires OPENIDE_REAL_PROVIDER_ACCEPTANCE=1, ANTHROPIC_API_KEY and exact OPENIDE_ANTHROPIC_TEST_MODEL; can incur API charges"]
fn anthropic_messages_plan_build_read_and_journal_recovery() {
    // All three gates run before creating any client, workspace or worker.
    // Missing configuration is a failing nonacceptance, never a silent pass.
    let credentials = credentials();
    let backend = Arc::new(ObservedBackend::new(&credentials));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("acceptance runtime");
    let count = runtime
        .block_on(backend.count_input_tokens(
            &[
                AgentMessage::system(
                    "Count this text request and its local read tool without running tools.",
                ),
                AgentMessage::user(PLAN),
            ],
            &read_tools().definitions(),
            CancellationToken::new(),
        ))
        .unwrap_or_else(|_| {
            panic!("Anthropic count_tokens acceptance failed; provider output remains private")
        });
    assert!(
        count.is_some_and(|tokens| tokens > 0),
        "provider did not return a positive text count"
    );
    assert!(
        backend.count_calls.load(Ordering::SeqCst) == 1,
        "unexpected count attempt"
    );

    let root = tempfile::tempdir().expect("acceptance directory");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("acceptance workspace");
    let marker = Uuid::new_v4().to_string();
    let receipt = format!("marker: {marker}\n");
    std::fs::write(workspace.join("receipt.txt"), &receipt).expect("local read fixture");
    let sessions_root = root.path().join("sessions");
    let checkpoints_root = root.path().join("checkpoint-store");
    let session_id = {
        let sessions = Arc::new(SessionStore::open(&sessions_root).expect("session store"));
        let session = sessions
            .create(&workspace, "native")
            .expect("native session");
        let lease = sessions
            .try_acquire_native_lease(&session.id)
            .expect("Plan lease");
        let admission = sessions
            .admit_native_turn(
                &session.id,
                "live-anthropic-plan",
                PLAN,
                &credentials.model,
                TurnMode::Plan,
            )
            .expect("Plan admission");
        let runner = AgentRunner::new(backend.clone(), read_tools())
            .with_native_runtime_identity("anthropic", ENDPOINT)
            .with_event_journal(sessions)
            .with_limits(LIMITS);
        await_plan(runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace: workspace.clone(),
                history: Vec::new(),
                prompt: PLAN.into(),
                mode: TurnMode::Plan,
            },
            lease,
        ));
        session.id
    };

    let sessions = Arc::new(SessionStore::open(&sessions_root).expect("reopened Plan store"));
    let plan_record = sessions
        .load(&session_id)
        .expect("Plan journal read")
        .expect("Plan session");
    let plan = replay_native(
        &session_id,
        &plan_record.events,
        None,
        &compatibility(&credentials.model),
    )
    .expect("Plan replay");
    assert!(plan.can_start_new_turn, "Plan did not reach idle");
    assert!(
        plan_record
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::PlanProposed { .. })),
        "Plan proposal was not preserved"
    );
    assert!(
        !plan_record
            .events
            .iter()
            .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })),
        "Plan executed a tool"
    );
    validate_provider_history(&plan_record.events, &backend);
    let admission = {
        let _lease = sessions
            .try_acquire_native_lease(&session_id)
            .expect("Build admission lease");
        sessions
            .admit_native_turn(
                &session_id,
                "live-anthropic-build",
                BUILD,
                &credentials.model,
                TurnMode::Build,
            )
            .expect("Build admission")
    };
    drop(sessions);

    let sessions =
        Arc::new(SessionStore::open(&sessions_root).expect("reopened admitted Build store"));
    let admitted = sessions
        .load(&session_id)
        .expect("Build journal read")
        .expect("Build session");
    let replay = replay_native(
        &session_id,
        &admitted.events,
        None,
        &compatibility(&credentials.model),
    )
    .expect("admitted Build replay");
    assert!(
        matches!(replay.recovery, ReplayRecovery::AdmittedNotStarted),
        "wrong recovery boundary"
    );
    let lease = sessions
        .try_acquire_native_lease(&session_id)
        .expect("Build recovery lease");
    let checkpoints =
        Arc::new(CheckpointFileStore::new(&checkpoints_root).expect("checkpoint store"));
    let tools = read_tools();
    let catalog = ToolCatalog::new(tools.definitions()).expect("read-only tool catalog");
    let engine = ResumeEngine::new(
        sessions.clone(),
        checkpoints,
        compatibility(&credentials.model),
        backend.clone(),
        tools,
        catalog,
        PermissionPolicy::new(),
        None::<TokenBudgetPolicy>,
    )
    .with_native_runtime_identity("anthropic", ENDPOINT)
    .with_limits(LIMITS);
    // No model context/input capacities were queried here. Do not invent a
    // verified window from default max_tokens, or treat the direct count above
    // as a verified ceiling. Separate HTTP tests prove budget enforcement.
    let cancel = CancellationToken::new();
    let step = runtime
        .block_on(async {
            let result = tokio::time::timeout(
                Duration::from_secs(300),
                engine.run_until_pause(&lease, cancel.clone()),
            )
            .await;
            if result.is_err() {
                cancel.cancel();
            }
            result
        })
        .unwrap_or_else(|_| {
            panic!("Anthropic Build recovery timed out; provider output remains private")
        })
        .unwrap_or_else(|_| {
            panic!("Anthropic Build recovery failed; provider output remains private")
        });
    assert!(
        matches!(step.disposition, ResumeDisposition::Complete) && step.replay.can_start_new_turn,
        "Build recovery did not complete"
    );
    drop(lease);
    drop(engine);
    drop(sessions);

    let sessions =
        Arc::new(SessionStore::open(&sessions_root).expect("final reopened session store"));
    let record = sessions
        .load(&session_id)
        .expect("final journal read")
        .expect("final session");
    let checkpoints =
        Arc::new(CheckpointFileStore::new(&checkpoints_root).expect("reopened checkpoint store"));
    let replay = replay_native(
        &session_id,
        &record.events,
        Some(&checkpoints),
        &compatibility(&credentials.model),
    )
    .expect("final replay");
    assert!(
        replay.can_start_new_turn && replay.active_turn.is_none(),
        "final journal is not idle"
    );
    validate_provider_history(&record.events, &backend);
    let build_events: Vec<_> = record
        .events
        .iter()
        .filter(|event| event.sequence > admission.sequence)
        .collect();
    let started: Vec<_> = build_events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } => Some(tool_call),
            _ => None,
        })
        .collect();
    assert!(started.len() == 1, "Build must execute exactly one read");
    assert!(
        started[0].name == "read_file" && started[0].arguments == json!({"path":"receipt.txt"}),
        "Build requested an unexpected tool or file"
    );
    assert!(
        build_events.iter().any(|event| matches!(&event.kind,
            AgentEventKind::ToolFinished { tool_call_id, output, is_error:false }
                if tool_call_id == &started[0].id && output.contains(&marker)
        )),
        "the local read result was not committed"
    );
    assert!(
        build_events.iter().any(|event| matches!(&event.kind,
            AgentEventKind::TurnCompleted { response, .. } if response.contains(&marker)
        )),
        "final response omitted the private receipt marker"
    );
    assert!(
        std::fs::read_to_string(workspace.join("receipt.txt")).expect("unchanged fixture")
            == receipt,
        "read fixture changed"
    );
    let tool_response = build_events
        .iter()
        .find_map(|event| match &event.kind {
            AgentEventKind::ModelResponseCommitted { assistant, .. }
                if !assistant.tool_calls.is_empty() =>
            {
                Some(assistant)
            }
            _ => None,
        })
        .expect("durable assistant tool request");
    assert!(
        backend
            .histories
            .lock()
            .expect("private histories")
            .iter()
            .any(|history| {
                history.iter().any(|message| {
                    message.role == openide_core::MessageRole::Assistant
                        && message.provider_output_items == tool_response.provider_output_items
                        && message.tool_calls == tool_response.tool_calls
                }) && history
                    .iter()
                    .any(|message| message.tool_call_id.as_deref() == Some(started[0].id.as_str()))
            }),
        "original ordered provider blocks were not replayed with the tool result"
    );

    // Idle Resume must inspect disk without contacting count/messages again.
    let calls = backend.model_calls.load(Ordering::SeqCst);
    let counts = backend.count_calls.load(Ordering::SeqCst);
    let lease = sessions
        .try_acquire_native_lease(&session_id)
        .expect("idle recovery lease");
    let tools = read_tools();
    let engine = ResumeEngine::new(
        sessions.clone(),
        checkpoints,
        compatibility(&credentials.model),
        backend.clone(),
        tools.clone(),
        ToolCatalog::new(tools.definitions()).expect("idle catalog"),
        PermissionPolicy::new(),
        None::<TokenBudgetPolicy>,
    )
    .with_native_runtime_identity("anthropic", ENDPOINT)
    .with_limits(LIMITS);
    let idle = runtime
        .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
        .unwrap_or_else(|_| {
            panic!("idle Anthropic Resume failed; provider output remains private")
        });
    assert!(
        matches!(idle.disposition, ResumeDisposition::Complete),
        "idle Resume did not complete"
    );
    assert!(
        backend.model_calls.load(Ordering::SeqCst) == calls
            && backend.count_calls.load(Ordering::SeqCst) == counts,
        "idle Resume made another provider request"
    );
    assert!(
        sessions
            .load(&session_id)
            .expect("unchanged idle journal")
            .expect("idle session")
            .events
            == record.events,
        "idle Resume changed the confirmed journal"
    );
}
