use super::*;
use openide_harness::MemoryTokenStore;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub(crate) fn complete_tool() -> Value {
    json!({"name":"inspect_echo", "description":"Descripción completa del servidor",
        "inputSchema":{"type":"object", "properties":{"nested":{"$ref":"#/$defs/deep"}},
            "$defs":{"deep":{"type":"object", "properties":{"tail":{"type":"string",
                "description":format!("{}FIN_SCHEMA", "evidencia del schema completo ".repeat(80))}}}}},
        "outputSchema":{"type":"object","properties":{"ok":{"type":"boolean"}}},
        "annotations":{"readOnlyHint":false},"_meta":{"source":"servidor real local"}})
}

fn second_tool() -> Value {
    json!({"name":"second-tool", "inputSchema":{"type":"object", "additionalProperties":false}})
}

pub(crate) fn sample_snapshot() -> McpInspectionSnapshot {
    make_snapshot(
        McpServerInfo {
            protocol_version: "2025-06-18".into(),
            name: "inspection fixture".into(),
            version: "7".into(),
            capabilities: json!({"tools":{}}),
            instructions: Some("Datos completos del servidor".into()),
        },
        vec![serde_json::from_value(complete_tool()).unwrap()],
        &InspectionSecrets::default(),
    )
    .unwrap()
}

pub(crate) struct HttpFixture {
    pub server: McpServerSettings,
    pub blocked: Arc<AtomicBool>,
    pub release: Arc<AtomicBool>,
    pub deleted: Arc<AtomicBool>,
    methods: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HttpFixture {
    pub fn new(legacy: bool, blocking: bool) -> Self {
        Self::with_blocked_method(legacy, blocking.then_some("tools/list"))
    }

    fn with_blocked_method(legacy: bool, blocked_method: Option<&'static str>) -> Self {
        Self::with_http_options(legacy, blocked_method, false)
    }

    /// A valid legacy endpoint whose optional notification GET is unsupported.
    /// The normal constructor retains the OAuth challenge used by its tests.
    pub fn with_unsupported_notifications(legacy: bool) -> Self {
        Self::with_http_options(legacy, None, true)
    }

    fn with_http_options(
        legacy: bool,
        blocked_method: Option<&'static str>,
        unsupported_notifications: bool,
    ) -> Self {
        Self::with_catalog_options(legacy, blocked_method, unsupported_notifications, None)
    }

    pub(crate) fn with_tools(legacy: bool, tools: Vec<Value>) -> Self {
        Self::with_catalog_options(legacy, None, false, Some(tools))
    }

    fn with_catalog_options(
        legacy: bool,
        blocked_method: Option<&'static str>,
        unsupported_notifications: bool,
        catalog: Option<Vec<Value>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = McpServerSettings {
            id: "inspection-local".into(),
            enabled: true,
            program: String::new(),
            args: vec![],
            cwd: None,
            credential_env_names: vec![],
            env: Default::default(),
            url: Some(format!("{base}/mcp")),
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        };
        let blocked = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let deleted = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let methods = Arc::new(Mutex::new(Vec::new()));
        let (worker_blocked, worker_release, worker_deleted, worker_stop, worker_methods) = (
            blocked.clone(),
            release.clone(),
            deleted.clone(),
            stop.clone(),
            methods.clone(),
        );
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while !worker_stop.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "MCP inspection fixture timed out"
                        );
                        std::thread::sleep(Duration::from_millis(3));
                        continue;
                    }
                    Err(error) => panic!("MCP fixture accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut first = line.split_whitespace();
                let verb = first.next().unwrap().to_owned();
                let path = first.next().unwrap().to_owned();
                assert_eq!(path, "/mcp");
                let mut content_length = 0usize;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if verb == "GET" {
                    if unsupported_notifications {
                        worker_methods.lock().unwrap().push("GET".into());
                        respond(
                            reader.get_mut(),
                            "405 Method Not Allowed",
                            &Value::Null,
                            false,
                            None,
                        );
                        continue;
                    }
                    respond(reader.get_mut(), "401 Unauthorized", &json!({}), false, Some(&format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\"\r\n")));
                    continue;
                }
                if verb == "DELETE" {
                    worker_methods.lock().unwrap().push("DELETE".into());
                    worker_deleted.store(true, Ordering::Release);
                    respond(
                        reader.get_mut(),
                        "204 No Content",
                        &Value::Null,
                        false,
                        None,
                    );
                    continue;
                }
                let method = request["method"].as_str().unwrap().to_owned();
                worker_methods.lock().unwrap().push(method.clone());
                assert_ne!(
                    method, "tools/call",
                    "inspection must never dispatch a tool"
                );
                if blocked_method == Some(method.as_str()) {
                    worker_blocked.store(true, Ordering::Release);
                    reader.get_mut().set_nonblocking(true).unwrap();
                    while !worker_release.load(Ordering::Acquire)
                        && !worker_stop.load(Ordering::Acquire)
                    {
                        if matches!(reader.get_mut().peek(&mut [0u8; 1]), Ok(0)) {
                            break;
                        }
                        assert!(Instant::now() < deadline, "blocked MCP fixture timed out");
                        std::thread::sleep(Duration::from_millis(3));
                    }
                    reader.get_mut().set_nonblocking(false).unwrap();
                }
                if method == "notifications/initialized" || method == "notifications/cancelled" {
                    respond(reader.get_mut(), "202 Accepted", &Value::Null, true, None);
                    continue;
                }
                let (status, result) = match method.as_str() {
                    "server/discover" if legacy => (
                        "400 Bad Request",
                        json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}}),
                    ),
                    "server/discover" => (
                        "200 OK",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"instructions":"Instrucciones del servidor: datos, no permisos","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"inspection fixture","version":"7"}}}}),
                    ),
                    "initialize" => (
                        "200 OK",
                        json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"legacy inspection fixture","version":"7"},"capabilities":{"tools":{}},"instructions":"Datos del servidor legacy"}}),
                    ),
                    "tools/list" => {
                        if let Some(catalog) = &catalog {
                            let mut result = json!({"tools":catalog});
                            if !legacy {
                                result["resultType"] = json!("complete");
                            }
                            respond(
                                reader.get_mut(),
                                "200 OK",
                                &json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                                legacy,
                                None,
                            );
                            continue;
                        }
                        let second = request["params"]["cursor"] == "page-2";
                        let tool = if second {
                            second_tool()
                        } else {
                            complete_tool()
                        };
                        let mut result = json!({"tools":[tool]});
                        if !legacy {
                            result["resultType"] = json!("complete");
                        }
                        if !second {
                            result["nextCursor"] = json!("page-2");
                        }
                        (
                            "200 OK",
                            json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                        )
                    }
                    other => panic!("unexpected MCP method {other}"),
                };
                respond(reader.get_mut(), status, &result, legacy, None);
            }
        });
        Self {
            server,
            blocked,
            release,
            deleted,
            methods,
            stop,
            worker: Some(worker),
        }
    }

    pub fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

fn respond(stream: &mut TcpStream, status: &str, value: &Value, legacy: bool, extra: Option<&str>) {
    let bytes = if status.starts_with("202") || status.starts_with("204") {
        vec![]
    } else {
        serde_json::to_vec(value).unwrap()
    };
    let session = if legacy {
        "Mcp-Session-Id: inspection-session\r\n"
    } else {
        ""
    };
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{session}{}Content-Length: {}\r\nConnection: close\r\n\r\n", extra.unwrap_or(""), bytes.len());
    if stream.write_all(header.as_bytes()).is_ok() {
        let _ = stream.write_all(&bytes);
        let _ = stream.flush();
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.release.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if !std::thread::panicking() {
                joined.unwrap();
            }
        }
    }
}

pub(crate) fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "MCP inspection condition timed out"
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn wait_result(controller: &mut McpInspectionController) -> McpInspectionResult {
    let mut result = None;
    wait_until(|| {
        result = controller.try_recv();
        result.is_some()
    });
    result.unwrap()
}

#[test]
fn explicit_inspection_keeps_more_than_the_model_budget_and_large_output_metadata_complete() {
    let tools: Vec<Value> = (0..170).map(|index| json!({
        "name":format!("catalog_tool_{index}"),
        "description":"Fixture tool metadata",
        "inputSchema":{"type":"object"},
        "outputSchema":{"type":"object","description":if index == 0 { "large-output-metadata-".repeat(4_000) } else { "output".into() }},
        "annotations":{"readOnlyHint":true}
    })).collect();
    assert!(serde_json::to_vec(&tools[0]).unwrap().len() > 64 * 1024);
    for legacy in [false, true] {
        let fixture = HttpFixture::with_tools(legacy, tools.clone());
        let workspace = tempfile::tempdir().unwrap();
        let mut controller =
            McpInspectionController::start(fixture.server.clone(), workspace.path()).unwrap();
        let result = wait_result(&mut controller);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(result.shutdown_error.is_none());
        let snapshot = result.snapshot.unwrap();
        assert_eq!(snapshot.tools.len(), 170);
        // HTTP catalogs are keyed by name and returned in lexical order. The
        // complete metadata for every unique name must survive unchanged;
        // input fixture insertion order is not a discovery contract.
        let expected: std::collections::BTreeMap<String, Value> = tools
            .iter()
            .map(|tool| (tool["name"].as_str().unwrap().to_owned(), tool.clone()))
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for tool in &snapshot.tools {
            assert!(
                seen.insert(&tool.name),
                "duplicated observed tool {}",
                tool.name
            );
            let expected = expected
                .get(&tool.name)
                .expect("unrecognized observed tool");
            assert!(
                &serde_json::to_value(tool).unwrap() == expected,
                "complete metadata changed for {}",
                tool.name
            );
        }
        let details: Value = serde_json::from_str(&snapshot.details_json).unwrap();
        assert!(
            details["tools"] == serde_json::to_value(&snapshot.tools).unwrap(),
            "technical JSON must preserve every tool and its complete metadata"
        );
        assert!(!fixture
            .methods()
            .iter()
            .any(|method| method == "tools/call"));
    }
}

/// An explicit local integration probe, never part of the regular suite. The
/// controller owns initialize/list/shutdown only and cannot invoke MCP tools.
#[test]
#[ignore = "requires explicit local REA initialize/list/shutdown probe"]
fn local_rea_inspection_probe() {
    let program = std::env::var_os("OPENADE_MCP_REAL_PROBE")
        .expect("set OPENADE_MCP_REAL_PROBE to the observed local REA 6.4.0 executable");
    let program = PathBuf::from(program);
    assert!(
        program.is_absolute(),
        "the probe requires an explicit absolute executable path"
    );
    let workspace = tempfile::tempdir().unwrap();
    let server = McpServerSettings {
        id: "rea-local-probe".into(),
        enabled: true,
        program: program.to_string_lossy().into_owned(),
        args: vec!["--mcp".into()],
        cwd: None,
        credential_env_names: vec![],
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: vec![],
    };
    let mut controller = McpInspectionController::start(server, workspace.path())
        .expect("the local REA inspection worker must start");
    let deadline = Instant::now() + Duration::from_secs(45);
    let result = loop {
        if let Some(result) = controller.try_recv() {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "the opt-in inspection exceeded 45 seconds"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    // Deliberately omit remote diagnostics and metadata from failure output.
    assert!(!result.cancelled, "the local REA inspection was cancelled");
    assert!(
        result.error.is_none(),
        "the local REA inspection failed; remote diagnostics omitted"
    );
    assert!(
        result.shutdown_error.is_none(),
        "the owned REA connection did not close cleanly"
    );
    let snapshot = result
        .snapshot
        .expect("completed inspection must retain a snapshot");
    assert_eq!(
        snapshot.tools.len(),
        170,
        "this probe pins the observed REA 6.4.0 catalog"
    );
    let raw = serde_json::to_value(&snapshot.tools).unwrap();
    let details: Value = serde_json::from_str(&snapshot.details_json).unwrap();
    assert!(
        details["tools"] == raw,
        "the technical inspection must preserve the full received catalog"
    );
    let definition_sizes: Vec<_> = snapshot
        .tools
        .iter()
        .map(|tool| serde_json::to_vec(tool).unwrap().len())
        .collect();
    let input_sizes: Vec<_> = snapshot
        .tools
        .iter()
        .map(|tool| serde_json::to_vec(&tool.input_schema).unwrap().len())
        .collect();
    let largest_definition = *definition_sizes.iter().max().unwrap();
    let largest_input_schema = *input_sizes.iter().max().unwrap();
    assert!(
        largest_definition > 64 * 1024,
        "the real catalog must exercise the formerly rejected metadata boundary"
    );
    assert!(largest_input_schema <= openide_harness::MAX_TOOL_SCHEMA_BYTES);
    let comparison = snapshot
        .tools
        .iter()
        .find(|tool| tool.name == "compare_web_captures")
        .expect("the measured large definition must be present");
    assert!(comparison.extra.contains_key("outputSchema"));
    assert!(comparison.extra.contains_key("annotations"));
    let report = json!({
        "probe":"patched_mcp_stdio_client_via_inspection_controller",
        "observed_package_version":"6.4.0",
        "program":program,
        "arguments":["--mcp"],
        "protocol_version":snapshot.server_info.protocol_version,
        "tools":snapshot.tools.len(),
        "catalog_definition_bytes":definition_sizes.iter().sum::<usize>(),
        "catalog_input_schema_bytes":input_sizes.iter().sum::<usize>(),
        "largest_definition_bytes":largest_definition,
        "largest_input_schema_bytes":largest_input_schema,
        "technical_json_bytes":snapshot.details_json.len(),
        "metadata_digest":snapshot.metadata_digest,
        "shutdown_confirmed":true,
        "agent_turn_started":false,
        "tools_call_sent":false
    });
    if let Some(path) = std::env::var_os("OPENADE_MCP_REAL_PROBE_SUMMARY") {
        let path = PathBuf::from(path);
        assert!(
            path.is_absolute(),
            "summary output must use an explicit absolute path"
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("{}", serde_json::to_string(&report).unwrap());
}

#[test]
fn explicit_http_inspection_negotiates_both_eras_reads_full_pages_and_closes_without_tool_calls() {
    for legacy in [false, true] {
        let fixture = HttpFixture::new(legacy, false);
        let workspace = tempfile::tempdir().unwrap();
        let mut controller =
            McpInspectionController::start(fixture.server.clone(), workspace.path()).unwrap();
        let result = wait_result(&mut controller);
        assert!(!result.cancelled);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(
            result.shutdown_error.is_none(),
            "{:?}",
            result.shutdown_error
        );
        let snapshot = result.snapshot.unwrap();
        assert_eq!(
            snapshot.server_info.protocol_version,
            if legacy { "2025-06-18" } else { "2026-07-28" }
        );
        assert_eq!(snapshot.server_info.version, "7");
        assert_eq!(snapshot.tools.len(), 2);
        assert_eq!(
            serde_json::to_value(&snapshot.tools[0]).unwrap(),
            complete_tool()
        );
        assert!(snapshot.details_json.contains("FIN_SCHEMA"));
        assert!(snapshot.details_json.contains("outputSchema"));
        assert_eq!(snapshot.metadata_digest.len(), 64);
        assert_eq!(fixture.deleted.load(Ordering::Acquire), legacy);
        let methods = fixture.methods();
        assert_eq!(
            methods
                .iter()
                .filter(|method| *method == "tools/list")
                .count(),
            2
        );
        assert!(!methods.iter().any(|method| method == "tools/call"));
        if legacy {
            assert_eq!(methods.last().unwrap(), "DELETE");
        }
    }
}

#[test]
fn cancelling_http_inspection_is_nonblocking_and_waits_for_legacy_delete() {
    let fixture = HttpFixture::new(true, true);
    let workspace = tempfile::tempdir().unwrap();
    let mut controller =
        McpInspectionController::start(fixture.server.clone(), workspace.path()).unwrap();
    wait_until(|| fixture.blocked.load(Ordering::Acquire));
    let started = Instant::now();
    controller.cancel();
    assert!(started.elapsed() < Duration::from_millis(100));
    let result = wait_result(&mut controller);
    assert!(result.cancelled);
    assert!(result.snapshot.is_none());
    assert!(result.shutdown_error.is_none());
    assert!(fixture.deleted.load(Ordering::Acquire));
    assert_eq!(fixture.methods().last().unwrap(), "DELETE");
}

#[test]
fn http_timeout_keeps_error_and_closes_legacy_session() {
    let fixture = HttpFixture::new(true, true);
    let workspace = tempfile::tempdir().unwrap();
    let mut controller = McpInspectionController::start_with_limits(
        fixture.server.clone(),
        workspace.path(),
        InspectionLimits {
            request_timeout: Duration::from_millis(250),
            total_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    let result = wait_result(&mut controller);
    assert!(!result.cancelled);
    assert!(result.snapshot.is_none());
    assert!(result.error.as_deref().unwrap().contains("timed out"));
    assert!(result.shutdown_error.is_none());
    assert!(fixture.deleted.load(Ordering::Acquire));
}

#[test]
fn total_deadline_during_legacy_handshake_waits_for_partial_session_cleanup() {
    let fixture = HttpFixture::with_blocked_method(true, Some("notifications/initialized"));
    let workspace = tempfile::tempdir().unwrap();
    let mut controller = McpInspectionController::start_with_limits(
        fixture.server.clone(),
        workspace.path(),
        InspectionLimits {
            request_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_millis(150),
            shutdown_timeout: Duration::from_secs(1),
        },
    )
    .unwrap();
    wait_until(|| fixture.blocked.load(Ordering::Acquire));
    let result = wait_result(&mut controller);
    assert!(!result.cancelled);
    assert!(result.snapshot.is_none());
    assert!(result.error.as_deref().unwrap().contains("tiempo límite"));
    assert!(result.shutdown_error.is_none());
    assert!(fixture.deleted.load(Ordering::Acquire));
    assert_eq!(fixture.methods().last().unwrap(), "DELETE");
    assert!(!fixture
        .methods()
        .iter()
        .any(|method| method == "tools/list"));
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_inspection_owner_is_nonblocking_and_releases_http_session_and_stdio_process() {
    let fixture = HttpFixture::new(true, true);
    let workspace = tempfile::tempdir().unwrap();
    let controller =
        McpInspectionController::start(fixture.server.clone(), workspace.path()).unwrap();
    wait_until(|| fixture.blocked.load(Ordering::Acquire));
    let started = Instant::now();
    drop(controller);
    assert!(started.elapsed() < Duration::from_millis(100));
    wait_until(|| fixture.deleted.load(Ordering::Acquire));
    assert_eq!(fixture.methods().last().unwrap(), "DELETE");

    let workspace = tempfile::tempdir().unwrap();
    let controller =
        McpInspectionController::start(stdio_server(workspace.path(), "stall"), workspace.path())
            .unwrap();
    wait_until(|| {
        std::fs::read_to_string(workspace.path().join("stdio.log"))
            .is_ok_and(|log| log.contains("tools/list"))
    });
    let pid = std::fs::read_to_string(workspace.path().join("stdio.pid")).unwrap();
    let started = Instant::now();
    drop(controller);
    assert!(started.elapsed() < Duration::from_millis(100));
    wait_until(|| !Path::new(&format!("/proc/{}", pid.trim())).exists());
    assert!(!std::fs::read_to_string(workspace.path().join("stdio.log"))
        .unwrap()
        .contains("UNEXPECTED_TOOL_CALL"));
}

#[cfg(unix)]
fn stdio_server(root: &Path, mode: &str) -> McpServerSettings {
    let initialize = json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","serverInfo":{"name":"stdio inspection fixture","version":"9"},"capabilities":{"tools":{}},"instructions":"Datos stdio"}});
    let first =
        json!({"jsonrpc":"2.0","id":2,"result":{"tools":[complete_tool()],"nextCursor":"page-2"}});
    let second = json!({"jsonrpc":"2.0","id":3,"result":{"tools":[second_tool()]}});
    let script = format!(
        r#"log="$1"
pidfile="$2"
mode="$3"
printf '%s\n' "$$" > "$pidfile"
lists=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf 'initialize\n' >> "$log"
      if [ "$mode" = badstdout ]; then printf 'invalid diagnostic on stdout\n'; fi
      printf '%s\n' '{initialize}'
      ;;
    *'"method":"notifications/initialized"'*) printf 'initialized\n' >> "$log" ;;
    *'"method":"tools/list"'*)
      printf 'tools/list\n' >> "$log"
      if [ "$mode" = stall ]; then IFS= read -r ignored; fi
      lists=$((lists+1))
      if [ "$lists" = 1 ]; then printf '%s\n' '{first}'; else printf '%s\n' '{second}'; fi
      ;;
    *'"method":"tools/call"'*) printf 'UNEXPECTED_TOOL_CALL\n' >> "$log" ;;
  esac
done
printf 'EOF-CLOSED\n' >> "$log"
"#
    );
    let program = root.join("inspection-fixture.sh");
    std::fs::write(&program, script).unwrap();
    McpServerSettings {
        id: "stdio-inspection".into(),
        enabled: true,
        program: "sh".into(),
        args: vec![
            program.to_string_lossy().into_owned(),
            root.join("stdio.log").to_string_lossy().into_owned(),
            root.join("stdio.pid").to_string_lossy().into_owned(),
            mode.into(),
        ],
        cwd: None,
        credential_env_names: vec![],
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: Vec::new(),
    }
}

#[cfg(unix)]
#[test]
fn stdio_inspection_preserves_schema_and_waits_for_process_eof_before_terminal() {
    let workspace = tempfile::tempdir().unwrap();
    let mut controller =
        McpInspectionController::start(stdio_server(workspace.path(), "normal"), workspace.path())
            .unwrap();
    let result = wait_result(&mut controller);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(result.shutdown_error.is_none());
    let snapshot = result.snapshot.unwrap();
    assert_eq!(snapshot.server_info.protocol_version, "2025-11-25");
    assert_eq!(snapshot.tools.len(), 2);
    assert_eq!(
        serde_json::to_value(&snapshot.tools[0]).unwrap(),
        complete_tool()
    );
    let log = std::fs::read_to_string(workspace.path().join("stdio.log")).unwrap();
    assert!(log.ends_with("EOF-CLOSED\n"));
    assert_eq!(log.matches("tools/list").count(), 2);
    assert!(!log.contains("UNEXPECTED_TOOL_CALL"));
}

#[cfg(target_os = "linux")]
#[test]
fn stdio_cancel_timeout_and_invalid_stdout_release_owned_process() {
    for mode in ["cancel", "timeout", "badstdout"] {
        let workspace = tempfile::tempdir().unwrap();
        // Cancellation is observed at the real tools/list boundary. Its setup
        // must not race the deliberately short deadline of the timeout case,
        // especially while other workspace tests are spawning processes.
        let limits = if mode == "cancel" {
            InspectionLimits::default()
        } else {
            InspectionLimits {
                request_timeout: Duration::from_millis(300),
                total_timeout: Duration::from_secs(2),
                shutdown_timeout: Duration::from_secs(1),
            }
        };
        let mut controller = McpInspectionController::start_with_limits(
            stdio_server(
                workspace.path(),
                if mode == "badstdout" { mode } else { "stall" },
            ),
            workspace.path(),
            limits,
        )
        .unwrap();
        if mode == "cancel" {
            wait_until(|| {
                let at_tool_request = std::fs::read_to_string(workspace.path().join("stdio.log"))
                    .is_ok_and(|log| log.contains("tools/list"));
                if !at_tool_request {
                    if let Some(result) = controller.try_recv() {
                        panic!("Inspection terminated before the cancellation boundary: {:?}; shutdown: {:?}", result.error, result.shutdown_error);
                    }
                }
                at_tool_request
            });
            let started = Instant::now();
            controller.cancel();
            assert!(started.elapsed() < Duration::from_millis(100));
        }
        let result = wait_result(&mut controller);
        assert_eq!(result.cancelled, mode == "cancel");
        assert!(result.snapshot.is_none());
        assert!(result.error.is_some());
        assert!(result.shutdown_error.is_none());
        let pid = std::fs::read_to_string(workspace.path().join("stdio.pid")).unwrap();
        wait_until(|| !Path::new(&format!("/proc/{}", pid.trim())).exists());
        assert!(!std::fs::read_to_string(workspace.path().join("stdio.log"))
            .unwrap()
            .contains("UNEXPECTED_TOOL_CALL"));
    }
}

#[tokio::test]
async fn oauth_store_observer_preserves_refresh_cas_and_checks_new_tokens_before_report_hash() {
    let tokens = Arc::new(MemoryTokenStore::default());
    let key = TokenKey {
        resource: "https://mcp.example.test/mcp".into(),
        issuer: "https://issuer.example.test".into(),
        client_id: "inspection-client".into(),
    };
    let secrets = InspectionSecrets::default();
    let store: Arc<dyn TokenStore> = Arc::new(InspectionTokenStore {
        inner: tokens.clone(),
        secrets: secrets.clone(),
    });
    let initial = OAuthTokens {
        access_token: "initial-expired-access".into(),
        refresh_token: Some("initial-refresh".into()),
        expires_at_unix_seconds: Some(0),
        scopes: vec!["read".into()],
    };
    store.save(&key, initial.clone()).await.unwrap();
    assert!(store.load(&key).await.unwrap().is_some());
    let grant = tokens.resume_grant_identity(&key).await.unwrap();
    let mut previous = initial.clone();
    for index in 1..=6 {
        let refreshed = OAuthTokens {
            access_token: format!("issued-access-{index}"),
            refresh_token: Some(format!("issued-refresh-{index}")),
            expires_at_unix_seconds: Some(index),
            scopes: vec!["read".into()],
        };
        store
            .save_refreshed(&key, &previous, refreshed.clone())
            .await
            .unwrap();
        previous = refreshed;
    }
    let mut tool: McpTool = serde_json::from_value(complete_tool()).unwrap();
    tool.description = Some("issued-access-7".into());
    assert!(!secrets.appears_in(&serde_json::to_value(&tool).unwrap()));
    // Real store CAS at a simulated closing boundary. This does not exercise
    // network OAuth refresh, browser authorization or a platform vault.
    let closing_tokens = OAuthTokens {
        access_token: "issued-access-7".into(),
        refresh_token: Some("issued-refresh-7".into()),
        expires_at_unix_seconds: Some(7),
        scopes: vec!["read".into()],
    };
    store
        .save_refreshed(&key, &previous, closing_tokens.clone())
        .await
        .unwrap();
    let info = McpServerInfo {
        protocol_version: "2025-06-18".into(),
        name: "fixture".into(),
        version: "1".into(),
        capabilities: json!({"tools":{}}),
        instructions: None,
    };
    let error = make_snapshot(info, vec![tool], &secrets).unwrap_err();
    assert!(error.contains("credencial"));
    for index in 1..=7 {
        for prefix in ["issued-access", "issued-refresh"] {
            let token = format!("{prefix}-{index}");
            assert!(!error.contains(&token));
            assert_eq!(
                secrets.sanitize(format!("rejected {token}")),
                "rejected [credencial]"
            );
        }
    }
    assert_eq!(tokens.resume_grant_identity(&key).await.unwrap(), grant);
    assert_eq!(
        store.save_refreshed(&key, &initial, closing_tokens).await,
        Err(OAuthError::AuthorizationRequired)
    );
    assert!(!store.delete_if_current(&key, &initial).await.unwrap());
    assert!(tokens.load(&key).await.unwrap().is_some());
    store.delete(&key).await.unwrap();
    assert!(tokens.load(&key).await.unwrap().is_none());
}

#[test]
fn secret_observation_limit_suppresses_payload_and_diagnostics_without_new_persistence() {
    let secrets = InspectionSecrets::default();
    for index in 0..257 {
        secrets.add(format!("private-token-{index}"));
    }
    assert!(secrets.appears_in(&json!({"normal":"metadata"})));
    assert!(!secrets
        .sanitize("private-token-256 remote error".into())
        .contains("private-token-256"));
}

#[test]
fn http_header_credentials_cannot_reach_inspection_metadata_or_diagnostics() {
    let mut config = openide_harness::McpHttpConfig::new("https://example.test/mcp");
    config.bearer_token = Some("fixture-bearer-private".into());
    config.env_headers = vec![("X-Api-Key".into(), "fixture-header-private".into())];
    let secrets = InspectionSecrets::default();
    secrets.observe_http_config(&config);
    for secret in ["fixture-bearer-private", "fixture-header-private"] {
        let snapshot = sample_snapshot();
        let mut tool = complete_tool();
        tool["description"] = json!(format!("Reflected credential: {secret}"));
        let result = make_snapshot(
            snapshot.server_info,
            vec![serde_json::from_value(tool).unwrap()],
            &secrets,
        );
        assert!(result.is_err());
        let diagnostic = secrets.sanitize(format!("Remote failure: {secret}"));
        assert!(!diagnostic.contains(secret));
        assert!(diagnostic.contains("[credencial]"));
    }
}

#[test]
fn the_last_connection_to_each_server_is_remembered_until_another_says_otherwise() {
    let workspace = tempfile::tempdir().unwrap();
    let server = |id: &str| McpServerSettings {
        id: id.into(),
        enabled: true,
        program: "servidor".into(),
        args: vec![],
        cwd: None,
        credential_env_names: vec![],
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: Vec::new(),
    };
    let saved = McpSettings {
        servers: vec![server("uno"), server("dos")],
    };
    let mut state = McpInspectionUiState::default();
    let mut inspect = |state: &mut McpInspectionUiState,
                       id: &str,
                       snapshot: Option<McpInspectionSnapshot>,
                       error: Option<&str>,
                       cancelled: bool| {
        let scope = McpInspectionScope {
            request_id: 1,
            workspace: workspace.path().canonicalize().unwrap(),
            server: server(id),
        };
        state.begin(scope.clone());
        let result = McpInspectionResult {
            scope,
            snapshot,
            error: error.map(str::to_owned),
            shutdown_error: None,
            cancelled,
        };
        assert!(state.apply_result(result, workspace.path(), &saved));
    };
    let tools = sample_snapshot().tools.len();
    inspect(&mut state, "uno", Some(sample_snapshot()), None, false);
    assert_eq!(
        state.last.get("uno"),
        Some(&McpServerStatus::Connected { tools })
    );
    // Another server's inspection keeps what was seen of the first.
    inspect(&mut state, "dos", None, Some("conexión rechazada"), false);
    assert_eq!(
        state.last.get("uno"),
        Some(&McpServerStatus::Connected { tools })
    );
    assert_eq!(
        state.last.get("dos"),
        Some(&McpServerStatus::Failed("conexión rechazada".into()))
    );
    // A cancelled one says nothing about the server; a later success replaces a failure.
    inspect(&mut state, "dos", None, None, true);
    assert_eq!(
        state.last.get("dos"),
        Some(&McpServerStatus::Failed("conexión rechazada".into()))
    );
    inspect(&mut state, "dos", Some(sample_snapshot()), None, false);
    assert_eq!(
        state.last.get("dos"),
        Some(&McpServerStatus::Connected { tools })
    );
}
