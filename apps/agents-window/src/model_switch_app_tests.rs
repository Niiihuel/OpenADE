//! Continuing a paused turn on another model, through the real window, the
//! real runner and a loopback provider. Paused journals are written by hand.

use super::*;
use crate::model_switch::{SwitchBlocker, SwitchStep};
use openide_core::{ToolCall, TurnPauseClass, UserImage, UserImageMime};

const OTHER: &str = "streaming/model:other";
const TASK: &str = "Migrá los tests";

fn switch_app(root: &Path, endpoint: &str) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = root.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = MODEL.into();
    app.view.endpoint = endpoint.into();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    app.tree.set_reduced_motion(true);
    app.reduced_motion = true;
    app
}

/// A session whose turn paused on a limit: before any response, or after one
/// tool call that has its result.
fn paused(app: &AgentsWindow, root: &Path, worked: bool, images: &[UserImage]) -> String {
    let store = app.store.as_ref().unwrap();
    let session = store.create(root, "native").unwrap();
    let admitted = store
        .admit_native_turn_with_images(&session.id, "turn-1", TASK, MODEL, TurnMode::Build, images)
        .unwrap();
    let call = ToolCall { id: "call-a".into(), name: "read_file".into(), arguments: json!({"path": "a"}) };
    let mut kinds = vec![
        AgentEventKind::TurnStarted { prompt: TASK.into() },
        AgentEventKind::ModelRequestStarted { step: 1, model: MODEL.into() },
    ];
    if worked {
        kinds.extend([
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
            AgentEventKind::ToolStarted { tool_call: call },
            AgentEventKind::ToolFinished {
                tool_call_id: "call-a".into(),
                output: "RESULTADO_YA_OBTENIDO".into(),
                is_error: false,
            },
            AgentEventKind::ModelRequestStarted { step: 2, model: MODEL.into() },
        ]);
    }
    kinds.push(AgentEventKind::TurnPaused {
        class: TurnPauseClass::LimitReached,
        limit_key: "model-switch-test".into(),
        resets_at_ms: None,
        step: if worked { 2 } else { 1 },
        detail: "HTTP 429: usage_limit_reached".into(),
    });
    for (offset, kind) in kinds.into_iter().enumerate() {
        store
            .append_native_event(&AgentEvent::new(&session.id, admitted.sequence + 1 + offset as u64, kind))
            .unwrap();
    }
    session.id
}

fn open(app: &mut AgentsWindow, session_id: &str) {
    app.open_session(session_id);
    app.refresh_sessions();
    native_frame(app);
}

/// Offer the same loopback provider under a second model, as a provider
/// listing would, and return its route.
fn other_route(app: &mut AgentsWindow, provider: &str) -> String {
    use composer_selection::{CredentialAvailability, ExecutionSupport, ModelRouteOption, RouteSource};
    let route = ModelRouteOption {
        route_id: format!("test:{provider}:{OTHER}"),
        backend: "native".into(),
        provider_id: provider.into(),
        provider_name: provider.into(),
        model_id: OTHER.into(),
        model_name: "Otro modelo".into(),
        credential: CredentialAvailability::NoKeyRequired,
        execution: ExecutionSupport::UnverifiedUserChoice,
        source: RouteSource::UserConfigured,
        catalog_present: false,
        provider_listed: None,
        effort_options: Vec::new(),
    };
    app.view.composer_routes.push(route.clone());
    route.route_id
}

fn journal(app: &AgentsWindow, session_id: &str) -> Vec<AgentEvent> {
    app.store.as_ref().unwrap().load(session_id).unwrap().unwrap().events
}

fn click(app: &mut AgentsWindow, id: &str) {
    app.apply_action(UiAction::Clicked { id: id.into() }, &egui::Context::default());
}

/// Run the continuation the switch queued and return the request it sent.
fn run_continuation(app: &mut AgentsWindow, fixture: &StreamingHttpFixture) -> Value {
    assert!(app.drive_switch_queue());
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    wait_app(app, |app| assistant_rows(app).last() == Some(&(FIRST_VISIBLE, true)));
    fixture.next_delta();
    fixture.terminal();
    wait_app(app, |app| app.active_run.is_none());
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    requests[0].1.clone()
}

fn user_texts(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn notices(app: &AgentsWindow) -> Vec<&str> {
    app.view
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Notice { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn typed(app: &AgentsWindow) -> Vec<&str> {
    app.view
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_root_paused_before_any_response_sends_its_own_prompt_to_the_new_model() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = switch_app(root.path(), &fixture.endpoint);
    let session = paused(&app, root.path(), false, &[]);
    open(&mut app, &session);
    app.view.prompt = "BORRADOR_SIN_ENVIAR".into();
    let before = journal(&app, &session);

    // The pause card opens the picker; picking a route asks first.
    click(&mut app, &format!("pause_model:{session}"));
    assert!(app.view.composer_model_menu_open);
    let route = other_route(&mut app, "custom");
    assert!(app.select_composer_route(&route));
    let confirm = app.view.model_switch.confirm.clone().expect("a confirmation");
    assert_eq!((confirm.moving, confirm.working, confirm.child, &confirm.blocker), (1, 0, false, &None));
    assert_eq!(journal(&app, &session), before, "asking changes nothing");
    assert_eq!(app.view.model, MODEL);
    assert!(crate::view::dialog_open(&app.view, 900.0));

    click(&mut app, "model_switch_confirm");
    let closed = journal(&app, &session);
    assert_eq!(&closed[..before.len()], before.as_slice());
    let AgentEventKind::TurnSuperseded { model_id, next_turn_id, host_prompt, .. } =
        closed.last().unwrap().kind.clone()
    else {
        panic!("{:?}", closed.last());
    };
    assert_eq!((model_id.as_str(), host_prompt), (OTHER, false));
    assert_eq!(app.view.model, OTHER);
    assert_eq!(
        app.switch_queue,
        [SwitchStep::StartRoot { session_id: session.clone(), drop_images: false }]
    );

    let request = run_continuation(&mut app, &fixture);
    assert_eq!(request["model"], OTHER);
    assert_eq!(user_texts(&request), [TASK], "the prompt once, and no host text");
    let events = journal(&app, &session);
    assert!(events.iter().any(|event| matches!(&event.kind,
        AgentEventKind::TurnAdmitted { turn_id, model, prompt, .. }
            if turn_id == &next_turn_id && model == OTHER && prompt == TASK)));
    assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCompleted { .. }));
    // The person sees their message once and a notice, and keeps their draft.
    assert_eq!(typed(&app), [TASK]);
    assert!(notices(&app).contains(&format!("Continúa en {OTHER} desde donde quedó").as_str()));
    assert_eq!(app.view.prompt, "BORRADOR_SIN_ENVIAR");
    app.refresh_sessions();
    assert!(app.view.model_switch.unfinished.is_empty() && app.view.pause.sessions.is_empty());
}

#[test]
fn a_root_with_committed_work_continues_from_it_and_never_repeats_a_tool() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = switch_app(root.path(), &fixture.endpoint);
    let session = paused(&app, root.path(), true, &[]);
    open(&mut app, &session);
    let route = other_route(&mut app, "custom");
    assert!(app.select_composer_route(&route));
    click(&mut app, "model_switch_confirm");
    let tools_before = journal(&app, &session)
        .iter()
        .filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. }))
        .count();
    assert!(matches!(
        journal(&app, &session).last().unwrap().kind,
        AgentEventKind::TurnSuperseded { host_prompt: true, .. }
    ));

    let request = run_continuation(&mut app, &fixture);
    assert_eq!(request["model"], OTHER);
    assert_eq!(user_texts(&request), [TASK, openide_harness::MODEL_SWITCH_CONTINUATION]);
    let sent = request.to_string();
    assert!(sent.contains("RESULTADO_YA_OBTENIDO"), "the finished work reaches the new model");
    let events = journal(&app, &session);
    assert_eq!(
        events.iter().filter(|event| matches!(event.kind, AgentEventKind::ToolStarted { .. })).count(),
        tools_before,
        "no tool ran again"
    );
    // Host-authored: a notice, never a message of the person's.
    assert_eq!(typed(&app), [TASK]);
    assert!(notices(&app).contains(&format!("Continúa en {OTHER} desde donde quedó").as_str()));
    app.sync_message_edit_turns();
    assert_eq!(app.view.message_edit.turns.len(), 1, "only the typed message is editable");
    // Reloaded from disk it reads the same.
    let reloaded = TranscriptProjection::from_events(&events);
    assert_eq!(reloaded.user_turns().len(), 1);
}

#[test]
fn a_switch_cut_after_closing_the_turn_is_finished_from_the_journal() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let session = {
        let app = switch_app(root.path(), &fixture.endpoint);
        let session = paused(&app, root.path(), true, &[]);
        // The app stops right after the terminal event is on disk.
        let store = app.store.as_ref().unwrap();
        let lease = store.try_acquire_native_lease(&session).unwrap();
        let checkpoints = CheckpointFileStore::new(app.data_root.join("sessions")).unwrap();
        openide_harness::supersede_paused_turn(store, &checkpoints, &lease, "custom", OTHER, "next-turn")
            .unwrap();
        session
    };
    let mut app = switch_app(root.path(), &fixture.endpoint);
    open(&mut app, &session);
    // Neither paused nor silently idle: the open switch is on screen.
    assert!(!app.view.pause.sessions.contains_key(&session));
    assert_eq!(app.view.model_switch.unfinished[&session].model_id, OTHER);
    assert!(app.view.model_switch.unfinished_card(&session).is_some());

    // The model is not offered right now: nothing is lost, it says why.
    click(&mut app, &format!("model_switch_finish:{session}"));
    assert!(app.switch_queue.is_empty());
    assert!(app.view.model_switch.unfinished[&session].error.is_some());
    assert!(matches!(journal(&app, &session).last().unwrap().kind, AgentEventKind::TurnSuperseded { .. }));

    other_route(&mut app, "custom");
    click(&mut app, &format!("model_switch_finish:{session}"));
    let request = run_continuation(&mut app, &fixture);
    assert_eq!(request["model"], OTHER);
    assert!(journal(&app, &session).iter().any(|event| matches!(&event.kind,
        AgentEventKind::TurnAdmitted { turn_id, .. } if turn_id == "next-turn")));
    app.refresh_sessions();
    assert!(app.view.model_switch.unfinished.is_empty());
}

#[test]
fn a_blocker_is_shown_before_anything_is_written_or_sent() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = switch_app(root.path(), &fixture.endpoint);
    let image = UserImage::from_bytes(UserImageMime::Png, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let session = paused(&app, root.path(), false, &[image]);
    open(&mut app, &session);
    let before = journal(&app, &session);

    // A provider that is not connected cannot take the turn at all.
    let unconnected = other_route(&mut app, "anthropic");
    assert!(app.select_composer_route(&unconnected));
    assert!(matches!(
        app.view.model_switch.confirm.as_ref().unwrap().blocker,
        Some(SwitchBlocker::Refused(_))
    ));
    click(&mut app, "model_switch_confirm");
    assert_eq!(journal(&app, &session), before);

    // This route cannot see the image of the message it would send again.
    let route = other_route(&mut app, "custom");
    assert!(app.select_composer_route(&route));
    assert_eq!(
        app.view.model_switch.confirm.as_ref().unwrap().blocker,
        Some(SwitchBlocker::Images { count: 1 })
    );
    click(&mut app, "model_switch_other");
    assert!(app.view.model_switch.confirm.is_none() && app.view.composer_model_menu_open);
    assert_eq!(journal(&app, &session), before, "choosing another model changes nothing");
    assert!(fixture.requests.lock().unwrap().is_empty());

    assert!(app.select_composer_route(&route));
    click(&mut app, "model_switch_without_images");
    let request = run_continuation(&mut app, &fixture);
    assert_eq!(user_texts(&request), [TASK]);
    assert!(journal(&app, &session).iter().any(|event| matches!(&event.kind,
        AgentEventKind::TurnAdmitted { model, .. } if model == OTHER)));
}

/// A provider that refuses every request with a reached limit.
struct Limited;

#[async_trait::async_trait]
impl openide_harness::ModelBackend for Limited {
    fn model_id(&self) -> &str {
        MODEL
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([7; 32])
    }
    async fn complete(
        &self,
        messages: &[AgentMessage],
        _tools: &[openide_harness::ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<openide_harness::ModelOutput, openide_harness::AgentError> {
        if messages.iter().any(|message| message.content == "Terminá enseguida") {
            return Ok(openide_harness::ModelOutput { content: "listo".into(), tool_calls: vec![] });
        }
        Err(openide_harness::AgentError::Provider(openide_harness::ProviderFailure::new(
            openide_harness::FailureClass::LimitReached,
            "HTTP 429: usage_limit_reached",
        )))
    }
}

#[test]
fn switching_at_the_root_moves_the_paused_tree_and_leaves_finished_agents_alone() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = switch_app(root.path(), &fixture.endpoint);
    let parent = paused(&app, root.path(), true, &[]);
    let store = app.store.clone().unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap());
    let worker = openide_harness::SubagentWorker::new(
        coordinator.clone(),
        store.clone(),
        Arc::new(Limited),
        Arc::new(openide_harness::LocalTools::new()),
        "custom",
        fixture.endpoint.clone(),
    );
    let spawn = |spawn_id: &str, task: &str| {
        let lease = Arc::new(store.try_acquire_native_lease(&parent).unwrap());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(worker.run(lease, spawn_id.into(), task.into(), TurnMode::Build, CancellationToken::new()))
            .unwrap()
    };
    let held = spawn("spawn-held", "Migrá facturas");
    let done = spawn("spawn-done", "Terminá enseguida");
    assert_eq!(held.status, SubagentStatus::Interrupted);
    assert_eq!(done.status, SubagentStatus::Completed);
    let (held, done) = (held.child_session_id, done.child_session_id);
    let done_before = store.load(&done).unwrap().unwrap();
    let held_before = store.load(&held).unwrap().unwrap();

    open(&mut app, &parent);
    let route = other_route(&mut app, "custom");
    assert!(app.select_composer_route(&route));
    assert_eq!(app.view.model_switch.confirm.as_ref().unwrap().moving, 2, "the root and its paused child");
    click(&mut app, "model_switch_confirm");

    // The paused child moved: its turn is closed for the new model and its
    // identity is the new route's. It continues before the root does.
    let moved = store.load(&held).unwrap().unwrap();
    assert_eq!(&moved.events[..held_before.events.len()], held_before.events.as_slice());
    assert!(
        matches!(&moved.events.last().unwrap().kind,
            AgentEventKind::TurnSuperseded { model_id, host_prompt: false, .. } if model_id == OTHER),
        "{}",
        app.view.status
    );
    assert_ne!(
        moved.session.subagent.as_ref().unwrap().request_digest,
        held_before.session.subagent.as_ref().unwrap().request_digest
    );
    assert_eq!(
        app.switch_queue,
        [
            SwitchStep::ResumeChild(held.clone()),
            SwitchStep::StartRoot { session_id: parent.clone(), drop_images: false },
        ]
    );
    // The finished child is exactly as it was.
    assert_eq!(store.load(&done).unwrap().unwrap(), done_before);
    let lease = store.try_acquire_native_lease(&parent).unwrap();
    let children = coordinator.list_children(&lease).unwrap();
    let status = |id: &str| children.iter().find(|child| child.session.id == id).unwrap().status;
    assert_eq!((status(&held), status(&done)), (SubagentStatus::Interrupted, SubagentStatus::Completed));
    assert!(matches!(journal(&app, &parent).last().unwrap().kind, AgentEventKind::TurnSuperseded { .. }));
}

#[test]
fn a_paused_subagent_switches_alone_from_its_own_view() {
    let root = tempfile::tempdir().unwrap();
    let fixture = StreamingHttpFixture::new(FIRST_VISIBLE.into(), SECOND_VISIBLE.into());
    let mut app = switch_app(root.path(), &fixture.endpoint);
    let parent = paused(&app, root.path(), true, &[]);
    let store = app.store.clone().unwrap();
    let coordinator =
        Arc::new(SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap());
    let worker = openide_harness::SubagentWorker::new(
        coordinator,
        store.clone(),
        Arc::new(Limited),
        Arc::new(openide_harness::LocalTools::new()),
        "custom",
        fixture.endpoint.clone(),
    );
    let child = {
        let lease = Arc::new(store.try_acquire_native_lease(&parent).unwrap());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(worker.run(lease, "spawn-1".into(), "Migrá facturas".into(), TurnMode::Build, CancellationToken::new()))
            .unwrap()
            .child_session_id
    };
    let parent_before = journal(&app, &parent);
    open(&mut app, &child);
    let route = other_route(&mut app, "custom");
    assert!(app.select_composer_route(&route));
    let confirm = app.view.model_switch.confirm.clone().unwrap();
    assert!(confirm.child && confirm.moving == 1);
    click(&mut app, "model_switch_confirm");
    assert!(
        matches!(journal(&app, &child).last().unwrap().kind, AgentEventKind::TurnSuperseded { .. }),
        "{}",
        app.view.status
    );
    assert_eq!(app.switch_queue, [SwitchStep::ResumeChild(child.clone())]);
    assert_eq!(journal(&app, &parent), parent_before, "the parent keeps its own turn and model");
}

#[test]
fn a_route_without_a_declared_window_is_budgeted_from_what_is_known() {
    let root = tempfile::tempdir().unwrap();
    let app = switch_app(root.path(), "http://127.0.0.1:9/v1");
    let window = |app: &AgentsWindow, provider: &str, model: &str| {
        let adapter = native_provider::adapter_for_model_at(&app.data_root, provider, model)
            .unwrap()
            .unwrap();
        app.native_budget_with_window(&adapter, provider, model)
            .map(|(budget, source)| (budget.context_window_tokens, budget.output_reserve_tokens, source))
    };
    // Nothing known: 128k, with a fifth of it reserved for the answer.
    assert_eq!(
        window(&app, "custom", "nobody/knows-this-model"),
        Some((ASSUMED_CONTEXT_WINDOW, ASSUMED_CONTEXT_WINDOW / 5, ContextWindowSource::Assumed))
    );
    // The bundled catalog knows better than the default.
    let listed = crate::model_metadata::native_route("codex", "gpt-6-luna", None, &[])
        .context_tokens
        .expect("the bundled catalog lists this subscription model");
    let (catalog_window, _, source) = window(&app, "codex", "gpt-6-luna").unwrap();
    assert_eq!((catalog_window, source), (listed, ContextWindowSource::Catalog));
    assert_ne!(listed, ASSUMED_CONTEXT_WINDOW);
    // What the provider stated when it refused a request beats both, and
    // the same turn path that had no budget now compacts against it.
    native_provider::save_learned_context_limit(&app.data_root, "custom", "nobody/knows-this-model", 32_000).unwrap();
    native_provider::save_learned_context_limit(&app.data_root, "codex", "gpt-6-luna", 200_000).unwrap();
    assert_eq!(
        window(&app, "custom", "nobody/knows-this-model"),
        Some((32_000, 6_400, ContextWindowSource::Learned))
    );
    assert_eq!(window(&app, "codex", "gpt-6-luna").map(|known| (known.0, known.2)), Some((200_000, ContextWindowSource::Learned)));
    assert!(app.token_budget_for_native_model("codex", "gpt-6-luna").is_some());
    // It survives a restart.
    let reopened = AgentsWindow::with_data_root(root.path().join("data"));
    assert_eq!(
        native_provider::learned_context_limit(&reopened.data_root, "custom", "nobody/knows-this-model"),
        Some(32_000)
    );
    // A history past the model's threshold is announced before the switch.
    let big = vec![AgentMessage::user("x".repeat(120_000))];
    assert!(app.history_needs_summary(&big, "custom", "nobody/knows-this-model"));
    assert!(!app.history_needs_summary(&[AgentMessage::user("hola")], "custom", "nobody/knows-this-model"));
}

#[test]
fn the_provider_stated_limit_is_remembered_when_its_notice_arrives() {
    let root = tempfile::tempdir().unwrap();
    let mut app = switch_app(root.path(), "http://127.0.0.1:9/v1");
    let session = app.store.as_ref().unwrap().create(root.path(), "native").unwrap();
    app.open_session(&session.id);
    let notice = |code: &str, detail: &str| AgentEventKind::HostNotice { code: code.into(), detail: detail.into() };
    let mut projection = TranscriptProjection::default();
    for (code, detail, expected) in [
        (openide_core::NOTICE_CONTEXT_SUMMARIZED, "Luna", "La conversación no entraba en Luna: se resumió antes de continuar"),
        (openide_core::NOTICE_CONTEXT_TRIMMED, "Luna", "La conversación no entraba en Luna: se recortaron salidas de herramientas antiguas"),
    ] {
        projection.apply_kind(&notice(code, detail));
        assert!(matches!(projection.items().last(),
            Some(TranscriptItem::Notice { message, is_error: false }) if message == expected));
    }
    assert_eq!(native_provider::learned_context_limit(&app.data_root, "custom", MODEL), None);
    app.process_event(AgentEvent::new(&session.id, 1, notice(openide_core::NOTICE_CONTEXT_LIMIT, "64000")));
    assert_eq!(native_provider::learned_context_limit(&app.data_root, "custom", MODEL), Some(64_000));
}
