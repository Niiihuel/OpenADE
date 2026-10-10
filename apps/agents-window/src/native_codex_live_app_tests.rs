//! Explicit account acceptance through the desktop's own backend factory.
//! This does not run the Codex process or use its libraries. The saved exact
//! model and registered production endpoint are preserved. One model step can
//! use the transport's existing bounded transient retries (up to three sends).
//! A rejected request is a failed acceptance, not a successful account check.

use super::*;
use std::{fs, io::Read, time::SystemTime};

const PROMPT: &str = "Give a one-sentence plan for reading a local text file. Do not call any tools and do not execute the plan. Keep the answer under 30 words.";
const INSTRUCTIONS: &str = "This is an explicitly authorized OpenIDE native provider acceptance. Answer the requested brief plan. Do not call tools, access external resources, or include account information.";

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
        let parent =
            fs::symlink_metadata(path.parent().unwrap()).expect("inspect private source directory");
        assert!(parent.is_dir() && !parent.file_type().is_symlink());
        assert!(parent.permissions().mode() & 0o077 == 0);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .expect("bounded source read");
    assert!(bytes.len() as u64 <= maximum);
    bytes
}

fn http_status_only(error: &str) -> Option<u16> {
    // Never print the provider body, account fields or the credential. The
    // native HTTP error format includes the status before the bounded body.
    error
        .strip_prefix("model request failed: ")
        .unwrap_or(error)
        .strip_prefix("HTTP ")
        .and_then(|suffix| suffix.get(..3))
        .filter(|status| status.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|status| status.parse().ok())
        .filter(|status| (100..=599).contains(status))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureCategory {
    Transport,
    Timeout,
    Cancellation,
    AuthConnection,
    RequestValidation,
    ProtocolSse,
    HttpKnownStatus,
    Unknown,
}

fn failure_category(error: Option<&str>, cancelled: bool) -> FailureCategory {
    if cancelled || error.is_some_and(|error| error == "agent run was cancelled") {
        return FailureCategory::Cancellation;
    }
    let Some(error) = error else {
        return FailureCategory::Unknown;
    };
    // Diagnostic classification only: never an authorization or retry rule.
    // Inspect known formatter prefixes instead of copying any arbitrary text.
    if http_status_only(error).is_some() {
        return FailureCategory::HttpKnownStatus;
    }
    if error.starts_with("invalid run request:") || error.starts_with("agent limit reached:") {
        return FailureCategory::RequestValidation;
    }
    let model_error = error
        .strip_prefix("model request failed: ")
        .unwrap_or(error);
    if model_error.starts_with("invalid Responses ") || model_error.starts_with("Responses ") {
        return FailureCategory::ProtocolSse;
    }
    if model_error.contains("timed out") || model_error.contains("timeout") {
        return FailureCategory::Timeout;
    }
    if model_error.starts_with("error sending request")
        || model_error.starts_with("error decoding response body")
        || model_error.starts_with("request or response body error")
    {
        return FailureCategory::Transport;
    }
    if [
        "No se pudo renovar la conexión",
        "No se pudo leer la conexión guardada",
        "Conexión guardada inválida",
        "La conexión de ChatGPT cambió",
        "La sesión de ChatGPT venció",
        "Vuelve a conectar tu cuenta de ChatGPT",
        "Conecta tu cuenta de ChatGPT",
        "Vuelve a iniciar sesión con ChatGPT",
    ]
    .iter()
    .any(|prefix| model_error.starts_with(prefix))
    {
        return FailureCategory::AuthConnection;
    }
    FailureCategory::Unknown
}

#[test]
fn live_failure_diagnostic_has_closed_categories_without_copying_provider_text() {
    for (error, expected) in [
        ("model request failed: error sending request for url (https://fixture.invalid/responses)", FailureCategory::Transport),
        ("model request failed: operation timed out", FailureCategory::Timeout),
        ("agent run was cancelled", FailureCategory::Cancellation),
        ("model request failed: La conexión de ChatGPT cambió durante el turno", FailureCategory::AuthConnection),
        ("invalid run request: fixture request", FailureCategory::RequestValidation),
        ("model request failed: invalid Responses SSE JSON: fixture body", FailureCategory::ProtocolSse),
        ("model request failed: HTTP 403 Forbidden: fixture-body-must-not-be-copied", FailureCategory::HttpKnownStatus),
        ("model request failed: fixture-body-must-not-be-copied", FailureCategory::Unknown),
    ] {
        let category = failure_category(Some(error), false);
        assert!(category == expected);
        let serialized = serde_json::to_string(&category).unwrap();
        assert!(!serialized.contains("fixture") && !serialized.contains("https://"));
    }
    assert!(failure_category(None, true) == FailureCategory::Cancellation);
    assert!(failure_category(None, false) == FailureCategory::Unknown);
    assert!(http_status_only("model request failed: Responses stream failed: HTTP 403").is_none());
}

#[test]
#[ignore = "requires explicit OPENIDE_CODEX_LIVE_ACCEPTANCE=1 and a fresh saved private Codex connection; makes one native model call"]
fn saved_codex_plan_uses_exact_production_factory_and_reopens_durable_native_history() {
    assert!(
        std::env::var("OPENIDE_CODEX_LIVE_ACCEPTANCE").is_ok_and(|value| value == "1"),
        "account acceptance requires its explicit opt-in flag"
    );
    let original_root = data_root();
    let settings_path = original_root.join("native-provider.json");
    let settings_bytes = bounded_read(&settings_path, 32 * 1024, false);
    let settings: NativeProviderSettings =
        serde_json::from_slice(&settings_bytes).expect("explicit saved route must parse");
    assert!(settings.provider_id == "codex");
    assert!(
        !settings.model_id.is_empty(),
        "no fallback model is allowed"
    );

    let credential_path = original_root.join("provider-credentials/codex.json");
    let credential_bytes = bounded_read(&credential_path, 96 * 1024, true);
    let credential: serde_json::Value =
        serde_json::from_slice(&credential_bytes).expect("private record must parse");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        credential["expires_at"]
            .as_u64()
            .is_some_and(|expires| expires >= now.saturating_add(600)),
        "the acceptance requires at least 600 seconds of validity; it must not refresh or rotate credentials"
    );
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
        std::io::Write::write_all(&mut copy, &credential_bytes).unwrap();
        copy.sync_all().unwrap();
    }
    #[cfg(not(unix))]
    fs::write(private_directory.join("codex.json"), &credential_bytes).unwrap();
    settings
        .save(&root)
        .expect("save only the isolated exact route");
    // Additional registry entries cannot override the reserved Codex adapter.
    // Preserve the validated registry file if the host has one; never invent
    // an endpoint to make this acceptance succeed.
    let registry_path = original_root.join("provider-registry.json");
    let registry_bytes = registry_path
        .exists()
        .then(|| bounded_read(&registry_path, 512 * 1024, false));
    if let Some(bytes) = &registry_bytes {
        fs::write(root.join("provider-registry.json"), bytes).unwrap();
    }
    let original_connection = settings
        .resolve_at(&original_root)
        .expect("saved connection");
    let connection = settings
        .resolve_at(&root)
        .expect("isolated saved connection");
    assert!(connection.model_id == settings.model_id);
    assert!(connection.endpoint == original_connection.endpoint);
    assert!(connection.wire_api == NativeWireApi::Responses);
    let endpoint = connection.endpoint.clone();
    let model = native_model_backend(connection, "codex", None, None, &root, None, &[])
        .expect("the production desktop factory must accept the saved route");
    assert!(model.model_id() == settings.model_id);

    let sessions = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let session = sessions.create(&workspace, "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "live-codex-plan",
            PROMPT,
            &settings.model_id,
            TurnMode::Plan,
        )
        .unwrap();
    let runner = AgentRunner::new(model, Arc::new(LocalTools::new().with_write_access(false)))
        .with_limits(RunLimits {
            max_model_steps: 1,
            max_tool_calls: 0,
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
            mode: TurnMode::Plan,
        },
        lease,
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut cancelled_at_deadline = false;
    loop {
        match handle.events.recv_timeout(Duration::from_millis(100)) {
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !cancelled_at_deadline {
                    handle.cancel();
                    cancelled_at_deadline = true;
                }
                assert!(
                    Instant::now() < deadline + Duration::from_secs(10),
                    "native cancellation acknowledgment exceeded its grace period"
                );
            }
        }
    }
    assert!(
        handle.journal_error().is_none(),
        "native journal publication failed"
    );
    let stored = sessions.load(&session.id).unwrap().unwrap();
    let requests = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ModelRequestStarted { .. }))
        .count();
    let responses = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ModelResponseCommitted { .. }))
        .count();
    let failure_error = stored.events.iter().find_map(|event| match &event.kind {
        AgentEventKind::TurnFailed { error } => Some(error.as_str()),
        _ => None,
    });
    let failure_status = failure_error.and_then(http_status_only);
    let failed_terminals = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::TurnFailed { .. }))
        .count();
    let cancelled_terminals = stored
        .events
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::TurnCancelled))
        .count();
    let completed = stored.events.iter().any(|event| matches!(
        &event.kind, AgentEventKind::TurnCompleted { response, .. } if !response.trim().is_empty()
    ));
    assert!(
        requests == 1,
        "the acceptance permits one logical model request"
    );
    assert!(
        !stored.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ToolStarted { .. } | AgentEventKind::ToolFinished { .. }
        )),
        "the minimal account probe must not execute tools"
    );
    assert!(
        bounded_read(&settings_path, 32 * 1024, false) == settings_bytes,
        "personal settings changed during the acceptance"
    );
    assert!(
        bounded_read(&credential_path, 96 * 1024, true) == credential_bytes,
        "personal credentials changed during the acceptance"
    );
    assert!(
        bounded_read(&private_directory.join("codex.json"), 96 * 1024, true) == credential_bytes,
        "the temporary connection unexpectedly refreshed"
    );
    if let Some(bytes) = registry_bytes {
        assert!(
            bounded_read(&registry_path, 512 * 1024, false) == bytes,
            "personal registry changed during the acceptance"
        );
    }
    drop(handle);
    drop(runner);
    drop(sessions);
    let reopened = SessionStore::open(root.join("sessions")).unwrap();
    let record = reopened.load(&session.id).unwrap().unwrap();
    assert!(
        record.events == stored.events,
        "native journal changed on reopen"
    );
    assert!(record.events.iter().any(|event| matches!(
        &event.kind, AgentEventKind::TurnAdmitted { model, mode: TurnMode::Plan, .. }
            if model == &settings.model_id
    )));
    // This diagnostic deliberately excludes the HTTP body and all account or
    // credential fields. A 401/403 is terminal in the production transport.
    let diagnostic = serde_json::json!({
        "provider":"codex", "model":settings.model_id, "endpoint":endpoint,
        "logical_model_requests":requests, "responses_committed":responses,
        "completed":completed, "http_status":failure_status,
        "failure_category":(!completed).then(|| failure_category(failure_error,cancelled_terminals != 0)),
        "failed_terminals":failed_terminals, "cancelled_terminals":cancelled_terminals,
        "deadline_cancelled":cancelled_at_deadline, "journal_reopened":true,
        "personal_files_unchanged":true,
        "build_tools_compaction_resume_verified":false
    });
    // Keep only this closed, non-secret diagnostic after a failed acceptance.
    // The isolated raw journal and copied credential are still removed by the
    // temporary directory's Drop. The retained file starts private (0600).
    let mut artifact = tempfile::Builder::new()
        .prefix("openide-codex-live-diagnostic-")
        .suffix(".json")
        .tempfile()
        .unwrap();
    serde_json::to_writer_pretty(artifact.as_file_mut(), &diagnostic).unwrap();
    artifact.as_file().sync_all().unwrap();
    let (file, artifact_path) = artifact.keep().unwrap();
    drop(file);
    println!("{diagnostic}");
    println!("private_diagnostic_artifact={}", artifact_path.display());
    assert!(
        usize::from(completed) + failed_terminals + cancelled_terminals == 1,
        "the account probe must contain exactly one durable terminal"
    );
    assert!(completed && responses == 1,
        "live Codex Plan acceptance did not complete; category {:?}, HTTP status {:?}; no Build, tools, compaction or Resume was attempted",
        failure_category(failure_error, cancelled_terminals != 0), failure_status);
}
