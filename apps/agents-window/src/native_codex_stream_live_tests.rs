//! Explicit Codex account acceptance: one workspace read through the real
//! desktop transport, then durable replay and an idle Resume with no new send.
//! Credentials are copied privately and are never exported or printed.

use super::*;
use async_trait::async_trait;
use openide_core::ModelStreamEvent;
use openide_harness::{
    AgentError, ModelCompletion, ModelEventSender, ModelOutput, ResumeEngine, ToolCatalog,
    ToolContext, ToolDefinition,
};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::SystemTime;

const FILE_NAME: &str = "acceptance-readonly.txt";
const MARKER: &str = "OPENIDE_READONLY_STREAM_6BD397";
const PROMPT: &str = "Call read_file exactly once with path acceptance-readonly.txt. Then reply with only the marker written in that file. Do not call other tools, read other files, use the internet or change anything.";
const INSTRUCTIONS: &str = "This is an explicitly authorized native Codex streaming acceptance. Read only the one requested local file with read_file exactly once. After its result, return only the file marker. Do not inspect account data, other files or external resources.";

fn bounded_read(path: &Path, maximum: u64, private: bool) -> Vec<u8> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).expect("read the explicit source file");
    let metadata = file.metadata().expect("inspect source file");
    assert!(metadata.is_file() && metadata.len() <= maximum);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        assert!(metadata.permissions().mode() & 0o077 == 0);
        let parent = fs::symlink_metadata(path.parent().unwrap()).unwrap();
        assert!(parent.is_dir() && !parent.file_type().is_symlink());
        assert!(parent.permissions().mode() & 0o077 == 0);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() as u64 <= maximum);
    bytes
}

struct CountedProduction {
    model: Arc<dyn ModelBackend>,
    invocations: AtomicUsize,
}

#[async_trait]
impl ModelBackend for CountedProduction {
    fn model_id(&self) -> &str {
        self.model.model_id()
    }

    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        self.model.request_profile_fingerprint()
    }

    async fn complete(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ModelOutput, AgentError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        self.model.complete(messages, tools, cancel).await
    }

    async fn stream_events(
        &self,
        messages: &[AgentMessage],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
        events: ModelEventSender,
    ) -> Result<ModelCompletion, AgentError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        self.model
            .stream_events(messages, tools, cancel, events)
            .await
    }
}

struct CountedRead {
    local: LocalTools,
    sessions: Arc<SessionStore>,
    session_id: String,
    executions: AtomicUsize,
    durable_boundary: AtomicBool,
}

#[async_trait]
impl ToolExecutor for CountedRead {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.local
            .definitions()
            .into_iter()
            .filter(|tool| tool.name == "read_file")
            .collect()
    }

    fn certifies_parallel_read(&self, name: &str) -> bool {
        name == "read_file"
    }

    async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<ToolOutput, AgentError> {
        // This guard is a test boundary, not model-controlled permission.
        assert!(call.name == "read_file");
        assert!(call.arguments["path"] == FILE_NAME);
        assert!(
            self.executions.fetch_add(1, Ordering::SeqCst) == 0,
            "only one nonmutating file read is authorized"
        );
        let stored = self.sessions.load(&self.session_id).unwrap().unwrap();
        let commit = stored
            .events
            .iter()
            .rposition(|event| {
                matches!(
                    &event.kind, AgentEventKind::ModelResponseCommitted { assistant, .. }
                        if assistant.tool_calls.iter().any(|confirmed| confirmed == call)
                )
            })
            .expect("complete validated call must already be durable");
        let started = stored
            .events
            .iter()
            .rposition(|event| {
                matches!(
                    &event.kind, AgentEventKind::ToolStarted { tool_call } if tool_call == call
                )
            })
            .expect("dispatch must already be durable");
        assert!(
            commit < started,
            "response commit must precede tool dispatch"
        );
        assert!(!stored.events.iter().any(|event| matches!(
            &event.kind, AgentEventKind::ToolFinished { tool_call_id, .. } if tool_call_id == &call.id
        )));
        self.durable_boundary.store(true, Ordering::SeqCst);
        self.local.execute(call, context).await
    }
}

fn http_status(error: Option<&str>) -> Option<u16> {
    error?
        .strip_prefix("model request failed: ")?
        .strip_prefix("HTTP ")?
        .get(..3)?
        .parse()
        .ok()
}

#[test]
#[ignore = "requires explicit OPENIDE_CODEX_LIVE_ACCEPTANCE=1 and a fresh private saved connection; two native model steps and one readonly tool"]
fn saved_codex_build_stream_commits_tool_before_execute_and_idle_resume_sends_nothing() {
    assert!(
        std::env::var("OPENIDE_CODEX_LIVE_ACCEPTANCE").is_ok_and(|value| value == "1"),
        "live account acceptance requires explicit opt-in"
    );
    let original_root = data_root();
    let settings_path = original_root.join("native-provider.json");
    let settings_bytes = bounded_read(&settings_path, 32 * 1024, false);
    let settings: NativeProviderSettings = serde_json::from_slice(&settings_bytes).unwrap();
    assert!(settings.provider_id == "codex" && !settings.model_id.is_empty());
    let credential_path = original_root.join("provider-credentials/codex.json");
    let credential_bytes = bounded_read(&credential_path, 96 * 1024, true);
    let credential: Value = serde_json::from_slice(&credential_bytes).unwrap();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(credential["expires_at"].as_u64().is_some_and(|expires| expires >= now.saturating_add(600)), "at least 600 seconds of validity required; no credential refresh or rotation is authorized");
    assert!(credential["oauth"] == true && credential["disabled"] == false);
    assert!(
        crate::provider_connections::read_key(&original_root, "OPENIDE_CODEX_ACCESS_TOKEN")
            .is_some()
    );

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let workspace = temporary.path().join("workspace");
    let private_directory = root.join("provider-credentials");
    fs::create_dir_all(&private_directory).unwrap();
    fs::create_dir(&workspace).unwrap();
    let workspace_bytes = format!("{MARKER}\n").into_bytes();
    fs::write(workspace.join(FILE_NAME), &workspace_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        fs::set_permissions(&private_directory, fs::Permissions::from_mode(0o700)).unwrap();
        let mut copy = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(private_directory.join("codex.json"))
            .unwrap();
        copy.write_all(&credential_bytes).unwrap();
        copy.sync_all().unwrap();
    }
    #[cfg(not(unix))]
    fs::write(private_directory.join("codex.json"), &credential_bytes).unwrap();
    settings.save(&root).unwrap();
    let registry_path = original_root.join("provider-registry.json");
    let registry_bytes = registry_path
        .exists()
        .then(|| bounded_read(&registry_path, 512 * 1024, false));
    if let Some(bytes) = &registry_bytes {
        fs::write(root.join("provider-registry.json"), bytes).unwrap();
    }
    let original_connection = settings.resolve_at(&original_root).unwrap();
    let connection = settings.resolve_at(&root).unwrap();
    assert!(
        connection.model_id == settings.model_id
            && connection.endpoint == original_connection.endpoint
            && connection.wire_api == NativeWireApi::Responses
    );
    let endpoint = connection.endpoint.clone();
    let backend = native_model_backend(connection, "codex", None, None, &root, None, &[])
        .expect("production desktop factory accepts the saved exact route");
    let model = Arc::new(CountedProduction {
        model: backend,
        invocations: AtomicUsize::new(0),
    });
    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    let tools = Arc::new(CountedRead {
        local: LocalTools::new().with_write_access(false),
        sessions: sessions.clone(),
        session_id: session.id.clone(),
        executions: AtomicUsize::new(0),
        durable_boundary: AtomicBool::new(false),
    });
    let catalog = ToolCatalog::from_executor(tools.as_ref()).unwrap();
    assert!(catalog.definitions().len() == 1 && catalog.definitions()[0].name == "read_file");
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "live-codex-build-stream",
            PROMPT,
            &settings.model_id,
            TurnMode::Build,
        )
        .unwrap();
    let runner = AgentRunner::new(model.clone(), tools.clone())
        .with_limits(RunLimits {
            max_model_steps: 2,
            max_tool_calls: 1,
            max_tool_output_chars: 16_000,
        })
        .with_system_instructions(INSTRUCTIONS)
        .with_native_runtime_identity("codex", &endpoint)
        .with_event_journal(sessions.clone())
        .with_native_steering(sessions.clone());
    let handle = runner.start_with_lease(
        AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.clone(),
            history: vec![],
            prompt: PROMPT.into(),
            mode: TurnMode::Build,
        },
        sessions.try_acquire_native_lease(&session.id).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut deadline_cancelled = false;
    let mut live = TranscriptProjection::new();
    loop {
        match handle.events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                live.apply_event(&event);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !deadline_cancelled {
                    handle.cancel();
                    deadline_cancelled = true;
                }
                assert!(
                    Instant::now() < deadline + Duration::from_secs(10),
                    "native cancellation exceeded grace period"
                );
            }
        }
    }
    assert!(
        handle.journal_error().is_none(),
        "native journal publication failed"
    );
    let stored = sessions.load(&session.id).unwrap().unwrap();
    let logical_requests = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
        .count();
    let response_commits = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }))
        .count();
    let provisional_calls = stored
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                AgentEventKind::ModelStreamUpdate {
                    update: ModelStreamEvent::ToolCallReady { .. },
                    ..
                }
            )
        })
        .count();
    let tool_finished = stored.events.iter().filter(|event| matches!(&event.kind, AgentEventKind::ToolFinished { output, is_error: false, .. } if output.contains(MARKER))).count();
    let completed = stored.events.iter().any(|event| matches!(&event.kind, AgentEventKind::TurnCompleted { response, .. } if response.trim() == MARKER));
    let failure = stored.events.iter().find_map(|event| match &event.kind {
        AgentEventKind::TurnFailed { error } => Some(error.as_str()),
        _ => None,
    });
    // A closed diagnostic excludes arbitrary provider bodies, tokens, account
    // fields and raw event payloads. Temporary sensitive data is dropped.
    let diagnostic = serde_json::json!({ "provider":"codex", "model":settings.model_id, "endpoint":endpoint, "logical_model_requests":logical_requests, "responses_committed":response_commits, "provisional_calls_ready":provisional_calls, "read_executions":tools.executions.load(Ordering::SeqCst), "read_result_committed":tool_finished, "durable_before_execute":tools.durable_boundary.load(Ordering::SeqCst), "completed":completed, "deadline_cancelled":deadline_cancelled, "http_status":http_status(failure), "protocol_error":failure.is_some_and(|error| error.strip_prefix("model request failed: ").is_some_and(|error| error.starts_with("Responses ") || error.starts_with("invalid Responses "))) });
    println!("{diagnostic}");
    assert!(
        completed,
        "live readonly Build acceptance failed; diagnostic has closed non-secret fields"
    );
    assert!(
        logical_requests == 2
            && response_commits == 2
            && model.invocations.load(Ordering::SeqCst) == 2
    );
    assert!(
        tools.executions.load(Ordering::SeqCst) == 1
            && tools.durable_boundary.load(Ordering::SeqCst)
            && tool_finished == 1
    );
    assert!(
        provisional_calls == 1,
        "one typed provider tool completion must reach the durable journal"
    );
    assert!(
        stored
            .events
            .iter()
            .filter(|event| matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnCancelled
                    | AgentEventKind::TurnFailed { .. }
            ))
            .count()
            == 1
    );
    assert!(bounded_read(&settings_path, 32 * 1024, false) == settings_bytes);
    assert!(bounded_read(&credential_path, 96 * 1024, true) == credential_bytes);
    assert!(
        bounded_read(&private_directory.join("codex.json"), 96 * 1024, true) == credential_bytes,
        "temporary credentials must not refresh"
    );
    if let Some(bytes) = &registry_bytes {
        assert!(bounded_read(&registry_path, 512 * 1024, false) == *bytes);
    }
    assert!(fs::read(workspace.join(FILE_NAME)).unwrap() == workspace_bytes);
    drop(handle);
    drop(runner);
    let reopened = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let record = reopened.load(&session.id).unwrap().unwrap();
    assert!(record.events == stored.events);
    let checkpoints = Arc::new(CheckpointFileStore::new(root.join("sessions")).unwrap());
    let compatibility = CheckpointCompatibility {
        provider: "codex".into(),
        model: settings.model_id.clone(),
        context_format_key: "live-codex-build-stream".into(),
    };
    let replay = replay_native(
        &session.id,
        &record.events,
        Some(&checkpoints),
        &compatibility,
    )
    .unwrap();
    assert!(matches!(replay.recovery, ReplayRecovery::Idle));
    assert!(TranscriptProjection::from_events(&record.events).items() == live.items());
    let engine = ResumeEngine::new(
        reopened.clone(),
        checkpoints,
        compatibility,
        model.clone(),
        tools.clone(),
        catalog,
        PermissionPolicy::new(),
        None::<TokenBudgetPolicy>,
    )
    .with_native_runtime_identity("codex", &endpoint)
    .with_system_instructions(INSTRUCTIONS);
    let step = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(engine.resume_step(
            &reopened.try_acquire_native_lease(&session.id).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
    assert!(step.disposition == ResumeDisposition::Complete && step.appended_events.is_empty());
    assert!(
        model.invocations.load(Ordering::SeqCst) == 2
            && tools.executions.load(Ordering::SeqCst) == 1,
        "idle Resume must not contact the model or rerun the read"
    );
    assert!(reopened.load(&session.id).unwrap().unwrap().events == stored.events);
    println!("{{\"journal_reopened\":true,\"replay_idle\":true,\"resume_idle_no_model_send\":true,\"workspace_and_personal_files_unchanged\":true}}");
}
