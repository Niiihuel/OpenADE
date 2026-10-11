//! The window's side of a paused turn: the timer, Esc and cancelling a tree.
//! Journals are written by hand; no provider is ever reached.

use super::*;
use crate::pause::{wall_clock_ms, AutoResume};
use openide_core::TurnPauseClass;

const MINUTE: u64 = 60_000;

/// A native session whose turn paused `paused_ago_ms` ago on a limit that
/// resets at `resets_at_ms`.
fn paused_session(app: &AgentsWindow, root: &Path, paused_ago_ms: u64, resets_at_ms: Option<u64>) -> String {
    let store = app.store.as_ref().unwrap();
    let session = store.create(root, "native").unwrap();
    let admitted = store
        .admit_native_turn(&session.id, "turn-1", "Migrá los tests", "m", TurnMode::Build)
        .unwrap();
    for (offset, kind) in [
        AgentEventKind::TurnStarted { prompt: "Migrá los tests".into() },
        AgentEventKind::ModelRequestStarted { step: 1, model: "m".into() },
        AgentEventKind::TurnPaused {
            class: TurnPauseClass::LimitReached,
            limit_key: "pause-app-test".into(),
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

fn app_with(root: &Path) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = root.to_string_lossy().into_owned();
    app.view.pause.auto_enabled = true;
    app
}

fn select(app: &mut AgentsWindow, session_id: &str) {
    app.open_session(session_id);
    app.refresh_sessions();
    app.view.pause.clock_ms = wall_clock_ms();
    assert!(app.view.pause.sessions.contains_key(session_id), "{}", app.view.status);
}

#[test]
fn the_timer_continues_a_paused_turn_once_after_its_reset_and_never_on_a_doubt() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_with(root.path());
    let now = wall_clock_ms();

    // Still closed: nothing happens, and Esc cancels the automatic
    // continuation while the pause itself stays.
    let waiting = paused_session(&app, root.path(), MINUTE, Some(now + 10 * MINUTE));
    select(&mut app, &waiting);
    assert!(matches!(
        app.view.pause.plan(&waiting, &app.view.pause.sessions[&waiting]),
        AutoResume::At(_)
    ));
    assert!(!app.arm_auto_resume() && app.pause_resume_queue.is_empty());
    assert!(app.cancel_auto_resume());
    assert_eq!(app.view.pause.plan(&waiting, &app.view.pause.sessions[&waiting]), AutoResume::Manual);
    assert!(app.view.pause.sessions.contains_key(&waiting));
    assert!(!app.cancel_auto_resume(), "nothing is armed any more");
    let journal = app.store.as_ref().unwrap().load(&waiting).unwrap().unwrap().events;
    assert!(matches!(journal.last().unwrap().kind, AgentEventKind::TurnPaused { .. }));

    // The reset passed a moment ago, as after a short sleep: one attempt
    // for this pause, announced once.
    let due = paused_session(&app, root.path(), 2 * 60 * MINUTE, Some(now - 5 * MINUTE));
    select(&mut app, &due);
    // Recorded as a notification when the window is not being looked at.
    app.window_focused = false;
    let before = app.view.notifications.items().len();
    assert!(app.arm_auto_resume());
    assert_eq!(app.pause_resume_queue, [due.clone()]);
    assert_eq!(app.view.notifications.items().len(), before + 1);
    assert!(!app.arm_auto_resume(), "already queued");
    app.pause_resume_queue.clear();
    assert!(!app.arm_auto_resume(), "one automatic attempt per pause");
    assert_eq!(app.view.notifications.items().len(), before + 1);

    // More than half an hour late, as after the app was closed overnight:
    // it asks instead of continuing.
    let late = paused_session(&app, root.path(), 3 * 60 * MINUTE, Some(now - 31 * MINUTE));
    select(&mut app, &late);
    assert_eq!(app.view.pause.plan(&late, &app.view.pause.sessions[&late]), AutoResume::Ask);
    assert!(!app.arm_auto_resume());
    let line = app.view.pause.composer_line(Some(&late)).unwrap();
    assert!(line.contains("continuá cuando quieras"), "{line}");

    // More than a day away, an unknown reset, or the setting off.
    let far = paused_session(&app, root.path(), 25 * 60 * MINUTE, Some(now - MINUTE));
    select(&mut app, &far);
    assert!(!app.arm_auto_resume());
    let unknown = paused_session(&app, root.path(), MINUTE, None);
    select(&mut app, &unknown);
    assert!(!app.arm_auto_resume());
    let off = paused_session(&app, root.path(), 60 * MINUTE, Some(now - MINUTE));
    select(&mut app, &off);
    app.view.pause.auto_enabled = false;
    assert!(!app.arm_auto_resume() && app.pause_resume_queue.is_empty());
}

#[test]
fn cancelling_a_pause_ends_the_turn_in_place_and_the_setting_is_saved() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_with(root.path());
    let ctx = egui::Context::default();
    let session_id = paused_session(&app, root.path(), MINUTE, Some(wall_clock_ms() + 60 * MINUTE));
    select(&mut app, &session_id);
    app.apply_action(UiAction::Clicked { id: format!("pause_detail:{session_id}") }, &ctx);
    assert_eq!(app.view.pause.detail_open.as_deref(), Some(session_id.as_str()));
    app.apply_action(UiAction::Clicked { id: format!("pause_model:{session_id}") }, &ctx);
    assert!(app.view.composer_model_menu_open);
    assert_eq!(app.view.pause.model_note.as_deref(), Some(session_id.as_str()));
    assert!(app.view.pause.sessions.contains_key(&session_id), "the picker changes nothing by itself");

    app.apply_action(UiAction::Clicked { id: format!("pause_cancel:{session_id}") }, &ctx);
    assert_eq!(app.view.status, "Turno cancelado");
    assert!(app.view.pause.sessions.is_empty() && !app.view.session_recovery.contains_key(&session_id));
    let events = app.store.as_ref().unwrap().load(&session_id).unwrap().unwrap().events;
    assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCancelled));
    assert_eq!(events.iter().filter(|event| matches!(event.kind, AgentEventKind::TurnCancelled)).count(), 1);
    // A second click finds nothing to cancel.
    app.apply_action(UiAction::Clicked { id: format!("pause_cancel:{session_id}") }, &ctx);
    assert_eq!(app.view.status, "El turno ya no está en pausa");

    // The Settings switch is on by default and survives a restart.
    assert!(app.settings_preferences.auto_resume_on_limit);
    app.apply_action(UiAction::Clicked { id: "settings_auto_resume_toggle".into() }, &ctx);
    assert!(!app.view.pause.auto_enabled);
    let reopened = AgentsWindow::with_data_root(root.path().join("data"));
    assert!(!reopened.settings_preferences.auto_resume_on_limit && !reopened.view.pause.auto_enabled);
}

/// A provider that refuses every request with a reached limit.
struct LimitedModel;

#[async_trait::async_trait]
impl openide_harness::ModelBackend for LimitedModel {
    fn model_id(&self) -> &str {
        "m"
    }
    fn request_profile_fingerprint(&self) -> Option<[u8; 32]> {
        Some([7; 32])
    }
    async fn complete(
        &self,
        _messages: &[openide_core::AgentMessage],
        _tools: &[openide_harness::ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<openide_harness::ModelOutput, openide_harness::AgentError> {
        let mut failure = openide_harness::ProviderFailure::new(
            openide_harness::FailureClass::LimitReached,
            "HTTP 429: usage_limit_reached",
        );
        failure.resets_at_ms = Some(wall_clock_ms() + 60 * MINUTE);
        Err(openide_harness::AgentError::Provider(failure))
    }
}

#[test]
fn cancelling_a_paused_tree_cancels_the_root_and_every_paused_descendant() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app_with(root.path());
    let parent = paused_session(&app, root.path(), MINUTE, Some(wall_clock_ms() + 60 * MINUTE));
    let store = app.store.clone().unwrap();
    // A real child of that session runs into the limit and pauses.
    let coordinator =
        Arc::new(SubagentCoordinator::new(store.clone(), SubagentLimits::default()).unwrap());
    let worker = openide_harness::SubagentWorker::new(
        coordinator.clone(),
        store.clone(),
        Arc::new(LimitedModel),
        Arc::new(openide_harness::LocalTools::new()),
        "fixture",
        "https://fixture.example/v1",
    );
    let child = {
        let lease = Arc::new(store.try_acquire_native_lease(&parent).unwrap());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(worker.run(
                lease,
                "spawn-1".into(),
                "Migrá facturas".into(),
                TurnMode::Build,
                CancellationToken::new(),
            ))
            .unwrap()
    };
    assert_eq!(child.status, SubagentStatus::Interrupted);
    let child = child.child_session_id;
    select(&mut app, &parent);
    // One tree: the child continues first, and reads as paused, not failed.
    assert_eq!(app.view.pause.sessions[&child].parent.as_deref(), Some(parent.as_str()));
    assert_eq!(
        crate::pause::resume_order(&app.view.pause.sessions, &parent),
        [child.clone(), parent.clone()]
    );

    app.apply_action(
        UiAction::Clicked { id: format!("pause_cancel:{parent}") },
        &egui::Context::default(),
    );
    assert_eq!(app.view.status, "Turno cancelado");
    assert!(app.view.pause.sessions.is_empty(), "{:?}", app.view.pause.sessions);
    for id in [&parent, &child] {
        let events = store.load(id).unwrap().unwrap().events;
        assert!(matches!(events.last().unwrap().kind, AgentEventKind::TurnCancelled), "{id}");
    }
    let lease = store.try_acquire_native_lease(&parent).unwrap();
    assert_eq!(coordinator.list_children(&lease).unwrap()[0].status, SubagentStatus::Cancelled);
}
