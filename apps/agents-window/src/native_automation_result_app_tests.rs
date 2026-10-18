//! Real desktop result reconciliation from native journals after process cuts.

use super::*;
use crate::{
    automation_view::{build_automations, AutomationScreen},
    automations::{AutomationDraft, AutomationRun, AutomationRunStatus, AutomationSchedule},
    native_automation::FrozenNativeAutomationAuthorityV1,
    native_automation_result::{NativeAutomationOutcome, NativeAutomationResultV1},
    native_provider::{DiscoveredModel, ProviderModelDiscovery, RuntimeConnection},
};
use openide_core::{CheckpointBudgetSnapshot, NativeEventJournal, Session, StoreError};
use openide_harness::CheckpointCoordinator;
use openide_ui::{DrawCommand, Size, UiTree};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Stdio},
    sync::{atomic::AtomicBool, Mutex},
};

const PROVIDER: &str = "custom";
const MODEL: &str = "automation-result/exact-model-ñ";
const PROMPT: &str = "Report the read-only result without using external tools.";
const INSTRUCTIONS: &str = "Original result authority ñ 🦀\r\nPreserve this snapshot.\n";
const FIRST: &str = "First persisted result ñ🙂 ";
const LAST: &str = "completed 終わり";
const KEY: &str = "automation-result-local-fixture-key";
const DEADLINE: Duration = Duration::from_secs(12);
const CUT_EXIT: i32 = 86;
const CUT_ROOT: &str = "OPENIDE_AUTOMATION_RESULT_CUT_ROOT";
const CUT_SESSION: &str = "OPENIDE_AUTOMATION_RESULT_CUT_SESSION";
const CUT_PHASE: &str = "OPENIDE_AUTOMATION_RESULT_CUT_PHASE";

#[derive(Clone, Copy)]
enum FirstResponse {
    Complete,
    CloseAfterDelta,
    HoldAfterDelta,
}

struct ResultProvider {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

fn read_request(socket: &mut TcpStream) -> Value {
    socket.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut bytes = Vec::new();
    let (start, length) = loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).unwrap();
        assert!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(headers.starts_with("POST /v1/chat/completions "));
            assert!(headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case(&format!("authorization: Bearer {KEY}"))));
            let length = headers
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
    while bytes.len() < start + length {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).unwrap();
        assert!(count > 0, "request ended before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[start..start + length]).unwrap()
}

impl ResultProvider {
    fn new(first: FirstResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (observed, released, stopping) = (requests.clone(), release.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("result provider accept: {error}"),
                };
                let request = read_request(&mut socket);
                let streamed = request["stream"] == true;
                assert_eq!(request["model"], MODEL);
                let number = {
                    let mut requests = observed.lock().unwrap();
                    requests.push(request);
                    requests.len()
                };
                if !streamed {
                    let summary = json!({"objective":PROMPT,"decisions":["Preserve frozen read-only authority"],"files":[],"current_state":format!("{FIRST}{LAST}"),"constraints":["No writes or commands"],"remaining_work":[]}).to_string();
                    let body = json!({"id":"result-compact","choices":[{"index":0,"message":{"role":"assistant","content":summary},"finish_reason":"stop"}]}).to_string();
                    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    socket.flush().unwrap();
                    continue;
                }
                let prefix = format!(
                    "data: {}\n\n",
                    json!({"id":"result-prefix","choices":[{"index":0,"delta":{"content":FIRST}}]})
                );
                let tail = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"result-tail","choices":[{"index":0,"delta":{"content":LAST}}]}),
                    json!({"id":"result-terminal","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":12,"total_tokens":112}})
                );
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",prefix.len()+tail.len()).unwrap();
                let split = prefix.find('🙂').unwrap() + 1;
                socket.write_all(&prefix.as_bytes()[..split]).unwrap();
                socket.flush().unwrap();
                socket.write_all(&prefix.as_bytes()[split..]).unwrap();
                socket.flush().unwrap();
                if number == 1 {
                    match first {
                        FirstResponse::CloseAfterDelta => continue,
                        FirstResponse::HoldAfterDelta => {
                            let deadline = Instant::now() + DEADLINE;
                            while !released.load(Ordering::Acquire)
                                && !stopping.load(Ordering::Acquire)
                            {
                                assert!(Instant::now() < deadline, "result SSE barrier timed out");
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            if stopping.load(Ordering::Acquire) {
                                return;
                            }
                        }
                        FirstResponse::Complete => {}
                    }
                }
                // Cancellation and process cuts intentionally close this socket.
                let _ = socket.write_all(tail.as_bytes());
                let _ = socket.flush();
            }
        });
        Self {
            endpoint,
            requests,
            release,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ResultProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.release.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    session: Session,
    dispatch: AutomationDispatch,
    frozen: FrozenNativeAutomationAuthorityV1,
}

impl Fixture {
    fn new(provider: &ResultProvider) -> (Self, AgentsWindow) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("data");
        let workspace = directory.path().join("project");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(workspace.join("AGENTS.md"), INSTRUCTIONS).unwrap();
        fs::write(
            root.join("provider-registry.json"),
            serde_json::to_vec(&json!({"version":1,"providers":[]})).unwrap(),
        )
        .unwrap();
        let mut app = AgentsWindow::with_data_root(root.clone());
        app.change_workspace(workspace.to_string_lossy().into_owned());
        let index = app.current_automation_store_index().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        app.automation_stores[index]
            .create(
                AutomationDraft {
                    title: "Native durable result".into(),
                    prompt: PROMPT.into(),
                    backend: "native".into(),
                    native_provider: Some(NativeProviderSettings {
                        provider_id: PROVIDER.into(),
                        model_id: MODEL.into(),
                        custom_endpoint: provider.endpoint.clone(),
                    }),
                    allow_changes: false,
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
                    name: "Exact result model".into(),
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
                endpoint: provider.endpoint.clone(),
                api_key: None,
                model_id: MODEL.into(),
                wire_api: NativeWireApi::ChatCompletions,
            },
            PROVIDER,
            &root,
            &workspace,
            None,
            &discoveries,
            None,
            None,
            INSTRUCTIONS.into(),
            PROMPT.into(),
            &dispatch.run_id,
            false,
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
        store
            .admit_native_turn(
                &session.id,
                frozen.first_turn_id(),
                PROMPT,
                MODEL,
                TurnMode::Build,
            )
            .unwrap();
        app.automation_stores[index]
            .attach_existing_native_session(&dispatch.run_id, &session, now + 2)
            .unwrap();
        app.automation_stores[index]
            .complete_run(&dispatch.run_id, Ok(session.id.clone()), now + 3)
            .unwrap();
        app.refresh_automation_view();
        (
            Self {
                _directory: directory,
                root,
                session,
                dispatch,
                frozen,
            },
            app,
        )
    }

    fn store(&self) -> Arc<SessionStore> {
        Arc::new(SessionStore::open(self.root.join("sessions")).unwrap())
    }
    fn journal_path(&self) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{}.jsonl", self.session.id))
    }
    fn journal(&self) -> Vec<u8> {
        fs::read(self.journal_path()).unwrap()
    }

    fn run(&self, app: &AgentsWindow) -> AutomationRun {
        app.automation_stores
            .iter()
            .filter(|store| store.workspace_path() == self.session.workspace)
            .flat_map(|store| store.runs())
            .find(|run| run.run_id == self.dispatch.run_id)
            .unwrap()
            .clone()
    }

    fn launch(&self, app: &mut AgentsWindow) {
        let store = app.store.clone().unwrap();
        let lease = store.try_acquire_native_lease(&self.session.id).unwrap();
        let admission = store
            .admit_native_turn(
                &self.session.id,
                self.frozen.first_turn_id(),
                PROMPT,
                MODEL,
                TurnMode::Build,
            )
            .unwrap();
        let run = app
            .start_frozen_automation_runner(
                &self.session,
                &self.frozen,
                self.frozen
                    .fixture_model_with_key(Some(KEY.into()))
                    .unwrap(),
                admission.sequence,
                lease,
            )
            .unwrap();
        app.background_automation_runs
            .push(BackgroundAutomationRun {
                automation_id: self.dispatch.automation_id.clone(),
                session_id: self.session.id.clone(),
                run,
            });
    }

    fn resume_request(&self) -> ResumeWorkerRequest {
        ResumeWorkerRequest {
            session_id: self.session.id.clone(),
            native_automation: Some(self.frozen.clone()),
            subagent_settings: Default::default(),
            provider: PROVIDER.into(),
            endpoint: self.frozen.endpoint().into(),
            sessions: self.store(),
            checkpoints: Arc::new(CheckpointFileStore::new(self.root.join("sessions")).unwrap()),
            compatibility: self.frozen.compatibility(),
            model: self
                .frozen
                .fixture_model_with_key(Some(KEY.into()))
                .unwrap(),
            base_tools: self.frozen.tools(),
            child_tools: self.frozen.tools(),
            child_command_profile: None,
            event_bridge: None,
            tool_preparation: None,
            permission_policy: self.frozen.policy(),
            command_isolation: CommandIsolation::HostUnrestricted,
            command_hooks: Arc::new(CommandHooks::default()),
            budget_policy: self.frozen.budget(),
            child_identity_budget: None,
            system_instructions: INSTRUCTIONS.into(),
            pre_context_instructions: None,
            limits: self.frozen.limits(),
        }
    }

    fn corrupt_settings(&self) -> Vec<(PathBuf, Vec<u8>)> {
        [
            "native-provider.json",
            "provider-registry.json",
            "agent-tool-settings.json",
            "native-command-hooks.json",
            "mcp-servers.json",
        ]
        .into_iter()
        .map(|name| {
            let path = self.root.join(name);
            let bytes = b"{MUTATED invalid settings \xff".to_vec();
            fs::write(&path, &bytes).unwrap();
            (path, bytes)
        })
        .chain(std::iter::once_with(|| {
            let path = self.session.workspace.join("AGENTS.md");
            let bytes = vec![0xff, 0xfe];
            fs::write(&path, &bytes).unwrap();
            (path, bytes)
        }))
        .collect()
    }
}

#[test]
fn shared_run_ids_keep_result_diagnostics_and_proofs_scoped_to_their_workspace() {
    let provider = ResultProvider::new(FirstResponse::Complete);
    let (fixture, mut app) = Fixture::new(&provider);
    fixture.launch(&mut app);
    finish(&mut app);
    let result_a = assert_result(&fixture, &app, NativeAutomationOutcome::Completed);
    let workspace_b = fixture
        .session
        .workspace
        .parent()
        .unwrap()
        .join("project-b");
    fs::create_dir(&workspace_b).unwrap();
    fs::write(workspace_b.join("AGENTS.md"), INSTRUCTIONS).unwrap();
    app.change_workspace(workspace_b.to_string_lossy().into_owned());
    let index_b = app.current_automation_store_index().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    app.automation_stores[index_b]
        .create(
            AutomationDraft {
                title: "Temporary B definition".into(),
                prompt: PROMPT.into(),
                backend: "native".into(),
                native_provider: Some(NativeProviderSettings {
                    provider_id: PROVIDER.into(),
                    model_id: MODEL.into(),
                    custom_endpoint: provider.endpoint.clone(),
                }),
                allow_changes: false,
                schedule: AutomationSchedule::Once { at_ms: now + 1 },
            },
            now,
        )
        .unwrap();
    let documents = fs::read_dir(fixture.root.join("automations"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("json")).then(|| {
                let document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                (path, document)
            })
        })
        .collect::<Vec<_>>();
    let original = documents
        .iter()
        .find(|(_, document)| {
            document["workspace_key"] == fixture.session.workspace.to_string_lossy().as_ref()
        })
        .unwrap()
        .1
        .clone();
    let (path_b, mut document_b) = documents
        .into_iter()
        .find(|(_, document)| {
            document["workspace_key"]
                == workspace_b
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
        })
        .unwrap();
    document_b["automations"] = original["automations"].clone();
    document_b["runs"] = original["runs"].clone();
    document_b["runs"][0]["session_id"] = Value::Null;
    document_b["runs"][0]
        .as_object_mut()
        .unwrap()
        .remove("native_result");
    // The file keeps B's own canonical workspace key and existing FNV filename.
    // Only the definition/claim are copied, matching the cross-project case.
    drop(app);
    fs::write(&path_b, serde_json::to_vec_pretty(&document_b).unwrap()).unwrap();
    let discovery = vec![ProviderModelDiscovery {
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
                name: "Exact B scope".into(),
                provider_reports_tools: true,
                tool_call_declared: Some(true),
                text_input_output: true,
                supported_efforts: vec![],
                context_window_tokens: Some(32_000),
                max_output_tokens: Some(4096),
            },
        )]),
    }];
    let frozen_b = FrozenNativeAutomationAuthorityV1::freeze(
        RuntimeConnection {
            auth: Default::default(),
            headers: Default::default(),
            endpoint: provider.endpoint.clone(),
            api_key: None,
            model_id: MODEL.into(),
            wire_api: NativeWireApi::ChatCompletions,
        },
        PROVIDER,
        &fixture.root,
        &workspace_b,
        None,
        &discovery,
        None,
        None,
        INSTRUCTIONS.into(),
        PROMPT.into(),
        &fixture.dispatch.run_id,
        false,
    )
    .unwrap();
    let store = fixture.store();
    let session_b = store
        .create_native_automation(
            &workspace_b,
            frozen_b
                .to_origin(
                    &fixture.dispatch.automation_id,
                    &fixture.dispatch.run_id,
                    fixture.dispatch.scheduled_at,
                    fixture.dispatch.manual,
                )
                .unwrap(),
        )
        .unwrap();
    assert_ne!(session_b.id, fixture.session.id);
    assert_eq!(
        session_b.native_automation.as_ref().unwrap().run_id,
        fixture.dispatch.run_id
    );
    assert_eq!(
        session_b.native_automation.as_ref().unwrap().automation_id,
        fixture.dispatch.automation_id
    );
    let lease = store.try_acquire_native_lease(&session_b.id).unwrap();
    store
        .admit_native_turn(
            &session_b.id,
            frozen_b.first_turn_id(),
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    // A valid pre-model failure is sufficient for the scope regression. This
    // journal is deliberately not presented as an HTTP execution acceptance.
    store
        .append_native_event(&AgentEvent::new(
            &session_b.id,
            2,
            AgentEventKind::TurnStarted {
                prompt: PROMPT.into(),
            },
        ))
        .unwrap();
    store
        .append_native_event(&AgentEvent::new(
            &session_b.id,
            3,
            AgentEventKind::TurnFailed {
                error: "B scope fixture stopped before any model request".into(),
            },
        ))
        .unwrap();
    drop(lease);
    fs::write(fixture.journal_path(), b"{A journal no longer verifiable\n").unwrap();
    let mut reopened = AgentsWindow::with_data_root(fixture.root.clone());
    reopened.reconcile_native_automation_session(&fixture.session.id);
    let key_a = (
        fixture.session.workspace.clone(),
        fixture.dispatch.run_id.clone(),
    );
    let diagnostic_a = reopened
        .native_automation_result_errors
        .get(&key_a)
        .unwrap()
        .clone();
    assert_eq!(
        fixture.run(&reopened).native_result.as_ref(),
        Some(&result_a)
    );
    reopened.change_workspace(workspace_b.to_string_lossy().into_owned());
    reopened.refresh_automation_view();
    assert!(
        reopened.view.automations.native_result_errors.is_empty(),
        "A diagnostic appeared in B"
    );
    let result_b = reopened
        .view
        .automations
        .runs
        .iter()
        .find(|run| run.run_id == fixture.dispatch.run_id)
        .unwrap()
        .native_result
        .clone()
        .unwrap();
    assert_eq!(result_b.outcome(), NativeAutomationOutcome::Failed);
    assert_eq!(result_b.session_id(), session_b.id);
    assert_eq!(result_b.workspace(), session_b.workspace);
    let lease = store.try_acquire_native_lease(&session_b.id).unwrap();
    reopened.reconcile_native_automation_session(&session_b.id);
    assert_eq!(
        reopened.native_automation_result_errors.get(&key_a),
        Some(&diagnostic_a)
    );
    drop(lease);
    reopened.reconcile_native_automation_session(&session_b.id);
    assert_eq!(
        reopened.native_automation_result_errors.get(&key_a),
        Some(&diagnostic_a),
        "B success cleared A diagnostic"
    );
    reopened.change_workspace(fixture.session.workspace.to_string_lossy().into_owned());
    reopened.refresh_automation_view();
    assert_eq!(
        reopened.view.automations.native_result_errors,
        vec![diagnostic_a]
    );
    assert_eq!(
        fixture.run(&reopened).native_result.as_ref(),
        Some(&result_a)
    );
    assert!(reopened.view.automations.runs.iter().all(|run| run
        .native_result
        .as_ref()
        .is_none_or(|result| result.workspace() == fixture.session.workspace)));
    assert_eq!(
        provider.requests().len(),
        1,
        "scope observation made a new model request"
    );
    assert_eq!(
        fs::read(fixture.journal_path()).unwrap(),
        b"{A journal no longer verifiable\n"
    );
}

fn drive_until(app: &mut AgentsWindow, mut ready: impl FnMut(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        app.poll_run();
        app.poll_resume();
        app.poll_automations();
        if ready(app) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "desktop result timed out: {} / {:?}",
            app.view.status,
            app.view.automations.native_result_errors
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn finish(app: &mut AgentsWindow) {
    drive_until(app, |app| {
        app.background_automation_runs.is_empty()
            && app.active_resume.is_none()
            && app.active_run.is_none()
    });
    drive_until(app, |app| {
        app.automation_stores
            .iter()
            .flat_map(|store| store.runs())
            .filter_map(|run| run.session_id.as_deref())
            .all(|id| {
                app.store
                    .as_ref()
                    .unwrap()
                    .try_acquire_native_lease(id)
                    .is_ok()
            })
    });
    app.reconcile_native_automation_results();
}

fn assert_result(
    fixture: &Fixture,
    app: &AgentsWindow,
    outcome: NativeAutomationOutcome,
) -> NativeAutomationResultV1 {
    let run = fixture.run(app);
    assert_eq!(
        run.status,
        AutomationRunStatus::Dispatched,
        "result replaced the dispatch status"
    );
    let result = run
        .native_result
        .expect("desktop did not record a native result");
    assert_eq!(result.outcome(), outcome);
    let store = fixture.store();
    let lease = store.try_acquire_native_lease(&fixture.session.id).unwrap();
    let record = store.load_strict(&fixture.session.id).unwrap().unwrap();
    let checkpoints = CheckpointFileStore::new(fixture.root.join("sessions")).unwrap();
    assert_eq!(
        result,
        NativeAutomationResultV1::derive(&record, &checkpoints)
            .unwrap()
            .snapshot()
            .clone()
    );
    drop(lease);
    assert_eq!(
        result.last_sequence(),
        record.events.last().unwrap().sequence
    );
    assert_eq!(
        result.terminal_sequence(),
        record
            .events
            .iter()
            .find(|event| matches!(
                event.kind,
                AgentEventKind::TurnCompleted { .. }
                    | AgentEventKind::TurnFailed { .. }
                    | AgentEventKind::TurnCancelled
            ))
            .map(|event| event.sequence)
    );
    assert_eq!(
        record
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnAdmitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        record.session.native_automation,
        fixture.session.native_automation
    );
    assert!(!record
        .session
        .native_automation
        .unwrap()
        .authority_snapshot_json
        .contains(KEY));
    result
}

#[test]
fn real_completed_result_survives_reopen_with_corrupt_current_settings() {
    let provider = ResultProvider::new(FirstResponse::Complete);
    let (fixture, mut app) = Fixture::new(&provider);
    fixture.launch(&mut app);
    finish(&mut app);
    let result = assert_result(&fixture, &app, NativeAutomationOutcome::Completed);
    assert!(!result.recovery_required());
    assert_eq!(provider.requests().len(), 1);
    let before = fixture.journal();
    let mutated = fixture.corrupt_settings();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(fixture.root.clone());
    reopened.reconcile_native_automation_results();
    assert_eq!(
        assert_result(&fixture, &reopened, NativeAutomationOutcome::Completed),
        result
    );
    assert_eq!(fixture.journal(), before);
    assert_eq!(provider.requests().len(), 1);
    assert!(reopened.background_automation_runs.is_empty());
    assert!(reopened.active_run.is_none() && reopened.active_resume.is_none());
    for (path, bytes) in mutated {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

struct CutJournal {
    store: Arc<SessionStore>,
    root: PathBuf,
    phase: String,
}

impl NativeEventJournal for CutJournal {
    fn append_native_event(&self, event: &AgentEvent) -> Result<(), StoreError> {
        self.store.append_native_event(event)?;
        if self.phase == "delta" && matches!(event.kind, AgentEventKind::AssistantDelta { .. }) {
            fs::write(
                self.root.join("result-cut-paused"),
                event.sequence.to_string(),
            )?;
            let deadline = Instant::now() + DEADLINE;
            while !self.root.join("result-cut-release").exists() {
                assert!(
                    Instant::now() < deadline,
                    "parent never released the real process cut"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            std::process::exit(CUT_EXIT);
        }
        if self.phase == "completed" && matches!(event.kind, AgentEventKind::TurnCompleted { .. }) {
            std::process::exit(CUT_EXIT);
        }
        Ok(())
    }
}

#[test]
#[ignore = "subprocess fixture: exits without destructors before the host result ACK"]
fn native_result_process_cut_child() {
    let Ok(root) = std::env::var(CUT_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let session_id = std::env::var(CUT_SESSION).unwrap();
    let phase = std::env::var(CUT_PHASE).unwrap();
    let store = Arc::new(SessionStore::open(root.join("sessions")).unwrap());
    let lease = store.try_acquire_native_lease(&session_id).unwrap();
    let record = store.load(&session_id).unwrap().unwrap();
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
    let admission = store
        .admit_native_turn(
            &session_id,
            frozen.first_turn_id(),
            frozen.prompt(),
            frozen.model_id(),
            TurnMode::Build,
        )
        .unwrap();
    assert_eq!(record.events, vec![admission.clone()]);
    let runner = AgentRunner::new(
        frozen.fixture_model_with_key(Some(KEY.into())).unwrap(),
        frozen.tools(),
    )
    .with_limits(frozen.limits())
    .with_permission_policy(frozen.policy())
    .with_system_instructions(frozen.instructions())
    .with_command_hooks(Arc::new(CommandHooks::default()))
    .with_native_runtime_identity(frozen.provider_id(), frozen.endpoint())
    .with_event_journal(Arc::new(CutJournal {
        store: store.clone(),
        root,
        phase: phase.clone(),
    }));
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
    while let Ok(event) = handle.events.recv_timeout(DEADLINE) {
        assert!(
            !matches!(event.kind, AgentEventKind::TurnFailed { .. }),
            "{phase}: {:?}",
            event.kind
        );
    }
    panic!("child did not reach {phase}");
}

fn child(fixture: &Fixture, phase: &str) -> Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::native_automation_result_app_tests::native_result_process_cut_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CUT_ROOT, &fixture.root)
        .env(CUT_SESSION, &fixture.session.id)
        .env(CUT_PHASE, phase)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn assert_cut(child: Child) {
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(CUT_EXIT),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn live_lease_is_not_interrupted_and_real_process_cut_recovers_to_completed_result() {
    let provider = ResultProvider::new(FirstResponse::Complete);
    let (fixture, mut app) = Fixture::new(&provider);
    let child = child(&fixture, "delta");
    let deadline = Instant::now() + DEADLINE;
    while !fixture.root.join("result-cut-paused").exists() {
        assert!(Instant::now() < deadline, "child never persisted its delta");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(provider.requests().len(), 1);
    assert!(matches!(
        fixture
            .store()
            .try_acquire_native_lease(&fixture.session.id),
        Err(StoreError::NativeSessionBusy(_))
    ));
    let before = fixture.journal();
    let prior_diagnostic =
        "Prior observation remains unverified while the worker owns its lease".to_owned();
    app.native_automation_result_errors.insert(
        (
            fixture.session.workspace.clone(),
            fixture.dispatch.run_id.clone(),
        ),
        prior_diagnostic.clone(),
    );
    app.reconcile_native_automation_results();
    assert!(
        fixture.run(&app).native_result.is_none(),
        "live worker was classified Interrupted"
    );
    assert_eq!(
        app.native_automation_result_errors.get(&(
            fixture.session.workspace.clone(),
            fixture.dispatch.run_id.clone()
        )),
        Some(&prior_diagnostic)
    );
    assert_eq!(fixture.journal(), before);
    fs::write(fixture.root.join("result-cut-release"), b"cut").unwrap();
    assert_cut(child);
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(fixture.root.clone());
    reopened.reconcile_native_automation_results();
    let interrupted = assert_result(&fixture, &reopened, NativeAutomationOutcome::Interrupted);
    assert!(interrupted.recovery_required());
    assert_eq!(interrupted.terminal_sequence(), None);
    assert_eq!(fixture.journal(), before);
    assert_eq!(provider.requests().len(), 1);
    reopened.open_session(&fixture.session.id);
    reopened.active_resume = Some(ResumeWorkerHandle::start(fixture.resume_request()).unwrap());
    reopened.view.running = true;
    finish(&mut reopened);
    let completed = assert_result(&fixture, &reopened, NativeAutomationOutcome::Completed);
    assert!(!completed.recovery_required());
    assert!(completed.last_sequence() > interrupted.last_sequence());
    assert_eq!(provider.requests().len(), 2);
    assert!(fixture.journal().starts_with(&before));
    let record = fixture.store().load(&fixture.session.id).unwrap().unwrap();
    assert_eq!(
        record
            .events
            .iter()
            .filter(|event| matches!(event.kind, AgentEventKind::TurnCompleted { .. }))
            .count(),
        1
    );
    for request in provider.requests() {
        assert!(request["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(INSTRUCTIONS));
    }
    drop(reopened);
    let mut twice = AgentsWindow::with_data_root(fixture.root.clone());
    twice.reconcile_native_automation_results();
    assert_eq!(
        assert_result(&fixture, &twice, NativeAutomationOutcome::Completed),
        completed
    );
    assert_eq!(provider.requests().len(), 2);
}

#[test]
fn completed_journal_cut_before_host_ack_is_reconciled_on_reopen() {
    let provider = ResultProvider::new(FirstResponse::Complete);
    let (fixture, app) = Fixture::new(&provider);
    assert_cut(child(&fixture, "completed"));
    assert!(
        fixture.run(&app).native_result.is_none(),
        "subprocess unexpectedly wrote the host ledger"
    );
    let before = fixture.journal();
    drop(app);
    let mut reopened = AgentsWindow::with_data_root(fixture.root.clone());
    reopened.reconcile_native_automation_results();
    let result = assert_result(&fixture, &reopened, NativeAutomationOutcome::Completed);
    assert!(!result.recovery_required());
    assert_eq!(fixture.journal(), before);
    assert_eq!(provider.requests().len(), 1);
    assert!(!reopened.reconcile_native_automation_session(&fixture.session.id));
    assert_eq!(fixture.journal(), before);
    assert_eq!(provider.requests().len(), 1);
}

#[test]
fn failed_and_cancelled_streams_record_exact_terminal_results_without_restarting_on_reopen() {
    for (response, outcome) in [
        (
            FirstResponse::CloseAfterDelta,
            NativeAutomationOutcome::Failed,
        ),
        (
            FirstResponse::HoldAfterDelta,
            NativeAutomationOutcome::Cancelled,
        ),
    ] {
        let provider = ResultProvider::new(response);
        let (fixture, mut app) = Fixture::new(&provider);
        fixture.launch(&mut app);
        drive_until(&mut app, |app| {
            app.store
                .as_ref()
                .unwrap()
                .load(&fixture.session.id)
                .unwrap()
                .unwrap()
                .events
                .iter()
                .any(|event| matches!(event.kind, AgentEventKind::AssistantDelta { .. }))
        });
        if outcome == NativeAutomationOutcome::Cancelled {
            assert!(!app.background_automation_runs.is_empty());
            app.background_automation_runs[0].run.cancel();
        }
        finish(&mut app);
        provider.release.store(true, Ordering::Release);
        let result = assert_result(&fixture, &app, outcome);
        assert!(!result.recovery_required());
        assert!(result.terminal_sequence().is_some());
        assert_eq!(provider.requests().len(), 1);
        let before = fixture.journal();
        let record = fixture.store().load(&fixture.session.id).unwrap().unwrap();
        assert!(!record.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::ModelResponseCommitted { .. } | AgentEventKind::TurnCompleted { .. }
        )));
        assert_eq!(
            record
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    AgentEventKind::TurnFailed { .. } | AgentEventKind::TurnCancelled
                ))
                .count(),
            1
        );
        drop(app);
        let mut reopened = AgentsWindow::with_data_root(fixture.root.clone());
        reopened.reconcile_native_automation_results();
        assert_eq!(assert_result(&fixture, &reopened, outcome), result);
        assert_eq!(fixture.journal(), before);
        assert_eq!(provider.requests().len(), 1);
        assert!(
            reopened.active_run.is_none()
                && reopened.active_resume.is_none()
                && reopened.background_automation_runs.is_empty()
        );
    }
}

fn assert_diagnostic_ui(app: &mut AgentsWindow, fixture: &Fixture) {
    app.change_workspace(fixture.session.workspace.to_string_lossy().into_owned());
    app.refresh_automation_view();
    app.view.automations.screen = AutomationScreen::History;
    app.view.automations.selected_id = Some(fixture.dispatch.automation_id.clone());
    assert!(!app.view.automations.native_result_errors.is_empty());
    for width in [360.0, 900.0] {
        let mut tree = UiTree::new(build_automations(&app.view.automations, width));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 900.0));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            text.contains("Resultado nativo no verificable"),
            "{width}: {text}"
        );
    }
}

#[test]
fn corrupt_or_missing_journal_and_checkpoint_preserve_the_last_verified_result_and_show_diagnostics(
) {
    let provider = ResultProvider::new(FirstResponse::Complete);
    let (fixture, mut app) = Fixture::new(&provider);
    fixture.launch(&mut app);
    finish(&mut app);
    let original = assert_result(&fixture, &app, NativeAutomationOutcome::Completed);
    let original_bytes = fixture.journal();
    for missing in [false, true] {
        if missing {
            fs::remove_file(fixture.journal_path()).unwrap();
        } else {
            fs::write(fixture.journal_path(), b"{adulterated journal\n").unwrap();
        }
        app.reconcile_native_automation_session(&fixture.session.id);
        assert_eq!(fixture.run(&app).native_result.as_ref(), Some(&original));
        assert_diagnostic_ui(&mut app, &fixture);
        if missing {
            assert!(!fixture.journal_path().exists());
        } else {
            assert_eq!(fixture.journal(), b"{adulterated journal\n");
        }
        assert_eq!(provider.requests().len(), 1);
        fs::write(fixture.journal_path(), &original_bytes).unwrap();
        app.reconcile_native_automation_session(&fixture.session.id);
    }
    // Publish a real semantic checkpoint through the coordinator/provider, then
    // prove its missing or corrupted bytes cannot downgrade cached completion.
    let store = fixture.store();
    let checkpoints = Arc::new(CheckpointFileStore::new(fixture.root.join("sessions")).unwrap());
    let lease = store.try_acquire_native_lease(&fixture.session.id).unwrap();
    let coordinator = CheckpointCoordinator::new(
        store.clone(),
        checkpoints.clone(),
        fixture
            .frozen
            .fixture_model_with_key(Some(KEY.into()))
            .unwrap(),
        fixture.frozen.compatibility(),
        CheckpointBudgetSnapshot::unverified(100, None),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let committed = runtime
        .block_on(coordinator.compact(&lease, CancellationToken::new()))
        .unwrap();
    drop(lease);
    drop(runtime);
    app.reconcile_native_automation_session(&fixture.session.id);
    let with_checkpoint = assert_result(&fixture, &app, NativeAutomationOutcome::Completed);
    assert!(with_checkpoint.last_sequence() > original.last_sequence());
    let path = fixture
        .root
        .join("sessions/checkpoints")
        .join(&fixture.session.id)
        .join(format!("{}.json", committed.reference.checkpoint_id));
    let checkpoint_bytes = fs::read(&path).unwrap();
    let journal_bytes = fixture.journal();
    assert_eq!(provider.requests().len(), 2);
    for missing in [true, false] {
        if missing {
            fs::remove_file(&path).unwrap();
        } else {
            fs::write(&path, b"{adulterated checkpoint\n").unwrap();
        }
        app.reconcile_native_automation_session(&fixture.session.id);
        assert_eq!(
            fixture.run(&app).native_result.as_ref(),
            Some(&with_checkpoint)
        );
        assert_diagnostic_ui(&mut app, &fixture);
        assert_eq!(fixture.journal(), journal_bytes);
        assert_eq!(provider.requests().len(), 2);
        if missing {
            assert!(!path.exists());
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"{adulterated checkpoint\n");
        }
        fs::write(&path, &checkpoint_bytes).unwrap();
        app.reconcile_native_automation_session(&fixture.session.id);
    }
}
