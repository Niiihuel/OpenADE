//! Paused turns continuing off screen, through the real window, the real
//! runner and a loopback provider that refuses with a reached limit. Every
//! pause here is one the runner journaled itself, unless stated otherwise.

use super::*;
use crate::automations::{AutomationDraft, AutomationSchedule};
use crate::native_provider::{DiscoveredModel, ProviderModelDiscovery, ProviderRegistry};
use crate::notifications::Target;
use crate::pause::{wall_clock_ms, AutoResume};
use openide_core::TurnPauseClass;
use openide_harness::LocalTool;
use std::sync::atomic::AtomicUsize;

const OTHER: &str = "streaming/model:other";
const TASK: &str = "Migrá los tests";
const CHILD_TASK: &str = "Migrá las facturas";
const DRAFT: &str = "BORRADOR_SIN_ENVIAR";
const CONTINUES: &str = "Límite renovado: el turno continúa";
const MINUTE: u64 = 60_000;

enum Reply {
    /// HTTP 429 for a usage limit that renewed a minute ago: the pause is
    /// real and its continuation is due at once.
    Limit,
    Text(&'static str),
    Tool(&'static str, Value),
}

/// A provider that answers each request as the script says.
struct ScriptedProvider {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedProvider {
    fn new(script: impl Fn(&Value) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopping) = (requests.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            let mut calls = 0;
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("scripted provider accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let (_, request) = read_request(&mut stream).unwrap();
                let reply = script(&request);
                seen.lock().unwrap().push(request);
                let (status, kind, body) = match reply {
                    Reply::Limit => (
                        "429 Too Many Requests",
                        "application/json",
                        json!({"error": {
                            "type": "usage_limit_reached",
                            "message": "usage limit reached",
                            "resets_at": wall_clock_ms() - MINUTE,
                        }})
                        .to_string(),
                    ),
                    reply => {
                        let (delta, finish) = match reply {
                            Reply::Tool(name, arguments) => {
                                calls += 1;
                                (
                                    json!({"tool_calls": [{"index": 0, "id": format!("call-{calls}"), "type": "function",
                                        "function": {"name": name, "arguments": arguments.to_string()}}]}),
                                    "tool_calls",
                                )
                            }
                            Reply::Text(text) => (json!({"content": text}), "stop"),
                            Reply::Limit => unreachable!(),
                        };
                        (
                            "200 OK",
                            "text/event-stream",
                            format!(
                                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                                json!({"id": "scripted", "choices": [{"index": 0, "delta": delta, "finish_reason": null}]}),
                                json!({"id": "scripted", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}),
                            ),
                        )
                    }
                };
                // A worker cancelled by the test may have hung up already.
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        });
        Self { endpoint, requests, stop, worker: Some(worker) }
    }

    /// Refuses the first `limited` requests, then answers `text`.
    fn limited(limited: usize, text: &'static str) -> Self {
        let left = AtomicUsize::new(limited);
        Self::new(move |_| {
            if left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1)).is_ok() {
                Reply::Limit
            } else {
                Reply::Text(text)
            }
        })
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn app_at(root: &Path, endpoint: &str) -> AgentsWindow {
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
    app.view.pause.auto_enabled = true;
    app
}

/// Drive the window's own polls until `done`.
fn settle(app: &mut AgentsWindow, mut done: impl FnMut(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        app.poll_run();
        app.poll_subagent_events();
        app.poll_background_resumes();
        if done(app) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out: {}", app.view.status);
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn journal(app: &AgentsWindow, session_id: &str) -> Vec<AgentEvent> {
    app.store.as_ref().unwrap().load(session_id).unwrap().unwrap().events
}

fn count(events: &[AgentEvent], kind: impl Fn(&AgentEventKind) -> bool) -> usize {
    events.iter().filter(|event| kind(&event.kind)).count()
}

/// A real turn the provider refuses: the runner pauses it and journals the
/// route, catalog, policy and context a pause has.
fn paused_turn(app: &mut AgentsWindow, root: &Path) -> String {
    let session = app.store.as_ref().unwrap().create(root, "native").unwrap();
    app.open_session(&session.id);
    native_frame(app);
    app.view.turn_mode = TurnMode::Build;
    app.view.prompt = TASK.into();
    app.start_run();
    assert!(matches!(app.active_run, Some(ActiveRun::Native(_))), "{}", app.view.status);
    settle(app, |app| app.active_run.is_none());
    assert!(
        matches!(journal(app, &session.id).last().unwrap().kind, AgentEventKind::TurnPaused { .. }),
        "{}",
        app.view.status
    );
    session.id
}

/// A pause written by hand, as `pause_app_tests` does: no route was ever
/// committed for it.
fn handwritten_pause(
    app: &AgentsWindow,
    root: &Path,
    class: TurnPauseClass,
    paused_ago_ms: u64,
    resets_at_ms: Option<u64>,
) -> String {
    let store = app.store.as_ref().unwrap();
    let session = store.create(root, "native").unwrap();
    let admitted = store
        .admit_native_turn(&session.id, "turn-1", TASK, MODEL, TurnMode::Build)
        .unwrap();
    for (offset, kind) in [
        AgentEventKind::TurnStarted { prompt: TASK.into() },
        AgentEventKind::ModelRequestStarted { step: 1, model: MODEL.into() },
        AgentEventKind::TurnPaused {
            class,
            limit_key: "background-resume-test".into(),
            resets_at_ms,
            step: 1,
            detail: "HTTP 429: usage_limit_reached".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = AgentEvent::new(&session.id, admitted.sequence + 1 + offset as u64, kind);
        event.timestamp_ms = wall_clock_ms() - paused_ago_ms;
        store.append_native_event(&event).unwrap();
    }
    session.id
}

/// Another conversation takes the screen: another model in its composer and
/// a draft being typed.
fn look_elsewhere(app: &mut AgentsWindow, root: &Path) -> String {
    let other = app.store.as_ref().unwrap().create(root, "native").unwrap();
    app.open_session(&other.id);
    app.view.model = OTHER.into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    app.view.prompt = DRAFT.into();
    tick(app);
    other.id
}

/// What a frame's clock does before arming, without waiting for a second.
fn tick(app: &mut AgentsWindow) {
    app.refresh_sessions();
    app.view.pause.clock_ms = wall_clock_ms();
}

/// What the person was told about `session`, newest first.
fn told(app: &AgentsWindow, session: &str) -> Vec<(String, u32)> {
    app.view
        .notifications
        .items()
        .iter()
        .filter(|item| item.target == Some(Target::Session(session.to_owned())))
        .map(|item| (item.title.clone(), item.count))
        .collect()
}

/// The screen as the person left it: their conversation, model and draft.
fn assert_screen_untouched(app: &AgentsWindow, on_screen: &str) {
    assert_eq!(app.view.selected_session.as_deref(), Some(on_screen));
    assert_eq!(app.view.prompt, DRAFT);
    assert_eq!((app.view.native_provider_id.as_str(), app.view.model.as_str()), ("custom", OTHER));
    assert!(!app.view.running && !app.view.background_run_selected);
    assert!(app.active_run.is_none() && app.active_resume.is_none() && app.active_child_recovery.is_none());
    assert!(journal(app, on_screen).is_empty(), "nothing was sent for the conversation on screen");
}

#[test]
fn a_paused_conversation_off_screen_continues_on_its_own_model_and_leaves_the_screen_alone() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(1, "LISTO");
    let mut app = app_at(root.path(), &provider.endpoint);
    let paused = paused_turn(&mut app, root.path());
    let on_screen = look_elsewhere(&mut app, root.path());
    // The composer's effort is not the paused turn's either.
    app.view.composer_effort = "high".into();
    let status = app.view.status.clone();
    assert!(matches!(
        app.view.pause.plan(&paused, &app.view.pause.sessions[&paused]),
        AutoResume::At(due) if app.view.pause.clock_ms >= due
    ));

    assert!(app.arm_auto_resume());
    assert_eq!(app.background_resumes.len(), 1, "{:?}", told(&app, &paused));
    assert!(app.pause_resume_queue.is_empty(), "the window's own turn is not taken");
    assert_eq!(app.view.status, status);
    assert_screen_untouched(&app, &on_screen);
    // While it continues it is neither paused nor free for another turn.
    assert!(!app.view.pause.sessions.contains_key(&paused));
    assert!(!app.view.session_recovery.contains_key(&paused));
    assert_eq!(app.off_screen_busy(&paused), Some("Espera a que termine la continuación automática"));
    assert_eq!(app.off_screen_busy(&on_screen), None);
    tick(&mut app);
    assert!(!app.arm_auto_resume(), "armed once");

    settle(&mut app, |app| app.background_resumes.is_empty());
    let requests = provider.requests();
    assert_eq!(requests.len(), 2, "the refused request and its one continuation");
    assert_eq!(requests[1]["model"], MODEL, "the paused turn's own model, not the composer's");
    let events = journal(&app, &paused);
    assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCompleted { .. }));
    for (what, times) in [
        ("admitted", count(&events, |kind| matches!(kind, AgentEventKind::TurnAdmitted { .. }))),
        ("paused", count(&events, |kind| matches!(kind, AgentEventKind::TurnPaused { .. }))),
        ("completed", count(&events, |kind| matches!(kind, AgentEventKind::TurnCompleted { .. }))),
    ] {
        assert_eq!(times, 1, "{what}");
    }
    assert_eq!(app.view.composer_effort, "high");
    assert_screen_untouched(&app, &on_screen);
    assert_eq!(
        told(&app, &paused),
        [("Turno completado".to_owned(), 1), (CONTINUES.to_owned(), 1)]
    );
    assert!(told(&app, &on_screen).is_empty());

    // Nothing is left to continue, and nothing runs twice.
    tick(&mut app);
    assert!(!app.arm_auto_resume() && app.background_resumes.is_empty());
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(app.off_screen_busy(&paused), None);
}

#[test]
fn two_workers_at_most_continue_off_screen_and_the_rest_stay_armed() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(3, "LISTO");
    let mut app = app_at(root.path(), &provider.endpoint);
    let paused: Vec<String> = (0..3).map(|_| paused_turn(&mut app, root.path())).collect();
    let on_screen = look_elsewhere(&mut app, root.path());

    assert!(app.arm_auto_resume());
    assert_eq!(app.background_resumes.len(), 2);
    assert_eq!(app.view.pause.auto_attempted.len(), 2, "the third was not attempted, only kept waiting");
    let waiting = paused.iter().find(|id| app.off_screen_busy(id).is_none()).unwrap().clone();
    assert!(matches!(
        app.view.pause.plan(&waiting, &app.view.pause.sessions[&waiting]),
        AutoResume::At(_)
    ));
    tick(&mut app);
    assert!(!app.arm_auto_resume() && app.background_resumes.len() == 2);

    settle(&mut app, |app| app.background_resumes.is_empty());
    tick(&mut app);
    assert!(app.arm_auto_resume());
    assert_eq!(app.off_screen_busy(&waiting), Some("Espera a que termine la continuación automática"));
    settle(&mut app, |app| app.background_resumes.is_empty());
    for id in &paused {
        let events = journal(&app, id);
        assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::TurnCompleted { .. })), 1, "{id}");
    }
    assert_eq!(provider.requests().len(), 6, "three refusals and one continuation each");
    assert_screen_untouched(&app, &on_screen);
}

#[test]
fn a_conversation_opened_while_it_continues_reads_as_running_and_takes_no_second_turn() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(1, "LISTO");
    let mut app = app_at(root.path(), &provider.endpoint);
    let paused = paused_turn(&mut app, root.path());
    look_elsewhere(&mut app, root.path());
    assert!(app.arm_auto_resume());
    // Its worker is asking the model: what it prepares first takes its time.
    let asking = Instant::now() + Duration::from_secs(10);
    while provider.requests().len() < 2 {
        assert!(Instant::now() < asking, "the worker never asked the model");
        std::thread::sleep(Duration::from_millis(1));
    }

    // The person goes to look at it before its worker is through.
    app.open_session(&paused);
    assert!(app.view.background_run_selected && !app.view.running);
    assert_eq!(app.view.status, "El turno en pausa continúa");
    app.view.prompt = "Otro mensaje".into();
    app.start_run();
    assert_eq!(app.view.status, "Espera a que termine la continuación automática de esta sesión");
    app.start_resume();
    assert_eq!(app.view.status, "Espera a que termine la continuación automática");
    assert!(app.active_run.is_none() && app.active_resume.is_none());
    assert_eq!(
        app.session_deletion_blocked_reason(&paused),
        Some("Espera a que termine la continuación automática antes de eliminar esta tarea")
    );
    // Its stop button stops that worker, quietly.
    assert!(app.cancel_selected_run());
    settle(&mut app, |app| app.background_resumes.is_empty());
    assert!(!app.view.background_run_selected);
    let events = journal(&app, &paused);
    assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::TurnAdmitted { .. })), 1);
    assert_eq!(told(&app, &paused), [(CONTINUES.to_owned(), 1)]);
    assert_eq!(provider.requests().len(), 2);
}

#[test]
fn a_pause_that_is_off_cancelled_asking_or_manual_never_continues_off_screen() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(0, "NUNCA");
    let mut app = app_at(root.path(), &provider.endpoint);
    let now = wall_clock_ms();
    let limit = TurnPauseClass::LimitReached;
    let renewed = Some(now - MINUTE);

    // Cancelled with Esc while it was on screen.
    let cancelled = handwritten_pause(&app, root.path(), limit, 10 * MINUTE, renewed);
    app.open_session(&cancelled);
    tick(&mut app);
    assert!(app.cancel_auto_resume());
    for (class, paused_ago_ms, resets_at_ms) in [
        // Renewed over half an hour ago: it asks.
        (limit, 3 * 60 * MINUTE, Some(now - 31 * MINUTE)),
        // Not renewed yet, no known reset, a reset more than a day away.
        (limit, MINUTE, Some(now + 10 * MINUTE)),
        (limit, MINUTE, None),
        (limit, 25 * 60 * MINUTE, renewed),
        // Causes the clock does not fix.
        (TurnPauseClass::OutOfCredits, 10 * MINUTE, renewed),
        (TurnPauseClass::Auth, 10 * MINUTE, renewed),
        (TurnPauseClass::Transient, 10 * MINUTE, renewed),
    ] {
        handwritten_pause(&app, root.path(), class, paused_ago_ms, resets_at_ms);
    }
    let on_screen = look_elsewhere(&mut app, root.path());
    assert_eq!(app.view.pause.sessions.len(), 8);
    assert!(!app.arm_auto_resume());

    // Due and armed, with the setting off.
    let due = handwritten_pause(&app, root.path(), limit, 10 * MINUTE, renewed);
    app.view.pause.auto_enabled = false;
    tick(&mut app);
    assert!(!app.arm_auto_resume());
    assert!(app.background_resumes.is_empty() && app.view.pause.auto_attempted.is_empty());
    assert!(app.view.notifications.items().is_empty());
    assert_screen_untouched(&app, &on_screen);

    // With it on, that one is the clock's, and only that one. Its journal
    // names no route, so there is nothing of its own to continue it on:
    // the person is told once and it stays paused for them.
    app.view.pause.auto_enabled = true;
    let before = journal(&app, &due);
    assert!(app.arm_auto_resume());
    assert!(app.background_resumes.is_empty());
    assert_eq!(app.view.notifications.items().len(), 1);
    assert_eq!(
        told(&app, &due),
        [("No se pudo continuar el turno en pausa: El turno no registró la ruta de su modelo".to_owned(), 1)]
    );
    for _ in 0..3 {
        tick(&mut app);
        assert!(!app.arm_auto_resume(), "one attempt per pause");
    }
    assert_eq!(told(&app, &due).len(), 1);
    assert_eq!(journal(&app, &due), before);
    assert_eq!(app.view.pause.plan(&due, &app.view.pause.sessions[&due]), AutoResume::Manual);
    assert_screen_untouched(&app, &on_screen);
    assert!(provider.requests().is_empty());
}

#[test]
fn a_route_changed_since_the_pause_is_told_once_and_sends_nothing() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(1, "NUNCA");
    let mut app = app_at(root.path(), &provider.endpoint);
    let paused = paused_turn(&mut app, root.path());
    let before = journal(&app, &paused);
    let on_screen = look_elsewhere(&mut app, root.path());
    // The provider's endpoint was edited in Settings since the turn paused.
    app.view.endpoint = "http://127.0.0.1:9/v1".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);

    assert!(app.arm_auto_resume());
    assert!(app.background_resumes.is_empty());
    assert_eq!(
        told(&app, &paused),
        [(
            "No se pudo continuar el turno en pausa: La ruta del modelo cambió desde que el turno quedó en pausa"
                .to_owned(),
            1
        )]
    );
    tick(&mut app);
    assert!(!app.arm_auto_resume());
    assert_eq!(told(&app, &paused).len(), 1);
    assert_eq!(journal(&app, &paused), before, "still paused, for a manual continue");
    assert!(app.view.pause.sessions.contains_key(&paused));
    assert_eq!(provider.requests().len(), 1, "only the request that was refused");
    assert_screen_untouched(&app, &on_screen);
}

#[test]
fn a_continuation_refused_again_waits_for_the_next_renewal_within_the_cap() {
    let root = tempfile::tempdir().unwrap();
    // The provider keeps refusing: every continuation pauses again.
    let provider = ScriptedProvider::limited(usize::MAX, "NUNCA");
    let mut app = app_at(root.path(), &provider.endpoint);
    let paused = paused_turn(&mut app, root.path());
    let on_screen = look_elsewhere(&mut app, root.path());

    let mut continued = 0;
    while app.arm_auto_resume() {
        continued += 1;
        assert!(continued <= 8, "the clock never stops re-arming");
        settle(&mut app, |app| app.background_resumes.is_empty());
        tick(&mut app);
    }
    let pause = &app.view.pause.sessions[&paused];
    assert_eq!(pause.pauses_in_turn as usize, continued + 1);
    assert_eq!(app.view.pause.plan(&paused, pause), AutoResume::Manual, "the re-arm cap");
    assert_eq!(provider.requests().len(), continued + 1);
    let events = journal(&app, &paused);
    assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnPaused { .. }));
    assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::TurnAdmitted { .. })), 1);
    // Told that it continued each time, and nothing else: no failure.
    assert_eq!(told(&app, &paused), [(CONTINUES.to_owned(), continued as u32)]);
    assert_screen_untouched(&app, &on_screen);
}

#[test]
fn a_continuation_that_needs_a_decision_stops_there_and_answers_nothing() {
    let root = tempfile::tempdir().unwrap();
    let asked = AtomicBool::new(false);
    let provider = ScriptedProvider::new(move |request| {
        if !asked.swap(true, Ordering::SeqCst) {
            Reply::Limit
        } else if request["messages"].as_array().unwrap().iter().any(|message| message["role"] == "tool") {
            Reply::Text("NUNCA")
        } else {
            Reply::Tool("write_file", json!({"path": "nuevo.txt", "content": "x"}))
        }
    });
    let mut app = app_at(root.path(), &provider.endpoint);
    let mut permissions = AgentToolSettings::default();
    permissions.set(LocalTool::WriteFile, true);
    assert!(app.commit_agent_tool_settings(permissions, "Writing asks first"));
    let paused = paused_turn(&mut app, root.path());
    let on_screen = look_elsewhere(&mut app, root.path());

    assert!(app.arm_auto_resume());
    settle(&mut app, |app| app.background_resumes.is_empty());
    let events = journal(&app, &paused);
    assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::PermissionRequested { .. })), 1);
    assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::PermissionResolved { .. })), 0);
    assert!(!root.path().join("nuevo.txt").exists());
    assert_eq!(app.view.session_recovery.get(&paused), Some(&SessionRecoveryView::Permission));
    assert_eq!(
        told(&app, &paused),
        [("El turno continuó y espera tu decisión".to_owned(), 1), (CONTINUES.to_owned(), 1)]
    );
    assert_eq!(provider.requests().len(), 2);
    assert_screen_untouched(&app, &on_screen);
    tick(&mut app);
    assert!(!app.arm_auto_resume());
}

#[test]
fn a_paused_tree_off_screen_continues_its_subagent_before_its_root() {
    let root = tempfile::tempdir().unwrap();
    let limited = Arc::new(AtomicBool::new(true));
    let refusing = limited.clone();
    let provider = ScriptedProvider::new(move |request| {
        let messages = request["messages"].as_array().unwrap();
        let results: Vec<&Value> = messages.iter().filter(|message| message["role"] == "tool").collect();
        if messages.iter().any(|message| message["role"] == "user" && message["content"] == CHILD_TASK) {
            // The subagent's own conversation.
            if refusing.load(Ordering::SeqCst) { Reply::Limit } else { Reply::Text("HIJO_LISTO") }
        } else if results.is_empty() {
            Reply::Tool("spawn_agent", json!({"prompt": CHILD_TASK}))
        } else if results.len() == 1 {
            let spawned: Value = serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
            Reply::Tool("wait_agent", json!({"agent_id": spawned["agent_id"]}))
        } else {
            Reply::Text("RAIZ_LISTA")
        }
    });
    let mut app = app_at(root.path(), &provider.endpoint);
    let parent = paused_turn(&mut app, root.path());
    let on_screen = look_elsewhere(&mut app, root.path());
    // The subagent hit the limit; its parent waits for it, both paused.
    let (child, waiting) = app
        .view
        .pause
        .sessions
        .iter()
        .find(|(_, pause)| pause.parent.as_deref() == Some(parent.as_str()))
        .map(|(id, _)| (id.clone(), app.view.pause.sessions[&parent].class))
        .expect("a paused subagent");
    assert_eq!(waiting, TurnPauseClass::WaitingOnChild);
    assert_eq!(
        crate::pause::resume_order(&app.view.pause.sessions, &parent),
        [child.clone(), parent.clone()]
    );
    let sent_before = provider.requests().len();
    limited.store(false, Ordering::SeqCst);

    assert!(app.arm_auto_resume());
    assert_eq!(app.background_resumes.len(), 1, "one tree, one worker at a time");
    assert!(app.background_resumes[0].holds(&child) && app.background_resumes[0].holds(&parent));
    assert!(app.view.pause.sessions.is_empty(), "the whole tree continues");
    settle(&mut app, |app| app.background_resumes.is_empty());

    // Leaves first: the subagent was asked, and only then its parent.
    let requests = provider.requests();
    let is_child = |request: &Value| {
        request["messages"].as_array().unwrap().iter().any(|message| message["content"] == CHILD_TASK)
    };
    assert_eq!(requests.len(), sent_before + 2, "one request each");
    assert!(is_child(&requests[sent_before]) && !is_child(&requests[sent_before + 1]));
    for id in [&child, &parent] {
        let events = journal(&app, id);
        assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCompleted { .. }), "{id}");
        assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::TurnCompleted { .. })), 1, "{id}");
        assert_eq!(requests.iter().filter(|request| request["model"] == MODEL).count(), requests.len());
    }
    // One conversation continued, as far as the person is concerned.
    assert_eq!(
        told(&app, &parent),
        [("Turno completado".to_owned(), 1), (CONTINUES.to_owned(), 1)]
    );
    assert!(told(&app, &child).is_empty());
    assert_screen_untouched(&app, &on_screen);
}

#[test]
fn a_paused_automation_continues_off_screen_under_its_frozen_authority() {
    let root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::limited(1, "AUTOMATIZACION_LISTA");
    let mut app = app_at(root.path(), &provider.endpoint);
    let workspace = root.path().join("proyecto");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), "Sólo lectura.").unwrap();
    app.change_workspace(workspace.display().to_string());
    let index = app.current_automation_store_index().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let native_provider = app.native_settings();
    app.automation_stores[index]
        .create(
            AutomationDraft {
                title: "Lectura programada".into(),
                prompt: TASK.into(),
                backend: "native".into(),
                native_provider: Some(native_provider),
                schedule: AutomationSchedule::Once { at_ms: now + 1 },
            },
            now,
        )
        .unwrap();
    let dispatch = app.automation_stores[index].claim_due(now + 1).unwrap().pop().unwrap();
    app.dispatch_automation(index, dispatch);
    let deadline = Instant::now() + DEADLINE;
    while !app.background_automation_runs.is_empty() {
        app.poll_automations();
        assert!(Instant::now() < deadline, "{:?}", app.view.automations.error);
        std::thread::sleep(Duration::from_millis(3));
    }
    tick(&mut app);
    let run = app.view.pause.sessions.keys().next().cloned().expect("the automation's run paused");
    assert!(app.store.as_ref().unwrap().load(&run).unwrap().unwrap().session.native_automation.is_some());

    // Since then the person widened what their own turns may do and chose
    // another model. None of it is the automation's.
    let mut permissions = AgentToolSettings::default();
    permissions.set(LocalTool::WriteFile, true);
    permissions.set(LocalTool::RunCommand, true);
    assert!(app.commit_agent_tool_settings(permissions, "Interactive turns may write"));
    let on_screen = look_elsewhere(&mut app, &workspace);

    assert!(app.arm_auto_resume());
    assert_eq!(app.background_resumes.len(), 1, "{:?}", told(&app, &run));
    settle(&mut app, |app| app.background_resumes.is_empty());
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["model"], MODEL);
        let mut tools: Vec<&str> = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        tools.sort_unstable();
        assert_eq!(tools, ["list_files", "read_file", "search_files", "update_plan"]);
    }
    let events = journal(&app, &run);
    assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCompleted { .. }));
    assert_eq!(count(&events, |kind| matches!(kind, AgentEventKind::TurnAdmitted { .. })), 1);
    assert_eq!(
        told(&app, &run),
        [("Turno completado".to_owned(), 1), (CONTINUES.to_owned(), 1)]
    );
    assert_screen_untouched(&app, &on_screen);
    tick(&mut app);
    assert!(!app.arm_auto_resume());
    assert_eq!(provider.requests().len(), 2);
}

#[test]
fn the_effort_of_a_paused_turn_comes_from_its_journaled_profile_not_from_the_composer() {
    const PROVIDER: &str = "esfuerzo-fixture";
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // A provider whose requests carry an effort, as the model lists them.
    let mut adapter = ProviderRegistry::builtin().adapter("custom").unwrap().clone();
    adapter.id = PROVIDER.into();
    adapter.name = "Effort fixture".into();
    adapter.endpoint = "http://127.0.0.1:9/v1".into();
    adapter.key_env = "OPENIDE_BACKGROUND_RESUME_ABSENT_KEY".into();
    adapter.user_endpoint = false;
    adapter.key_optional = false;
    adapter.reasoning_effort_field = Some("reasoning_effort".into());
    std::fs::write(
        data.join("provider-registry.json"),
        serde_json::to_vec(&json!({"version": 1, "providers": [adapter]})).unwrap(),
    )
    .unwrap();
    let mut app = AgentsWindow::with_data_root(data);
    app.model_discoveries = vec![ProviderModelDiscovery {
        provider_id: PROVIDER.into(),
        models: BTreeMap::from([(
            MODEL.to_owned(),
            DiscoveredModel {
                id: MODEL.into(),
                name: "Effort fixture".into(),
                listing_index: None,
                provider_reports_tools: true,
                tool_call_declared: Some(true),
                text_input_output: true,
                supported_efforts: vec!["low".into(), "high".into()],
                efforts_declared: true,
                context_window_tokens: Some(32_000),
                max_output_tokens: Some(4_096),
                max_input_tokens: None,
                limits_declared: false,
                capabilities_declared: false,
                declared_capabilities: Default::default(),
            },
        )]),
    }];
    let settings = NativeProviderSettings {
        provider_id: PROVIDER.into(),
        model_id: MODEL.into(),
        custom_endpoint: String::new(),
    };
    let connection = settings.resolve_at(&app.data_root).unwrap();
    let route_of = |model: &Arc<dyn ModelBackend>| {
        openide_core::ModelRouteSnapshot::new(
            PROVIDER,
            MODEL,
            &connection.endpoint,
            model.request_profile_fingerprint().unwrap(),
        )
        .unwrap()
    };
    let admitted_with = |effort: &str| {
        route_of(
            &native_model_backend(
                connection.clone(),
                PROVIDER,
                Some(effort),
                None,
                &app.data_root,
                None,
                &app.model_discoveries,
            )
            .unwrap(),
        )
    };
    let (default, high) = (admitted_with("default"), admitted_with("high"));
    assert_ne!(default, high, "the effort is part of the journaled profile");

    // Whatever the composer shows, each turn gets back exactly its own.
    app.view.composer_effort = "low".into();
    for journaled in [&default, &high] {
        let model = app.journaled_route_backend(&connection, journaled).unwrap();
        assert_eq!(&route_of(&model), journaled);
    }
    // A profile none of this model's efforts produces: nothing to continue on.
    let mut foreign = high.clone();
    foreign.request_profile_digest = "0".repeat(64);
    assert_eq!(
        app.journaled_route_backend(&connection, &foreign).err().as_deref(),
        Some("La ruta del modelo cambió desde que el turno quedó en pausa")
    );
}
