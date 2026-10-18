use super::*;
use crate::native_provider::{DiscoveredModel, ProviderRegistry};
use openide_core::{AgentMessage, SessionStore};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "automation-exact-model-ñ";
const PROVIDER: &str = "automation-fixture";
const ENVIRONMENT: &str = "AUTOMATION_FROZEN_FIXTURE_KEY";
const SECRET: &str = "not-written-to-automation-snapshot-credential";
const INSTRUCTIONS: &str = "  Trusted project ñ 🦀\r\nKeep bytes intact.  \n";
const PROMPT: &str = "  Inspect the project.\nNever run a command.  ";

fn occurrence() -> (String, String, i64) {
    let automation_id = uuid::Uuid::new_v4().to_string();
    let scheduled_at = 123_456;
    let run_id = format!("{automation_id}:{scheduled_at}");
    (automation_id, run_id, scheduled_at)
}

fn budget() -> TokenBudgetPolicy {
    TokenBudgetPolicy {
        input_limit_tokens: None,
        context_window_tokens: 12_000,
        output_reserve_tokens: 1_024,
        tool_schema_reserve_tokens: 2_048,
        auto_compact_token_limit: Some(7_000),
    }
}

fn configure(root: &Path, endpoint: &str, wire: NativeWireApi) -> Vec<ProviderModelDiscovery> {
    let mut adapter = ProviderRegistry::builtin()
        .adapter("custom")
        .unwrap()
        .clone();
    adapter.id = PROVIDER.into();
    adapter.name = "Automation fixture".into();
    adapter.endpoint = endpoint.into();
    adapter.key_env = ENVIRONMENT.into();
    adapter.wire_api = wire;
    adapter.key_optional = false;
    adapter.user_endpoint = false;
    adapter.reasoning_effort_field =
        (wire == NativeWireApi::ChatCompletions).then(|| "reasoning_effort".into());
    adapter.chat_request_fields = BTreeMap::from([("thinking".into(), json!({"type":"disabled"}))]);
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("provider-registry.json"),
        serde_json::to_vec(&json!({"version":1,"providers":[adapter]})).unwrap(),
    )
    .unwrap();
    native_provider::save_output_limit(
        root,
        PROVIDER,
        MODEL,
        Some(NativeOutputLimit {
            provider_id: PROVIDER.into(),
            model_id: MODEL.into(),
            wire_api: wire,
            field: if wire == NativeWireApi::Responses {
                "max_output_tokens"
            } else {
                "max_completion_tokens"
            }
            .into(),
            tokens: 512,
        }),
    )
    .unwrap();
    vec![ProviderModelDiscovery {
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
                name: "Exact fixture".into(),
                provider_reports_tools: true,
                tool_call_declared: Some(true),
                text_input_output: true,
                supported_efforts: vec!["gentle".into(), "intense".into()],
                context_window_tokens: Some(12_000),
                max_output_tokens: Some(1_024),
            },
        )]),
    }]
}

fn freeze(
    root: &Path,
    workspace: &Path,
    endpoint: &str,
    wire: NativeWireApi,
    run_id: &str,
) -> FrozenNativeAutomationAuthorityV1 {
    let discoveries = configure(root, endpoint, wire);
    FrozenNativeAutomationAuthorityV1::freeze(
        RuntimeConnection {
            auth: Default::default(),
            headers: Default::default(),
            endpoint: endpoint.into(),
            api_key: Some(SECRET.into()),
            model_id: MODEL.into(),
            wire_api: wire,
        },
        PROVIDER,
        root,
        workspace,
        None,
        &discoveries,
        Some(HostedWebSearchMode::Cached),
        Some(budget()),
        INSTRUCTIONS.into(),
        PROMPT.into(),
        run_id,
        false,
    )
    .unwrap()
}

#[derive(Clone, Debug)]
struct ObservedRequest {
    request_line: String,
    authorization: Option<String>,
    body: Value,
}

struct HttpFixture {
    endpoint: String,
    observations: Arc<Mutex<Vec<ObservedRequest>>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HttpFixture {
    fn new(wire: NativeWireApi) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let observations = Arc::new(Mutex::new(Vec::new()));
        let recorded = observations.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("automation HTTP accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                recorded.lock().unwrap().push(read_request(&mut stream));
                let response = if wire == NativeWireApi::Responses {
                    json!({"id":"resp_frozen", "status":"completed", "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"frozen response"}]}]})
                } else {
                    json!({"id":"chat_frozen","choices":[{"index":0,"message":{"role":"assistant","content":"frozen response"},"finish_reason":"stop"}]})
                };
                let body = serde_json::to_string(&response).unwrap();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                stream.flush().unwrap();
            }
        });
        Self {
            endpoint,
            observations,
            stopped,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<ObservedRequest> {
        self.observations.lock().unwrap().clone()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn read_request(stream: &mut TcpStream) -> ObservedRequest {
    let mut bytes = Vec::new();
    let (header_end, length) = loop {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "automation request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
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
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    ObservedRequest {
        request_line: headers.lines().next().unwrap().into(),
        authorization: headers.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim().into())
        }),
        body: serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn complete(snapshot: &FrozenNativeAutomationAuthorityV1) {
    let backend = snapshot
        .model_with_credential_reader(|name| {
            assert_eq!(name, ENVIRONMENT);
            Some(SECRET.into())
        })
        .unwrap();
    snapshot.verify_model(backend.as_ref()).unwrap();
    let messages = [
        AgentMessage::system(snapshot.instructions()),
        AgentMessage::user(snapshot.prompt()),
    ];
    let catalog = snapshot.catalog().unwrap();
    let result = runtime()
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(8),
                backend.complete(&messages, catalog.definitions(), CancellationToken::new()),
            )
            .await
        })
        .unwrap()
        .unwrap();
    assert_eq!(result.content, "frozen response");
}

#[test]
fn readonly_catalog_contains_no_external_effect_tools() {
    let catalog = readonly_catalog().unwrap();
    let names: Vec<_> = catalog
        .definitions()
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"list_files"));
    assert!(names.contains(&"search_files"));
    assert!(names.contains(&"update_plan"));
    assert_eq!(names.len(), 4);
}

#[test]
fn a_workspace_write_profile_offers_edits_and_sandboxed_commands_and_keeps_its_limits() {
    let catalog = profile_catalog(WORKSPACE_WRITE_V1).unwrap();
    let names: Vec<_> = catalog.definitions().iter().map(|definition| definition.name.as_str()).collect();
    for tool in ["read_file", "write_file", "apply_patch", "run_command", "update_plan"] {
        assert!(names.contains(&tool), "{tool} in {names:?}");
    }
    let run_command = catalog.definitions().iter().find(|definition| definition.name == "run_command").unwrap();
    assert!(run_command.parameters["properties"].get("elevated").is_none(), "never elevated");
    assert_ne!(catalog.digest_hex(), readonly_catalog().unwrap().digest_hex());
    assert_ne!(FrozenLimits::from(profile_limits(WORKSPACE_WRITE_V1)), FrozenLimits::from(profile_limits(READ_ONLY_V1)));
}

#[test]
fn an_automation_allowed_to_change_freezes_its_profile_or_refuses_without_a_sandbox() {
    let directory = tempfile::tempdir().unwrap();
    let server = HttpFixture::new(NativeWireApi::ChatCompletions);
    let (_, run_id, _) = occurrence();
    let discoveries = configure(directory.path(), &server.endpoint, NativeWireApi::ChatCompletions);
    let frozen = FrozenNativeAutomationAuthorityV1::freeze(
        RuntimeConnection {
            auth: Default::default(),
            headers: Default::default(),
            endpoint: server.endpoint.clone(),
            api_key: Some(SECRET.into()),
            model_id: MODEL.into(),
            wire_api: NativeWireApi::ChatCompletions,
        },
        PROVIDER,
        directory.path(),
        directory.path(),
        None,
        &discoveries,
        None,
        Some(budget()),
        INSTRUCTIONS.into(),
        PROMPT.into(),
        &run_id,
        true,
    );
    if !openide_harness::CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
        assert!(frozen.unwrap_err().contains("aislamiento"));
        return;
    }
    let frozen = frozen.unwrap();
    assert!(frozen.allows_changes());
    assert!(frozen.catalog().unwrap().definitions().iter().any(|definition| definition.name == "write_file"));
    let origin = frozen.to_origin("automation", &run_id, 1, true).unwrap();
    assert_eq!(origin.authority_profile, openide_core::NativeAutomationAuthorityProfile::WorkspaceWriteV1);
    // A snapshot that claims the write profile with the read-only limits is refused.
    let mut tampered: serde_json::Value = serde_json::to_value(&frozen).unwrap();
    tampered["limits"] = serde_json::to_value(FrozenLimits::from(readonly_v1_limits())).unwrap();
    let tampered: FrozenNativeAutomationAuthorityV1 = serde_json::from_value(tampered).unwrap();
    assert!(tampered.validate().is_err());
}

#[test]
fn frozen_chat_reconstructs_real_request_without_current_configuration_or_stored_secret() {
    let directory = tempfile::tempdir().unwrap();
    let server = HttpFixture::new(NativeWireApi::ChatCompletions);
    let (_, run_id, _) = occurrence();
    let snapshot = freeze(
        directory.path(),
        directory.path(),
        &server.endpoint,
        NativeWireApi::ChatCompletions,
        &run_id,
    );
    assert_eq!(snapshot.instructions(), INSTRUCTIONS);
    assert_eq!(snapshot.prompt(), PROMPT);
    assert_eq!(snapshot.budget(), Some(budget()));
    assert_eq!(
        snapshot.capabilities.reasoning_efforts,
        vec!["gentle", "intense"]
    );
    assert_eq!(snapshot.model_output_maximum, Some(1_024));
    assert!(snapshot.hosted_web_search.is_none());
    assert!(snapshot.effort.is_none());
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(!json.contains(SECRET));
    assert!(json.contains(ENVIRONMENT));
    let decoded: FrozenNativeAutomationAuthorityV1 = serde_json::from_str(&json).unwrap();
    assert_eq!(snapshot, decoded);
    // All current provider/settings files now fail parsing. Neither reload nor
    // request reconstruction may consult them or a replacement model catalog.
    for name in [
        "provider-registry.json",
        "native-provider.json",
        "native-output-limits.json",
    ] {
        fs::write(directory.path().join(name), b"{ damaged current settings").unwrap();
    }
    fs::write(
        directory.path().join("AGENTS.md"),
        b"New mutable instructions",
    )
    .unwrap();
    complete(&decoded);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].request_line,
        "POST /v1/chat/completions HTTP/1.1"
    );
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some(format!("Bearer {SECRET}").as_str())
    );
    let body = &requests[0].body;
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["messages"][0]["content"], INSTRUCTIONS);
    assert_eq!(body["messages"][1]["content"], PROMPT);
    assert_eq!(body["thinking"], json!({"type":"disabled"}));
    assert_eq!(body["max_completion_tokens"], 512);
    assert!(body.get("reasoning_effort").is_none());
    let names: Vec<_> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 4);
    assert!(names.contains(&"update_plan"));
    assert!(!names.contains(&"run_command"));
}

#[test]
fn frozen_responses_preserves_hosted_search_cap_and_exact_profile() {
    let directory = tempfile::tempdir().unwrap();
    let server = HttpFixture::new(NativeWireApi::Responses);
    let (_, run_id, _) = occurrence();
    let snapshot = freeze(
        directory.path(),
        directory.path(),
        &server.endpoint,
        NativeWireApi::Responses,
        &run_id,
    );
    assert_eq!(
        snapshot.hosted_web_search.as_ref().unwrap().mode,
        HostedWebSearchMode::Cached
    );
    assert_eq!(snapshot.compatibility().context_format_key, "responses-v1");
    fs::remove_file(directory.path().join("provider-registry.json")).unwrap();
    fs::remove_file(directory.path().join("native-output-limits.json")).unwrap();
    complete(&snapshot);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request_line, "POST /v1/responses HTTP/1.1");
    let body = &requests[0].body;
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["max_output_tokens"], 512);
    assert!(body.get("reasoning").is_none());
    assert!(body.get("thinking").is_none());
    assert!(body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["type"] == "web_search" && tool["external_web_access"] == false));
    assert!(body["include"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item == "web_search_call.action.sources"));
    let wrong = ResponsesBackend::new(&server.endpoint, None, MODEL)
        .with_max_output_tokens(513)
        .unwrap();
    assert!(snapshot.verify_model(&wrong).is_err());

    let value = serde_json::to_value(&snapshot).unwrap();
    let mut unknown_search = value.clone();
    unknown_search["hosted_web_search"]["unknown_option"] = json!(true);
    assert!(serde_json::from_value::<FrozenNativeAutomationAuthorityV1>(unknown_search).is_err());
    let mut unknown_location = value;
    unknown_location["hosted_web_search"]["location"] =
        json!({"country":null,"region":null,"city":null,"timezone":null,"secret_extra":"denied"});
    assert!(serde_json::from_value::<FrozenNativeAutomationAuthorityV1>(unknown_location).is_err());
}

#[test]
fn malformed_authority_is_rejected_before_credentials_and_network() {
    let directory = tempfile::tempdir().unwrap();
    let server = HttpFixture::new(NativeWireApi::ChatCompletions);
    let (_, run_id, _) = occurrence();
    let original = freeze(
        directory.path(),
        directory.path(),
        &server.endpoint,
        NativeWireApi::ChatCompletions,
        &run_id,
    );
    let value = serde_json::to_value(&original).unwrap();
    let mutations: Vec<(&str, Value)> = vec![
        ("version", json!(2)),
        ("authority_profile", json!("unrestricted")),
        ("mode", json!("plan")),
        ("effort", json!("intense")),
        ("first_turn_id", json!("not-an-automation-turn")),
        ("endpoint", json!("https://secret@example.com/v1")),
        ("endpoint", json!("https://example.com/v1?api_key=secret")),
        ("endpoint", json!("http://example.com/v1")),
        ("credential_env", json!("OTHER-KEY")),
        ("backend_profile_fingerprint", json!("a".repeat(64))),
        ("prompt", json!("Changed prompt")),
        ("instructions", json!("Changed instructions")),
        ("catalog_digest", json!("a".repeat(64))),
        ("permission_policy_digest", json!("a".repeat(64))),
        ("hooks_digest", json!("a".repeat(64))),
        ("chat_request_fields", json!({"messages":[]})),
        (
            "capabilities",
            json!({"tool_calls":false,"reasoning_efforts":[],"tool_call_reasoning_efforts":null,"default_tool_call_effort":null}),
        ),
        (
            "budget",
            json!({"context_window_tokens":100,"output_reserve_tokens":50,"tool_schema_reserve_tokens":50,"auto_compact_token_limit":null}),
        ),
    ];
    for (field, replacement) in mutations {
        let mut altered = value.clone();
        altered[field] = replacement;
        let snapshot: FrozenNativeAutomationAuthorityV1 = serde_json::from_value(altered).unwrap();
        let mut read = false;
        assert!(
            snapshot
                .model_with_credential_reader(|_| {
                    read = true;
                    Some(SECRET.into())
                })
                .is_err(),
            "accepted {field}"
        );
        assert!(!read, "looked up credentials for invalid {field}");
    }
    for path in [
        vec![],
        vec!["capabilities"],
        vec!["budget"],
        vec!["limits"],
        vec!["configured_output_limit"],
    ] {
        let mut altered = value.clone();
        let mut object = &mut altered;
        for key in path {
            object = &mut object[key];
        }
        object["unexpected_authority"] = json!(true);
        assert!(serde_json::from_value::<FrozenNativeAutomationAuthorityV1>(altered).is_err());
    }
    assert!(server.requests().is_empty());
}

#[test]
fn native_origin_roundtrip_reopen_and_first_admission_binding() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let data = directory.path().join("data");
    let (automation_id, run_id, scheduled_at) = occurrence();
    let snapshot = freeze(
        &data,
        &workspace,
        "http://127.0.0.1:9/v1",
        NativeWireApi::ChatCompletions,
        &run_id,
    );
    let origin = snapshot
        .to_origin(&automation_id, &run_id, scheduled_at, false)
        .unwrap();
    assert!(!origin.authority_snapshot_json.contains(SECRET));
    let store = SessionStore::open(directory.path().join("store")).unwrap();
    let session = store.create_native_automation(&workspace, origin).unwrap();
    assert_eq!(
        FrozenNativeAutomationAuthorityV1::from_session(&session).unwrap(),
        Some(snapshot.clone())
    );
    drop(store);
    let reopened = SessionStore::open(directory.path().join("store")).unwrap();
    let saved = reopened.load(&session.id).unwrap().unwrap().session;
    assert_eq!(
        FrozenNativeAutomationAuthorityV1::from_session(&saved).unwrap(),
        Some(snapshot.clone())
    );
    let mut changed = snapshot.clone();
    changed.prompt = "Another admitted prompt".into();
    changed.prompt_digest = prompt_digest(&changed.prompt);
    changed.validate().unwrap();
    let mut bad = saved.clone();
    let origin = bad.native_automation.as_mut().unwrap();
    origin.authority_snapshot_json = serde_json::to_string(&changed).unwrap();
    origin.authority_snapshot_digest =
        openide_core::native_automation_snapshot_digest(&origin.authority_snapshot_json).unwrap();
    assert!(FrozenNativeAutomationAuthorityV1::from_session(&bad).is_err());
    let mut unknown = saved.clone();
    let origin = unknown.native_automation.as_mut().unwrap();
    let mut json: Value = serde_json::from_str(&origin.authority_snapshot_json).unwrap();
    json["current_settings"] = json!(true);
    origin.authority_snapshot_json = serde_json::to_string(&json).unwrap();
    origin.authority_snapshot_digest =
        openide_core::native_automation_snapshot_digest(&origin.authority_snapshot_json).unwrap();
    assert!(FrozenNativeAutomationAuthorityV1::from_session(&unknown).is_err());
    let ordinary = reopened.create(&workspace, "native").unwrap();
    assert_eq!(
        FrozenNativeAutomationAuthorityV1::from_session(&ordinary).unwrap(),
        None
    );
    assert!(snapshot
        .to_origin(&automation_id, &format!("{automation_id}:987"), 987, false)
        .is_err());
    let other_workspace = directory.path().join("other-project");
    fs::create_dir(&other_workspace).unwrap();
    let moved = reopened
        .create_native_automation(&other_workspace, saved.native_automation.clone().unwrap())
        .unwrap();
    assert!(
        FrozenNativeAutomationAuthorityV1::from_session(&moved).is_err(),
        "attributed frozen instructions to another project"
    );
}
