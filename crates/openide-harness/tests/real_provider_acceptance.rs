//! Opt-in acceptance against the two native provider transports.
//!
//! These tests can incur API charges. They never print credentials, prompts,
//! provider responses, or journal contents. See docs/native-real-provider-acceptance.md.

use openide_core::{
    replay_native, AgentEvent, AgentEventKind, AgentRunRequest, CheckpointCompatibility,
    CheckpointFileStore, ReplayRecovery, SessionStore, TurnMode, MODEL_CONTEXT_VERSION,
};
use openide_harness::{
    AgentRunner, LocalTools, ModelBackend, ModelRequestCapabilities, OpenAiCompatibleBackend,
    PermissionPolicy, ResponsesBackend, ResumeDisposition, ResumeEngine, RunLimits,
    TokenBudgetPolicy, ToolCatalog, ToolExecutor, ToolPermissions,
};
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OPT_IN: &str = "OPENIDE_REAL_PROVIDER_ACCEPTANCE";
const PLAN_PROMPT: &str =
    "Give a one-sentence plan for reading a local receipt, without calling any tool.";
const BUILD_PROMPT: &str = "Use read_file to read receipt.txt exactly once. Then report the exact marker from that file in your final answer. Do not use another tool.";
const LIMITS: RunLimits = RunLimits {
    max_model_steps: 3,
    max_tool_calls: 1,
    max_tool_output_chars: 2_000,
};

#[derive(Clone, Copy)]
enum Provider {
    OpenAiResponses,
    DeepSeekChat,
}

impl Provider {
    fn id(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai",
            Self::DeepSeekChat => "deepseek",
        }
    }

    fn base(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "https://api.openai.com/v1",
            Self::DeepSeekChat => "https://api.deepseek.com",
        }
    }

    fn key_env(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OPENAI_API_KEY",
            Self::DeepSeekChat => "DEEPSEEK_API_KEY",
        }
    }

    fn model_env(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OPENIDE_OPENAI_TEST_MODEL",
            Self::DeepSeekChat => "OPENIDE_DEEPSEEK_TEST_MODEL",
        }
    }

    fn context_format_key(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "responses-v1",
            Self::DeepSeekChat => "chat-v1",
        }
    }

    fn backend(self, key: &str, model: &str) -> Arc<dyn ModelBackend> {
        match self {
            Self::OpenAiResponses => Arc::new(ResponsesBackend::new(
                self.base(),
                Some(key.to_owned()),
                model,
            )),
            Self::DeepSeekChat => Arc::new(
                OpenAiCompatibleBackend::new(self.base(), Some(key.to_owned()), model)
                    .with_capabilities(ModelRequestCapabilities {
                        tool_calls: true,
                        ..Default::default()
                    })
                    .expect("acceptance route opts in to tool calls")
                    .with_request_field("thinking", serde_json::json!({"type":"disabled"}))
                    .expect("documented request option is valid"),
            ),
        }
    }

    fn compatibility(self, model: &str) -> CheckpointCompatibility {
        CheckpointCompatibility {
            provider: self.id().into(),
            model: model.into(),
            context_format_key: self.context_format_key().into(),
        }
    }
}

struct Credentials {
    key: String,
    model: String,
}

fn credentials(provider: Provider) -> Option<Credentials> {
    if env::var(OPT_IN).ok().as_deref() != Some("1") {
        eprintln!("real-provider acceptance skipped: explicit opt-in is absent");
        return None;
    }
    let key = env::var(provider.key_env())
        .ok()
        .filter(|value| !value.trim().is_empty());
    let model = env::var(provider.model_env())
        .ok()
        .filter(|value| !value.trim().is_empty());
    match (key, model) {
        (Some(key), Some(model)) => Some(Credentials { key, model }),
        _ => {
            eprintln!("real-provider acceptance skipped: provider key or model is absent");
            None
        }
    }
}

fn read_only_tools() -> Arc<LocalTools> {
    Arc::new(LocalTools::new().with_permissions(ToolPermissions {
        read_file: true,
        list_files: false,
        search_files: false,
        write_file: false,
        run_command: false,
    }))
}

fn assert_context_marker(events: &[AgentEvent]) {
    assert!(events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::ModelContextCommitted {
            version: MODEL_CONTEXT_VERSION,
            ..
        }
    )));
}

fn wait_for_completed_run(handle: openide_harness::AgentRunHandle) {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut completed = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            handle.cancel();
            panic!("real-provider Plan turn exceeded its time limit");
        }
        match handle
            .events
            .recv_timeout(remaining.min(Duration::from_secs(30)))
        {
            Ok(event) => {
                match event.kind {
                    AgentEventKind::TurnCompleted { .. } => completed = true,
                    AgentEventKind::TurnFailed { .. } | AgentEventKind::TurnCancelled => {
                        panic!("real-provider Plan turn did not complete; inspect local journal securely")
                    }
                    _ => assert!(!completed, "event arrived after terminal Plan event"),
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        completed,
        "real-provider Plan turn ended without completion"
    );
}

fn run_acceptance(provider: Provider) {
    let Some(credentials) = credentials(provider) else {
        return;
    };
    let root = tempfile::tempdir().expect("acceptance directory");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("acceptance workspace");
    let marker = Uuid::new_v4().to_string();
    std::fs::write(workspace.join("receipt.txt"), format!("marker: {marker}\n"))
        .expect("acceptance read fixture");
    let sessions_root = root.path().join("sessions");

    // Run Plan through the ordinary native worker, then close and reopen its
    // store to prove that the next turn uses disk replay rather than memory.
    let session_id = {
        let sessions = Arc::new(SessionStore::open(&sessions_root).expect("session store"));
        let session = sessions
            .create(&workspace, "native")
            .expect("native session");
        let lease = sessions
            .try_acquire_native_lease(&session.id)
            .expect("Plan session lease");
        let admission = sessions
            .admit_native_turn(
                &session.id,
                "acceptance-plan",
                PLAN_PROMPT,
                &credentials.model,
                TurnMode::Plan,
            )
            .expect("Plan admission");
        let runner = AgentRunner::new(
            provider.backend(&credentials.key, &credentials.model),
            read_only_tools(),
        )
        .with_native_runtime_identity(provider.id(), provider.base())
        .with_event_journal(sessions.clone())
        .with_limits(LIMITS);
        let handle = runner.start_with_lease(
            AgentRunRequest {
                session_id: Some(session.id.clone()),
                sequence_start: admission.sequence,
                workspace: workspace.clone(),
                history: Vec::new(),
                prompt: PLAN_PROMPT.into(),
                mode: TurnMode::Plan,
            },
            lease,
        );
        wait_for_completed_run(handle);
        session.id
    };

    let sessions = Arc::new(SessionStore::open(&sessions_root).expect("reopened session store"));
    let plan_record = sessions
        .load(&session_id)
        .expect("Plan journal")
        .expect("Plan session");
    let plan = replay_native(
        &session_id,
        &plan_record.events,
        None,
        &provider.compatibility(&credentials.model),
    )
    .expect("Plan journal replay");
    assert!(plan.can_start_new_turn, "Plan journal is not complete");
    assert_context_marker(&plan_record.events);
    assert!(plan_record.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::TurnAdmitted {
            mode: TurnMode::Plan,
            ..
        }
    )));
    assert!(!plan_record
        .events
        .iter()
        .any(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })));

    // Admit Build, reopen through ResumeEngine at AdmittedNotStarted, then
    // exercise one read_file call and its durable model continuation.
    let admission = {
        let lease = sessions
            .try_acquire_native_lease(&session_id)
            .expect("Build admission lease");
        let admission = sessions
            .admit_native_turn(
                &session_id,
                "acceptance-build-recovery",
                BUILD_PROMPT,
                &credentials.model,
                TurnMode::Build,
            )
            .expect("Build admission");
        drop(lease);
        admission
    };
    drop(sessions);
    let sessions = Arc::new(SessionStore::open(&sessions_root).expect("recovery session store"));
    let admitted = sessions
        .load(&session_id)
        .expect("admitted Build journal")
        .expect("admitted Build session");
    let interrupted = replay_native(
        &session_id,
        &admitted.events,
        None,
        &provider.compatibility(&credentials.model),
    )
    .expect("admitted Build replay");
    assert!(matches!(
        interrupted.recovery,
        ReplayRecovery::AdmittedNotStarted
    ));
    let lease = sessions
        .try_acquire_native_lease(&session_id)
        .expect("Build recovery lease");
    let checkpoints = Arc::new(
        CheckpointFileStore::new(root.path().join("checkpoints"))
            .expect("acceptance checkpoint store"),
    );
    let tools = read_only_tools();
    let catalog = ToolCatalog::new(tools.definitions()).expect("read-only tool catalog");
    let engine = ResumeEngine::new(
        sessions.clone(),
        checkpoints.clone(),
        provider.compatibility(&credentials.model),
        provider.backend(&credentials.key, &credentials.model),
        tools,
        catalog,
        PermissionPolicy::new(),
        None::<TokenBudgetPolicy>,
    )
    .with_native_runtime_identity(provider.id(), provider.base())
    .with_limits(LIMITS);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("acceptance runtime");
    let step = runtime
        .block_on(engine.run_until_pause(&lease, CancellationToken::new()))
        .unwrap_or_else(|_| {
            panic!("real-provider Build recovery failed; inspect local journal securely")
        });
    assert!(matches!(step.disposition, ResumeDisposition::Complete));
    assert!(step.replay.can_start_new_turn);
    drop(lease);
    drop(engine);
    drop(sessions);

    let sessions = SessionStore::open(&sessions_root).expect("final reopened session store");
    let record = sessions
        .load(&session_id)
        .expect("Build journal")
        .expect("Build session");
    let replay = replay_native(
        &session_id,
        &record.events,
        Some(&checkpoints),
        &provider.compatibility(&credentials.model),
    )
    .expect("Build journal replay");
    assert!(replay.can_start_new_turn);
    assert!(replay.active_turn.is_none());
    assert_context_marker(&record.events);
    assert!(record.events.iter().any(|event| matches!(
        event.kind,
        AgentEventKind::TurnAdmitted {
            mode: TurnMode::Build,
            ..
        }
    )));
    let build_events = record
        .events
        .iter()
        .filter(|event| event.sequence > admission.sequence)
        .collect::<Vec<_>>();
    let started = build_events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::ToolStarted { tool_call } => Some(tool_call),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 1, "Build must execute one local read");
    assert_eq!(started[0].name, "read_file");
    assert!(build_events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::ToolFinished {
            tool_call_id,
            output,
            is_error: false,
        } if tool_call_id == &started[0].id && output.contains(&marker)
    )));
    assert!(build_events.iter().any(|event| matches!(
        &event.kind,
        AgentEventKind::TurnCompleted { response, .. } if response.contains(&marker)
    )));
    assert!(
        std::fs::read_to_string(workspace.join("receipt.txt")).expect("read fixture remains")
            == format!("marker: {marker}\n"),
        "read fixture changed"
    );
    if matches!(provider, Provider::OpenAiResponses) {
        assert!(record.events.iter().any(|event| matches!(
            &event.kind,
            AgentEventKind::ModelResponseCommitted {
                response_id: Some(_),
                assistant,
                ..
            } if !assistant.provider_output_items.is_empty()
        )));
    }
}

#[test]
#[ignore = "requires explicit opt-in, OPENAI_API_KEY, and OPENIDE_OPENAI_TEST_MODEL"]
fn openai_responses_plan_build_and_journal_recovery() {
    run_acceptance(Provider::OpenAiResponses);
}

#[test]
#[ignore = "requires explicit opt-in, DEEPSEEK_API_KEY, and OPENIDE_DEEPSEEK_TEST_MODEL"]
fn deepseek_chat_plan_build_and_journal_recovery() {
    run_acceptance(Provider::DeepSeekChat);
}
