//! Editing an earlier message against a real store in a temporary directory.
//! The model endpoint is a closed local port: no request ever leaves.

use super::*;
use openide_core::AgentMessage;
use std::time::Duration;

const DRAFT: &str = "Borrador de abajo, sin enviar";

struct Fixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    session: String,
    sessions: PathBuf,
}

fn seed_turn(store: &SessionStore, session: &str, prompt: &str) {
    let admitted = store
        .admit_native_turn(session, &uuid::Uuid::new_v4().to_string(), prompt, "test-model", TurnMode::Build)
        .unwrap();
    for (offset, kind) in [
        AgentEventKind::TurnStarted { prompt: prompt.into() },
        AgentEventKind::ModelRequestStarted { step: 1, model: "test-model".into() },
        AgentEventKind::ModelResponseCommitted {
            step: 1,
            assistant: AgentMessage::assistant(format!("Respuesta a {prompt}"), vec![]),
            response_id: None,
            usage: None,
        },
        AgentEventKind::TurnCompleted { response: format!("Respuesta a {prompt}"), steps: 1 },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_native_event(&AgentEvent::new(session, admitted.sequence + 1 + offset as u64, kind))
            .unwrap();
    }
}

fn fixture(prompts: &[&str]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = workspace.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = "test-model".into();
    app.view.endpoint = "http://127.0.0.1:9".into();
    app.view.composer_effort = "default".into();
    assert!(app.persist_native_settings(), "{}", app.view.status);
    let store = app.store.as_ref().unwrap().clone();
    let session = store.create(&workspace, "native").unwrap().id;
    for prompt in prompts {
        seed_turn(&store, &session, prompt);
    }
    app.open_session(&session);
    app.view.prompt = DRAFT.into();
    app.draft_dirty_at = Some(Instant::now());
    let sessions = root.path().join("data/sessions");
    Fixture { _root: root, app, session, sessions }
}

fn click(app: &mut AgentsWindow, id: &str) {
    app.apply_action(UiAction::Clicked { id: id.into() }, &egui::Context::default());
}

fn events(fixture: &Fixture) -> Vec<AgentEvent> {
    fixture.app.store.as_ref().unwrap().load(&fixture.session).unwrap().unwrap().events
}

fn admitted_prompts(events: &[AgentEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            AgentEventKind::TurnAdmitted { prompt, .. } => Some(prompt.as_str()),
            _ => None,
        })
        .collect()
}

fn settle(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.active_run.is_some() || app.view.running {
        app.cancel_selected_run();
        app.poll_run();
        assert!(Instant::now() < deadline, "the turn did not stop: {}", app.view.status);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn editing_a_message_and_cancelling_restores_the_bottom_draft_exactly() {
    let mut fixture = fixture(&["Primero", "Segundo"]);
    let app = &mut fixture.app;
    assert_eq!(app.view.message_edit.turns.len(), 2);
    click(app, "message_edit:0");
    assert_eq!(app.view.message_edit.editing.as_ref().unwrap().user_row, 0);
    assert_eq!(app.view.prompt, "Primero");
    // Typing in the edit never reaches the session's draft.
    app.view.prompt = "Primero, con otra idea".into();
    app.draft_dirty_at = Some(Instant::now());
    app.save_draft();
    click(app, "message_edit_cancel");
    assert!(app.view.message_edit.editing.is_none());
    assert_eq!(app.view.prompt, DRAFT);
    assert_eq!(
        app.store
            .as_ref()
            .unwrap()
            .load_draft(Path::new(&app.view.workspace), Some(&fixture.session))
            .unwrap()
            .as_deref(),
        Some(DRAFT)
    );
    assert_eq!(events(&fixture).len(), 10);
}

#[test]
fn sending_from_an_earlier_message_asks_rewinds_sends_and_can_be_undone() {
    let mut fixture = fixture(&["Primero", "Segundo"]);
    let before = events(&fixture);
    click(&mut fixture.app, "message_edit:0");
    fixture.app.view.prompt = "Primero, editado".into();

    // Sending asks first and changes nothing.
    fixture.app.start_run();
    assert_eq!(fixture.app.view.message_edit.confirm.as_ref().unwrap().later_messages, 1);
    assert_eq!(events(&fixture), before);
    click(&mut fixture.app, "message_edit_confirm_cancel");
    assert!(fixture.app.view.message_edit.confirm.is_none());
    assert!(fixture.app.view.message_edit.editing.is_some());
    assert_eq!(fixture.app.view.prompt, "Primero, editado");

    // A boundary recorded during the turn that is about to be removed.
    let storage = fixture.sessions.clone();
    let workspace = PathBuf::from(&fixture.app.view.workspace);
    crate::session_changes_external::mark_external_runtime(&storage, &fixture.session, &workspace, 7)
        .unwrap();
    fixture.app.start_run();
    click(&mut fixture.app, "message_edit_keep_files");
    assert!(fixture.app.view.message_edit.editing.is_none(), "{}", fixture.app.view.status);
    // It no longer points past the journal, so later turns can prepare MCP.
    assert_eq!(
        crate::session_changes_external::external_runtime_was_prepared(&storage, &fixture.session, &workspace)
            .unwrap(),
        Some(0)
    );
    assert!(fixture.app.view.message_edit.confirm.is_none());
    // The journal restarts at the edited message, with the removed sequences.
    let after = events(&fixture);
    assert_eq!(admitted_prompts(&after), vec!["Primero, editado"]);
    assert_eq!(after[0].sequence, before[0].sequence);
    // The bottom draft is back, and the removed events are saved.
    assert_eq!(fixture.app.view.prompt, DRAFT);
    assert_eq!(fixture.app.view.message_edit.undo.as_ref().unwrap().removed_messages, 2);
    let tails: Vec<_> = std::fs::read_dir(&fixture.sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().contains(".rewind-"))
        .collect();
    assert_eq!(tails.len(), 1);
    let saved: Vec<AgentEvent> = std::fs::read_to_string(&tails[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(saved, before);

    // While the new turn runs the undo waits.
    if fixture.app.view.running {
        click(&mut fixture.app, "message_rewind_undo");
        assert!(fixture.app.view.message_edit.undo.as_ref().unwrap().error.is_some());
    }
    settle(&mut fixture.app);
    click(&mut fixture.app, "message_rewind_undo");
    assert!(fixture.app.view.message_edit.undo.is_none(), "{}", fixture.app.view.status);
    assert_eq!(events(&fixture), before);
    assert_eq!(fixture.app.view.message_edit.turns.len(), 2);
    assert_eq!(fixture.app.view.prompt, DRAFT);
    // The replacement turn was not discarded: it has its own saved tail.
    let tails = std::fs::read_dir(&fixture.sessions)
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().path().to_string_lossy().contains(".rewind-"))
        .count();
    assert_eq!(tails, 1);
}

#[test]
fn messages_whose_typed_text_is_not_certain_offer_no_edit() {
    let composed = "Revisá esto\n\nAdjuntos de este proyecto (rutas relativas): [{\"path\":\"a.rs\",\"type\":\"file\"}]\nAbre los archivos y explora las carpetas pertinentes; úsalos como contexto para esta tarea.";
    let mut fixture = fixture(&["Simple", composed]);
    let turns = fixture.app.view.message_edit.turns.clone();
    assert_eq!(turns[0].text.as_deref(), Some("Simple"));
    assert!(!turns[0].drops_attachments);
    assert_eq!(turns[1].text, None);
    click(&mut fixture.app, "message_edit:1");
    assert!(fixture.app.view.message_edit.editing.is_none());
    assert_eq!(fixture.app.view.prompt, DRAFT);

    // With the typed text on record the same message opens, and says that
    // its attachments are not resent.
    let session = fixture.session.clone();
    fixture.app.record_typed_prompt(&session, composed, "Revisá esto");
    fixture.app.new_session();
    fixture.app.open_session(&session);
    let turn = fixture.app.view.message_edit.turns[1].clone();
    assert_eq!(turn.text.as_deref(), Some("Revisá esto"));
    assert!(turn.drops_attachments);
    click(&mut fixture.app, "message_edit:1");
    assert_eq!(fixture.app.view.prompt, "Revisá esto");
    assert!(fixture.app.view.message_edit.editing.as_ref().unwrap().drops_attachments);
}

#[test]
fn sessions_that_are_never_rewound_offer_no_edit() {
    // A subagent's session.
    let mut fixture = fixture(&["Primero"]);
    let store = fixture.app.store.as_ref().unwrap().clone();
    let path = fixture.sessions.join(format!("{}.session.json", fixture.session));
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    metadata["subagent"] = serde_json::json!({
        "parent_session_id": "parent", "depth": 1, "spawn_id": "tool-1", "request_digest": ""
    });
    std::fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let session = fixture.session.clone();
    fixture.app.new_session();
    fixture.app.open_session(&session);
    assert!(fixture.app.view.message_edit.turns.is_empty());
    click(&mut fixture.app, "message_edit:0");
    assert!(fixture.app.view.message_edit.editing.is_none());
    assert!(store.rewind_native(&session, 1).is_err());

    // An external CLI's history has no admitted turns to cut at.
    let mut fixture = self::fixture(&[]);
    let workspace = PathBuf::from(&fixture.app.view.workspace);
    let cli = store_session(&fixture, &workspace);
    fixture.app.open_session(&cli);
    assert!(fixture.app.view.message_edit.turns.is_empty());
}

fn store_session(fixture: &Fixture, workspace: &Path) -> String {
    let store = fixture.app.store.as_ref().unwrap();
    let session = store.create(workspace, "codex").unwrap().id;
    for (sequence, kind) in [
        AgentEventKind::TurnStarted { prompt: "Desde la CLI".into() },
        AgentEventKind::TurnCompleted { response: "Listo".into(), steps: 1 },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_event(&session, &AgentEvent::new(&session, sequence as u64 + 1, kind))
            .unwrap();
    }
    session
}
