//! Native Anthropic factory, frozen authority and UI acceptance with loopback HTTP.

use super::*;
use crate::native_automation::FrozenNativeAutomationAuthorityV1;
use crate::native_provider::{
    Adapter, DiscoveredModel, NativeOutputLimit, ProviderModelDiscovery, ProviderRegistry,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use openide_core::{AgentMessage, ModelRouteSnapshot, TurnMode, UserImage, UserImageMime};
use openide_ui::{Color, Component, DrawCommand, Frame, HitKind, Point, ScrollArea, Size, UiTree};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const PROVIDER: &str = "native-anthropic-arbitrary-gateway";
const MODEL: &str = "opaque-messages-model-exact-ñ";
const KEY: &str = "explicit-loopback-key-never-written-to-snapshot";
const INSTRUCTIONS: &str = "Original Messages authority résumé 🌱";
const PROMPT: &str = "Confirm the native Messages transport";

#[derive(Clone, Debug)]
struct ObservedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

fn read_http(stream: &TcpStream) -> ObservedRequest {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
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
    let mut bytes = vec![0; headers["content-length"].parse::<usize>().unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    ObservedRequest {
        path,
        headers,
        body: serde_json::from_slice(&bytes).unwrap(),
    }
}

fn respond(mut stream: TcpStream, kind: &str, body: String) {
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    stream.flush().unwrap();
}

struct MessagesFixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MessagesFixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("Anthropic loopback accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let request = read_http(&stream);
                assert_eq!(
                    request.headers.get("x-api-key").map(String::as_str),
                    Some(KEY)
                );
                assert_eq!(
                    request.headers.get("anthropic-version").map(String::as_str),
                    Some("2023-06-01")
                );
                assert_eq!(
                    request
                        .headers
                        .get("anthropic-workspace-id")
                        .map(String::as_str),
                    Some("wrk_fixture_original")
                );
                assert!(!request.headers.contains_key("authorization"));
                assert_eq!(request.body["model"], MODEL);
                captured.lock().unwrap().push(request.clone());
                if request.path == "/v1/messages/count_tokens" {
                    respond(
                        stream,
                        "application/json",
                        json!({"input_tokens":10_000}).to_string(),
                    );
                    continue;
                }
                assert_eq!(request.path, "/v1/messages");
                let output = if request.body["max_tokens"].as_u64().unwrap() <= 512 {
                    64
                } else {
                    3000
                };
                let usage = json!({"input_tokens":10_000,"output_tokens":output});
                let response = json!({"id":"msg_native_app_fixture","type":"message","role":"assistant","model":MODEL,
                    "content":[{"type":"text","text":"Native Messages loopback confirmed."}],
                    "stop_reason":"end_turn","stop_sequence":null,"usage":usage});
                if request.body["stream"] == true {
                    let events = [
                        json!({"type":"message_start","message":{"id":"msg_native_app_fixture","type":"message","role":"assistant","model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10_000,"output_tokens":0}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Native Messages loopback confirmed."}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":output}}),
                        json!({"type":"message_stop"}),
                    ];
                    let body = events
                        .iter()
                        .map(|event| {
                            format!(
                                "event: {}\ndata: {event}\n\n",
                                event["type"].as_str().unwrap()
                            )
                        })
                        .collect::<String>();
                    respond(stream, "text/event-stream", body);
                } else {
                    respond(stream, "application/json", response.to_string());
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

    fn captured(&self) -> Vec<ObservedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MessagesFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if std::thread::panicking() {
                let _ = thread.join();
            } else {
                thread.join().unwrap();
            }
        }
    }
}

fn configure(data: &Path, endpoint: &str) -> (Adapter, Vec<ProviderModelDiscovery>) {
    let mut adapter = ProviderRegistry::builtin()
        .adapter("anthropic")
        .unwrap()
        .clone();
    adapter.id = PROVIDER.into();
    adapter.name = "Native Messages fixture".into();
    adapter.endpoint = endpoint.into();
    adapter.key_env = "OPENIDE_ANTHROPIC_APP_KEY_EXPLICIT_FIXTURE_ONLY".into();
    adapter.key_optional = false;
    adapter.user_endpoint = false;
    adapter.headers.insert(
        "anthropic-workspace-id".into(),
        "wrk_fixture_original".into(),
    );
    adapter.chat_request_fields = BTreeMap::from([("thinking".into(), json!({"type":"adaptive"}))]);
    save_adapter(data, &adapter);
    NativeProviderSettings {
        provider_id: PROVIDER.into(),
        model_id: MODEL.into(),
        custom_endpoint: String::new(),
    }
    .save(data)
    .unwrap();
    let discoveries = vec![ProviderModelDiscovery {
        provider_id: PROVIDER.into(),
        models: BTreeMap::from([(
            MODEL.into(),
            DiscoveredModel {
                listing_index: None,
                limits_declared: true,
                id: MODEL.into(),
                name: "Exact opaque Messages model".into(),
                provider_reports_tools: true,
                tool_call_declared: Some(true),
                text_input_output: true,
                supported_efforts: vec!["low".into(), "high".into()],
                efforts_declared: true,
                capabilities_declared: true,
                context_window_tokens: None,
                max_input_tokens: Some(100_000),
                max_output_tokens: Some(8192),
                declared_capabilities: BTreeMap::from([
                    ("image_input".into(), true),
                    ("thinking".into(), true),
                    ("enabled_thinking".into(), true),
                    ("adaptive_thinking".into(), true),
                    ("effort".into(), true),
                ]),
            },
        )]),
    }];
    (adapter, discoveries)
}

fn save_adapter(data: &Path, adapter: &Adapter) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(
        data.join("provider-registry.json"),
        serde_json::to_vec(&json!({"version":1,"providers":[adapter]})).unwrap(),
    )
    .unwrap();
}

fn connection(adapter: &Adapter) -> RuntimeConnection {
    RuntimeConnection {
        endpoint: adapter.endpoint.clone(),
        api_key: Some(KEY.into()),
        model_id: MODEL.into(),
        wire_api: adapter.wire_api,
        auth: adapter.auth.clone(),
        headers: adapter.headers.clone(),
    }
}

fn factory(
    data: &Path,
    adapter: &Adapter,
    discoveries: &[ProviderModelDiscovery],
    effort: Option<&str>,
) -> Arc<dyn ModelBackend> {
    native_model_backend(
        connection(adapter),
        PROVIDER,
        effort,
        None,
        data,
        None,
        discoveries,
    )
    .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn complete(model: &dyn ModelBackend) {
    let output = runtime()
        .block_on(model.complete(
            &[
                AgentMessage::system(INSTRUCTIONS),
                AgentMessage::user(PROMPT),
            ],
            &[],
            CancellationToken::new(),
        ))
        .unwrap();
    assert_eq!(output.content, "Native Messages loopback confirmed.");
}

fn freeze(
    data: &Path,
    workspace: &Path,
    adapter: &Adapter,
    discoveries: &[ProviderModelDiscovery],
    run_id: &str,
) -> FrozenNativeAutomationAuthorityV1 {
    FrozenNativeAutomationAuthorityV1::freeze(
        connection(adapter),
        PROVIDER,
        data,
        workspace,
        None,
        discoveries,
        None,
        native_token_budget(adapter, MODEL, None, discoveries),
        INSTRUCTIONS.into(),
        PROMPT.into(),
        run_id,
    )
    .unwrap()
}

#[test]
fn messages_factory_uses_exact_effort_output_policy_and_frozen_authority_after_registry_corruption()
{
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let fixture = MessagesFixture::new();
    let (adapter, discoveries) = configure(&data, &fixture.endpoint);
    let capabilities = native_request_capabilities(&adapter, MODEL, None, &discoveries);
    assert_eq!(capabilities.reasoning_efforts, ["low", "high"]);
    let high = factory(&data, &adapter, &discoveries, Some("high"));
    complete(high.as_ref());
    let default = factory(&data, &adapter, &discoveries, Some("default"));
    complete(default.as_ref());
    let automation_id = uuid::Uuid::new_v4().to_string();
    let run_id = format!("{automation_id}:123456");
    let automatic = freeze(&data, &workspace, &adapter, &discoveries, &run_id);
    complete(
        automatic
            .fixture_model_with_key(Some(KEY.into()))
            .unwrap()
            .as_ref(),
    );
    let captured = fixture.captured();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0].body["output_config"], json!({"effort":"high"}));
    for request in &captured[..3] {
        assert_eq!(request.body["max_tokens"], 4096);
        assert_eq!(request.body["thinking"], json!({"type":"adaptive"}));
        assert!(request.body.get("reasoning_effort").is_none());
        assert!(request.body.get("max_completion_tokens").is_none());
    }
    assert!(captured[1].body.get("output_config").is_none());
    assert!(captured[2].body.get("output_config").is_none());
    native_provider::save_output_limit(
        &data,
        PROVIDER,
        MODEL,
        Some(NativeOutputLimit {
            provider_id: PROVIDER.into(),
            model_id: MODEL.into(),
            wire_api: NativeWireApi::AnthropicMessages,
            field: "max_tokens".into(),
            tokens: 512,
        }),
    )
    .unwrap();
    let limited = factory(&data, &adapter, &discoveries, None);
    complete(limited.as_ref());
    let frozen = freeze(&data, &workspace, &adapter, &discoveries, &run_id);
    let frozen_backend = frozen.fixture_model_with_key(Some(KEY.into())).unwrap();
    complete(frozen_backend.as_ref());
    assert_eq!(
        frozen_backend.request_profile_fingerprint(),
        limited.request_profile_fingerprint()
    );
    let sessions = openide_core::SessionStore::open(data.join("native-sessions")).unwrap();
    let origin = frozen
        .to_origin(&automation_id, &run_id, 123456, false)
        .unwrap();
    assert!(!origin.authority_snapshot_json.contains(KEY));
    let session = sessions
        .create_native_automation(&workspace, origin.clone())
        .unwrap();
    drop(sessions);
    std::fs::write(
        data.join("provider-registry.json"),
        b"corrupt current registry",
    )
    .unwrap();
    std::fs::write(
        data.join("native-provider.json"),
        b"corrupt current settings",
    )
    .unwrap();
    std::fs::write(
        data.join("native-output-limits.json"),
        b"corrupt current output policy",
    )
    .unwrap();
    let sessions = openide_core::SessionStore::open(data.join("native-sessions")).unwrap();
    let restored_session = sessions.load(&session.id).unwrap().unwrap().session;
    assert_eq!(restored_session.native_automation.as_ref(), Some(&origin));
    let restored = FrozenNativeAutomationAuthorityV1::from_session(&restored_session)
        .unwrap()
        .unwrap();
    let restored_backend = restored.fixture_model_with_key(Some(KEY.into())).unwrap();
    assert_eq!(
        restored_backend.request_profile_fingerprint(),
        limited.request_profile_fingerprint()
    );
    assert_eq!(restored.budget().unwrap().input_limit_tokens, Some(100_000));
    assert_eq!(restored.budget().unwrap().context_window_tokens, 0);
    complete(restored_backend.as_ref());
    let captured = fixture.captured();
    assert_eq!(captured.len(), 6);
    for request in &captured[3..] {
        assert_eq!(request.body["max_tokens"], 512);
        assert!(request.body.get("output_config").is_none());
    }
    assert_eq!(captured[4].body, captured[5].body);
}

#[test]
fn messages_factory_honors_exact_image_and_thinking_capabilities_before_network_effects() {
    let root = tempfile::tempdir().unwrap();
    let fixture = MessagesFixture::new();
    let (mut adapter, mut discoveries) = configure(root.path(), &fixture.endpoint);
    let model = factory(root.path(), &adapter, &discoveries, None);
    assert!(model.supports_user_images());
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC").unwrap();
    let image = UserImage::from_bytes(UserImageMime::Png, &png).unwrap();
    std::fs::write(root.path().join("screen.png"), &png).unwrap();
    let attachment = composer_attachment_for_path(root.path(), Path::new("screen.png")).unwrap();
    let prepared = prepare_native_attachments_with_image_support(
        root.path(),
        std::slice::from_ref(&attachment),
        native_image_input_supported(&adapter, MODEL, None, &discoveries),
    )
    .unwrap();
    assert_eq!(prepared.images, vec![image.clone()]);
    let input = vec![AgentMessage::user_with_images(PROMPT, prepared.images).unwrap()];
    runtime()
        .block_on(model.complete(&input, &[], CancellationToken::new()))
        .unwrap();
    assert_eq!(
        runtime()
            .block_on(model.count_input_tokens(&input, &[], CancellationToken::new()))
            .unwrap(),
        Some(10_000)
    );
    let captured = fixture.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].path, "/v1/messages");
    assert_eq!(captured[1].path, "/v1/messages/count_tokens");
    assert_eq!(captured[0].body["messages"], captured[1].body["messages"]);
    assert_eq!(
        captured[0].body["messages"][0]["content"][1],
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":image.data_base64}})
    );
    discoveries[0]
        .models
        .get_mut(MODEL)
        .unwrap()
        .declared_capabilities
        .insert("image_input".into(), false);
    let without_images = factory(root.path(), &adapter, &discoveries, None);
    assert!(!without_images.supports_user_images());
    assert!(prepare_native_attachments_with_image_support(
        root.path(),
        std::slice::from_ref(&attachment),
        native_image_input_supported(&adapter, MODEL, None, &discoveries)
    )
    .is_err());
    assert_ne!(
        without_images.request_profile_fingerprint(),
        model.request_profile_fingerprint()
    );
    assert!(runtime()
        .block_on(without_images.complete(&input, &[], CancellationToken::new()))
        .is_err());
    assert!(runtime()
        .block_on(without_images.count_input_tokens(&input, &[], CancellationToken::new()))
        .is_err());
    discoveries[0]
        .models
        .get_mut(MODEL)
        .unwrap()
        .declared_capabilities
        .insert("effort".into(), false);
    assert!(native_model_backend(
        connection(&adapter),
        PROVIDER,
        Some("high"),
        None,
        root.path(),
        None,
        &discoveries
    )
    .is_err());
    discoveries[0]
        .models
        .get_mut(MODEL)
        .unwrap()
        .declared_capabilities
        .insert("adaptive_thinking".into(), false);
    assert!(native_model_backend(
        connection(&adapter),
        PROVIDER,
        None,
        None,
        root.path(),
        None,
        &discoveries
    )
    .is_err());
    adapter.chat_request_fields = BTreeMap::from([(
        "thinking".into(),
        json!({"type":"enabled","budget_tokens":1024}),
    )]);
    save_adapter(root.path(), &adapter);
    complete(factory(root.path(), &adapter, &discoveries, None).as_ref());
    discoveries[0]
        .models
        .get_mut(MODEL)
        .unwrap()
        .declared_capabilities
        .insert("enabled_thinking".into(), false);
    assert!(native_model_backend(
        connection(&adapter),
        PROVIDER,
        None,
        None,
        root.path(),
        None,
        &discoveries
    )
    .is_err());
    assert_eq!(
        fixture.captured().len(),
        3,
        "denied exact capabilities must not issue transport requests"
    );
}

fn frame_text(frame: &Frame) -> String {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn scroll_to_action(tree: &mut UiTree, width: f32, id: &str) -> Frame {
    for _ in 0..60 {
        let frame = tree.layout(Size::new(width, 720.0));
        if frame
            .regions
            .iter()
            .any(|region| region.id == id && region.kind == HitKind::Button)
        {
            return frame;
        }
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(width * 0.65, 360.0),
            delta_y: -180.0,
        });
    }
    panic!("missing reachable action {id} at {width}px");
}

fn click(tree: &mut UiTree, frame: &Frame, id: &str) -> Vec<UiAction> {
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap()
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    actions
}

#[test]
fn messages_components_present_real_factory_journal_and_save_exact_policy_at_both_widths() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let fixture = MessagesFixture::new();
    let (adapter, discoveries) = configure(&data, &fixture.endpoint);
    let model = factory(&data, &adapter, &discoveries, None);
    let budget = native_token_budget(&adapter, MODEL, None, &discoveries).unwrap();
    assert_eq!(budget.context_window_tokens, 0);
    assert_eq!(budget.input_limit_tokens, Some(100_000));
    let mut app = AgentsWindow::with_data_root(data.clone());
    app.view.workspace = workspace.display().to_string();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = PROVIDER.into();
    app.view.model = MODEL.into();
    app.view.composer_model = MODEL.into();
    app.model_discoveries = discoveries.clone();
    app.refresh_composer_routes();
    // The public connection UX intentionally offers four providers. Project
    // the open engine registry directly to test these reusable native leaf
    // components without claiming arbitrary-provider account navigation.
    assert!(!app
        .view
        .composer_routes
        .iter()
        .any(|route| route.provider_id == PROVIDER));
    let registry_routes = composer_selection::model_route_options_with_discovery_at(
        app.view.catalog.as_deref(),
        &app.native_settings(),
        &app.model_discoveries,
        &data,
    )
    .unwrap();
    assert!(registry_routes
        .iter()
        .any(|route| route.provider_id == PROVIDER && route.model_id == MODEL));
    let registry_metadata = crate::model_metadata::for_routes(
        &registry_routes,
        app.view.catalog.as_deref(),
        &app.model_discoveries,
        &[],
    );
    let sessions = app.store.as_ref().unwrap().clone();
    let session = sessions.create(&workspace, "native").unwrap();
    let lease = sessions.try_acquire_native_lease(&session.id).unwrap();
    let admission = sessions
        .admit_native_turn(
            &session.id,
            "anthropic-ui-context",
            PROMPT,
            MODEL,
            TurnMode::Build,
        )
        .unwrap();
    let handle = AgentRunner::new(
        model.clone(),
        Arc::new(LocalTools::new().with_write_access(false)),
    )
    .with_system_instructions(INSTRUCTIONS)
    .with_native_runtime_identity(PROVIDER, &fixture.endpoint)
    .with_token_budget_policy(budget)
    .with_event_journal(sessions.clone())
    .with_native_steering(sessions.clone())
    .start_with_lease(
        openide_core::AgentRunRequest {
            session_id: Some(session.id.clone()),
            sequence_start: admission.sequence,
            workspace: workspace.clone(),
            history: vec![],
            prompt: PROMPT.into(),
            mode: TurnMode::Build,
        },
        lease,
    );
    loop {
        let event = handle.events.recv_timeout(Duration::from_secs(10)).unwrap();
        match event.kind {
            AgentEventKind::TurnCompleted { .. } => break,
            AgentEventKind::TurnFailed { error } => {
                panic!("native Messages context fixture: {error}")
            }
            _ => {}
        }
    }
    while handle.events.recv_timeout(Duration::from_secs(10)).is_ok() {}
    let record = sessions.load(&session.id).unwrap().unwrap();
    let route = ModelRouteSnapshot::new(
        PROVIDER,
        MODEL,
        &fixture.endpoint,
        model.request_profile_fingerprint().unwrap(),
    )
    .unwrap();
    app.view.native_context = crate::native_context_ui::NativeContextUiState::selected_route(
        PROVIDER,
        MODEL,
        workspace.clone(),
        Some(budget),
        Some(8192),
        vec![],
    )
    .apply_validated(&record, replay_stored_native(&data, &record), Some(&route));
    assert!(
        app.view.native_context.error.is_none(),
        "{:?}",
        app.view.native_context.error
    );
    assert_eq!(app.view.native_context.window, None);
    assert_eq!(app.view.native_context.input_limit, Some(100_000));
    assert_eq!(app.view.native_context.remaining_percent, Some(90));
    assert_eq!(
        app.view
            .native_context
            .last_usage
            .as_ref()
            .unwrap()
            .output_tokens,
        3000
    );
    app.view.selected_session = Some(session.id.clone());
    app.view.composer_context_percent = Some(90);
    app.view.composer_context_open = true;
    let ctx = egui::Context::default();
    for width in [360.0, 900.0] {
        let props = crate::composer_ui::ComposerProps::from_state(
            &app.view,
            width,
            false,
            None,
            "Continuar",
            None,
        );
        let mut tree = UiTree::new(crate::composer_ui::build_composer(&props));
        tree.set_reduced_motion(true);
        let frame = tree.layout(Size::new(width, 720.0));
        let text = frame_text(&frame);
        assert!(text.contains("Uso de entrada"), "{text}");
        assert!(text.contains("10% usado"), "{text}");
        assert!(!text.contains("13% usado"));
        // Output remains a reported row; it receives no segment in the input
        // limit meter. The two colors are the renderer's public visual result.
        let input_segment = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if *color == Color::rgb(153, 129, 245) && rect.height == 6.0 =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .unwrap();
        let unused_segment = frame
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. }
                    if *color == crate::theme::TREE_GUIDE
                        && rect.height == 6.0
                        && rect.y == input_segment.y =>
                {
                    Some(*rect)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            (input_segment.width / (input_segment.width + unused_segment.width) - 0.10).abs()
                < 0.001
        );
        assert!(!frame.commands.iter().any(|command|matches!(command,DrawCommand::FillRect {rect,color,..} if *color == Color::rgb(219,129,119) && rect.height == 6.0)));

        let mut tooltip_state = crate::view::tests::empty_state();
        tooltip_state.workspace = app.view.workspace.clone();
        tooltip_state.selected_backend = "native".into();
        tooltip_state.native_provider_id = PROVIDER.into();
        tooltip_state.model = MODEL.into();
        tooltip_state.composer_model = MODEL.into();
        tooltip_state.composer_routes = registry_routes.clone();
        tooltip_state.composer_model_metadata = registry_metadata.clone();
        tooltip_state.composer_context_open = false;
        tooltip_state.composer_model_menu_open = true;
        tooltip_state.composer_model_list_open = true;
        tooltip_state.composer_model_query = MODEL.into();
        tooltip_state.composer_collapsed_model_providers.clear();
        // The key above is explicitly injected into a local RuntimeConnection;
        // no environment detection or external account success is asserted.
        let displayed_route = tooltip_state
            .composer_routes
            .iter_mut()
            .find(|route| route.provider_id == PROVIDER && route.model_id == MODEL)
            .unwrap();
        displayed_route.credential =
            composer_selection::CredentialAvailability::EnvPresentUnverified;
        let tooltip_id = format!("composer_route:{}", displayed_route.route_id);
        let props = crate::composer_ui::ComposerProps::from_state(
            &tooltip_state,
            width,
            false,
            None,
            "Continuar",
            None,
        );
        let mut tree = UiTree::new(crate::composer_ui::build_composer(&props));
        tree.set_reduced_motion(true);
        let start = Instant::now();
        let viewport = Size::new(width, 900.0);
        let initial = tree.layout_at(viewport, start);
        let region = initial
            .regions
            .iter()
            .find(|region| region.id == tooltip_id)
            .unwrap();
        let point = Point::new(
            region.rect.x + region.rect.width / 2.0,
            region.rect.y + region.rect.height / 2.0,
        );
        assert_eq!(
            initial.hit_test(point),
            Some(tooltip_id.as_str()),
            "route region {:?}",
            region.rect
        );
        tree.handle_event_at(UiEvent::PointerMove(point), start);
        tree.layout_at(viewport, start + Duration::from_millis(1));
        let shown = tree.layout_at(viewport, start + Duration::from_millis(700));
        let text = frame_text(&shown);
        for shown in ["Entrada máx.", "100K", "Salida máx.", "8K"] {
            assert!(text.contains(shown), "{shown}: {text}");
        }
    }

    // Saving a different output policy changes the exact request profile and
    // invalidates its previous context measurement. Verify both immutable
    // presentations before exercising these real state-changing actions.
    for width in [360.0, 900.0] {
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Providers;
        app.view.catalog_settings_tab = CatalogSettingsTab::Connection;
        app.refresh_output_limit_editor();
        assert!(app.view.native_output_limit_messages);
        assert!(!app.view.native_output_limit_responses);
        assert_eq!(app.view.native_output_limit_field, "max_tokens");
        app.apply_action(
            UiAction::InputChanged {
                id: "native_output_limit_tokens".into(),
                value: "512".into(),
            },
            &ctx,
        );
        // Render the output editor directly; App still handles the actual
        // action and saves the exact engine route's policy. This exercises no
        // account login or four-provider connection-page navigation.
        let mut tree = UiTree::new(
            ScrollArea::new(
                "test_native_messages_output",
                crate::settings_ui::native_output_settings_for_test(&app.view, width < 640.0),
            )
            .render(),
        );
        tree.set_reduced_motion(true);
        let frame = scroll_to_action(&mut tree, width, "native_output_limit_save");
        let text = frame_text(&frame);
        assert!(text.contains("Messages · max_tokens"), "{text}");
        for id in [
            "native_output_limit_field:max_tokens",
            "native_output_limit_field:max_completion_tokens",
        ] {
            assert!(!tree.focus(id));
            assert!(!frame.regions.iter().any(|region| region.id == id));
        }
        for action in click(&mut tree, &frame, "native_output_limit_save") {
            app.apply_action(action, &ctx);
        }
        assert_eq!(
            native_provider::configured_output_limit(
                &data,
                PROVIDER,
                MODEL,
                NativeWireApi::AnthropicMessages
            )
            .unwrap()
            .unwrap()
            .tokens,
            512
        );
        assert_eq!(app.view.native_context.last_usage, None);
        assert_eq!(app.view.native_context.remaining_percent, None);
        assert_eq!(app.view.composer_context_percent, None);
    }
    assert_eq!(
        fixture.captured().len(),
        3,
        "UI inspection and output setting changes must not infer"
    );
}
