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
    fixture_at(prompts, "http://127.0.0.1:9")
}

fn fixture_at(prompts: &[&str], endpoint: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = workspace.to_string_lossy().into_owned();
    app.view.selected_backend = "native".into();
    app.view.native_provider_id = "custom".into();
    app.view.model = "test-model".into();
    app.view.endpoint = endpoint.into();
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
fn retrying_a_turn_is_its_edit_with_the_same_text_and_the_same_question() {
    let mut fixture = fixture(&["Primero", "Segundo"]);
    let before = events(&fixture);
    // A restored session shows when each turn really ended.
    for row in 0..2 {
        let id = crate::transcript::turn_time_id(row);
        assert!(fixture.app.view.item_times.ended_ms(&id).is_some(), "{id}");
    }
    click(&mut fixture.app, "turn_retry:0");
    let app = &mut fixture.app;
    assert_eq!(app.view.message_edit.editing.as_ref().unwrap().user_row, 0);
    assert_eq!(app.view.prompt, "Primero");
    assert_eq!(app.view.message_edit.confirm.as_ref().unwrap().later_messages, 1);
    assert_eq!(events(&fixture), before, "asking changes nothing");

    // Declining leaves the edit open, and leaving it restores the draft.
    let app = &mut fixture.app;
    click(app, "message_edit_confirm_cancel");
    click(app, "message_edit_cancel");
    assert!(app.view.message_edit.editing.is_none());
    assert_eq!(app.view.prompt, DRAFT);
    // It never starts over an edit, nor for a message that would lose attachments.
    click(app, "message_edit:1");
    click(app, "turn_retry:0");
    assert_eq!(app.view.message_edit.editing.as_ref().unwrap().user_row, 1);
    assert!(app.view.message_edit.confirm.is_none());
    click(app, "message_edit_cancel");
    app.view.message_edit.turns[0].drops_attachments = true;
    click(app, "turn_retry:0");
    click(app, "turn_retry:9");
    assert!(app.view.message_edit.editing.is_none() && app.view.message_edit.confirm.is_none());
    assert_eq!(events(&fixture), before);

    // Copying marks its button and takes nothing else with it.
    let app = &mut fixture.app;
    click(app, "turn_copy:1");
    assert!(app.view.message_actions.copied.as_ref().unwrap().shows("turn_copy:1", Instant::now()));
    assert!(!app.expire_message_copy(&egui::Context::default()), "the check stays for its time");
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

fn write_call(id: &str, path: &str, content: &str) -> openide_core::ToolCall {
    openide_core::ToolCall {
        id: id.into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": path, "content": content}),
    }
}

/// A turn whose calls really ran, through the executor that records their
/// file captures. `run_command` is journaled without running.
fn seed_tool_turn(fixture: &Fixture, prompt: &str, calls: &[openide_core::ToolCall]) {
    let store = fixture.app.store.as_ref().unwrap().clone();
    let session = fixture.session.as_str();
    let workspace = PathBuf::from(&fixture.app.view.workspace);
    let mut sequence = store
        .admit_native_turn(session, &uuid::Uuid::new_v4().to_string(), prompt, "test-model", TurnMode::Build)
        .unwrap()
        .sequence;
    let mut append = |kind| {
        sequence += 1;
        store.append_native_event(&AgentEvent::new(session, sequence, kind)).unwrap();
    };
    append(AgentEventKind::TurnStarted { prompt: prompt.into() });
    append(AgentEventKind::ModelRequestStarted { step: 1, model: "test-model".into() });
    append(AgentEventKind::ModelResponseCommitted {
        step: 1,
        assistant: AgentMessage::assistant("", calls.to_vec()),
        response_id: None,
        usage: None,
    });
    let executor = crate::session_changes::with_file_snapshots(
        Arc::new(LocalTools::new().with_write_access(true)),
        store.storage_root().to_owned(),
        session.to_owned(),
        workspace.clone(),
    );
    let context = openide_harness::ToolContext {
        workspace,
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    for call in calls {
        append(AgentEventKind::ToolStarted { tool_call: call.clone() });
        let output = match call.name.as_str() {
            "write_file" => {
                let output = runtime.block_on(executor.execute(call, &context)).unwrap();
                assert!(!output.is_error, "{}", output.content);
                output.content
            }
            _ => String::new(),
        };
        append(AgentEventKind::ToolFinished { tool_call_id: call.id.clone(), output, is_error: false });
    }
    append(AgentEventKind::ModelRequestStarted { step: 2, model: "test-model".into() });
    append(AgentEventKind::ModelResponseCommitted {
        step: 2,
        assistant: AgentMessage::assistant("Listo", vec![]),
        response_id: None,
        usage: None,
    });
    append(AgentEventKind::TurnCompleted { response: "Listo".into(), steps: 2 });
}

/// "Primero", then a turn that rewrote `a.txt` and `b.txt`, created
/// `new.txt` and ran a command. The first message is open for editing and
/// its confirmation is asked.
fn revert_fixture() -> Fixture {
    let mut fixture = fixture(&["Primero"]);
    let workspace = PathBuf::from(&fixture.app.view.workspace);
    std::fs::write(workspace.join("a.txt"), "original a\n").unwrap();
    std::fs::write(workspace.join("b.txt"), "original b\n").unwrap();
    seed_tool_turn(
        &fixture,
        "Segundo",
        &[
            write_call("w-a", "a.txt", "agent a\n"),
            write_call("w-b", "b.txt", "agent b\n"),
            write_call("w-new", "new.txt", "agent new\n"),
            openide_core::ToolCall {
                id: "cmd".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({"command": "true"}),
            },
        ],
    );
    let session = fixture.session.clone();
    fixture.app.new_session();
    fixture.app.open_session(&session);
    click(&mut fixture.app, "message_edit:0");
    fixture.app.view.prompt = "Primero, editado".into();
    fixture.app.start_run();
    fixture
}

fn disk(fixture: &Fixture, path: &str) -> Option<String> {
    std::fs::read_to_string(Path::new(&fixture.app.view.workspace).join(path)).ok()
}

fn put(fixture: &Fixture, path: &str, content: &str) {
    std::fs::write(Path::new(&fixture.app.view.workspace).join(path), content).unwrap();
}

#[test]
fn reverting_restores_the_files_rewinds_and_an_undo_puts_back_what_did_not_change_since() {
    let mut fixture = revert_fixture();
    let before = events(&fixture);
    let confirm = fixture.app.view.message_edit.confirm.clone().unwrap();
    assert_eq!(confirm.restorable, ["a.txt", "b.txt", "new.txt"]);
    assert!(confirm.skipped.is_empty());
    assert_eq!(confirm.opaque.commands, 1);

    click(&mut fixture.app, "message_edit_revert");
    assert!(fixture.app.view.message_edit.editing.is_none(), "{:?}", fixture.app.view.message_edit.confirm);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("original a\n"));
    assert_eq!(disk(&fixture, "b.txt").as_deref(), Some("original b\n"));
    assert_eq!(disk(&fixture, "new.txt"), None);
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero, editado"]);
    let notice = fixture.app.view.message_edit.undo.clone().unwrap();
    assert_eq!((notice.restored_files, notice.skipped_files.len(), notice.undone), (3, 0, false));

    // The replacement turn changed one of them: an undo leaves it alone.
    settle(&mut fixture.app);
    put(&fixture, "b.txt", "replacement turn\n");
    click(&mut fixture.app, "message_rewind_undo");
    assert_eq!(events(&fixture), before, "{}", fixture.app.view.status);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("agent a\n"));
    assert_eq!(disk(&fixture, "new.txt").as_deref(), Some("agent new\n"));
    assert_eq!(disk(&fixture, "b.txt").as_deref(), Some("replacement turn\n"));
    let notice = fixture.app.view.message_edit.undo.clone().unwrap();
    assert!(notice.undone);
    assert_eq!(notice.skipped_files, ["b.txt"]);
    click(&mut fixture.app, "message_rewind_undo_dismiss");
    assert!(fixture.app.view.message_edit.undo.is_none());
}

#[test]
fn revert_acts_on_what_is_restorable_when_it_runs_and_refuses_with_nothing_to_restore() {
    // The disk changed while the dialog was open.
    let mut fixture = revert_fixture();
    put(&fixture, "b.txt", "edited by hand\n");
    click(&mut fixture.app, "message_edit_revert");
    assert!(fixture.app.view.message_edit.editing.is_none());
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("original a\n"));
    assert_eq!(disk(&fixture, "b.txt").as_deref(), Some("edited by hand\n"));
    let notice = fixture.app.view.message_edit.undo.clone().unwrap();
    assert_eq!((notice.restored_files, notice.skipped_files), (2, vec!["b.txt".to_owned()]));
    settle(&mut fixture.app);

    // Nothing restorable: "Revertir" changes nothing, "No revertir" still works
    // and the command that cannot be undone is still reported.
    let mut fixture = revert_fixture();
    click(&mut fixture.app, "message_edit_confirm_cancel");
    for path in ["a.txt", "b.txt", "new.txt"] {
        put(&fixture, path, "edited by hand\n");
    }
    fixture.app.start_run();
    let before = events(&fixture);
    let confirm = fixture.app.view.message_edit.confirm.clone().unwrap();
    assert!(confirm.restorable.is_empty());
    assert_eq!(confirm.skipped.len(), 3);
    assert_eq!(confirm.opaque.commands, 1);
    click(&mut fixture.app, "message_edit_revert");
    assert!(fixture.app.view.message_edit.confirm.as_ref().unwrap().error.is_some());
    assert_eq!(events(&fixture), before);
    click(&mut fixture.app, "message_edit_keep_files");
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero, editado"]);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("edited by hand\n"));
    assert_eq!(fixture.app.view.message_edit.undo.as_ref().unwrap().restored_files, 0);
    settle(&mut fixture.app);
}

#[cfg(unix)]
#[test]
fn a_failed_write_or_a_failed_rewind_leaves_files_and_journal_as_they_were() {
    use std::os::unix::fs::PermissionsExt;
    let agent = [("a.txt", "agent a\n"), ("b.txt", "agent b\n"), ("new.txt", "agent new\n")];
    let unchanged = |fixture: &Fixture, before: &[AgentEvent]| {
        let confirm = fixture.app.view.message_edit.confirm.as_ref().expect("the dialog stays open");
        assert!(confirm.error.is_some());
        assert!(fixture.app.view.message_edit.editing.is_some());
        assert!(fixture.app.view.message_edit.undo.is_none());
        assert_eq!(events(fixture), before);
        for (path, content) in agent {
            assert_eq!(disk(fixture, path).as_deref(), Some(content), "{path}: {:?}", confirm.error);
        }
    };

    // The second write fails: the first is put back and nothing is rewound.
    let mut fixture = revert_fixture();
    let before = events(&fixture);
    let read_only = Path::new(&fixture.app.view.workspace).join("b.txt");
    std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o444)).unwrap();
    click(&mut fixture.app, "message_edit_revert");
    unchanged(&fixture, &before);

    // The rewind is refused after the files were written: they are put back.
    let mut fixture = revert_fixture();
    let before = events(&fixture);
    let store = fixture.app.store.as_ref().unwrap().clone();
    let lease = store.try_acquire_native_lease(&fixture.session).unwrap();
    click(&mut fixture.app, "message_edit_revert");
    unchanged(&fixture, &before);
    drop(lease);
    click(&mut fixture.app, "message_edit_revert");
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("original a\n"));
    settle(&mut fixture.app);
}

/// A model endpoint that accepts and never answers: a turn sent to it runs
/// until it is cancelled. Local only; the listener lives with the test.
fn silent_endpoint() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    (listener, endpoint)
}

/// "Primero" answered, then "Segundo" really running against `endpoint`,
/// with its message published as editable.
fn live_turn(endpoint: &str) -> Fixture {
    let mut fixture = fixture_at(&["Primero"], endpoint);
    fixture.app.view.prompt = "Segundo".into();
    fixture.app.start_run();
    assert!(fixture.app.view.running && fixture.app.active_run.is_some(), "{}", fixture.app.view.status);
    let deadline = Instant::now() + Duration::from_secs(30);
    while fixture.app.view.message_edit.turns.len() < 2 {
        fixture.app.poll_run();
        assert!(fixture.app.view.running, "the turn ended by itself: {}", fixture.app.view.status);
        assert!(
            Instant::now() < deadline,
            "the turn was never admitted: {:?}",
            events(&fixture).iter().map(|event| format!("{:.40?}", event.kind)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    fixture
}

/// Run the app's two polls, as a frame does, until `done`.
fn frames_until(fixture: &mut Fixture, done: impl Fn(&AgentsWindow) -> bool, each: impl Fn(&Fixture)) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(&fixture.app) {
        each(fixture);
        fixture.app.poll_run();
        fixture.app.poll_message_edit_stop();
        assert!(Instant::now() < deadline, "it never settled: {}", fixture.app.view.status);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_running_turn_message_opens_for_editing_and_the_turn_keeps_running() {
    let (_listener, endpoint) = silent_endpoint();
    let mut fixture = live_turn(&endpoint);
    let before = admitted_prompts(&events(&fixture)).len();
    click(&mut fixture.app, "message_edit:1");
    let app = &mut fixture.app;
    assert_eq!(app.view.message_edit.editing.as_ref().unwrap().user_row, 1, "{}", app.view.status);
    assert_eq!(app.view.prompt, "Segundo");
    app.view.prompt = "Segundo, editado".into();
    // Sending from the edit composer asks; it does not stop anything either.
    click(app, "run_toggle");
    let confirm = app.view.message_edit.confirm.clone().unwrap();
    assert!(confirm.stops_turn && !confirm.stopping);
    assert_eq!(confirm.later_messages, 0);
    click(app, "message_edit_confirm_cancel");
    for _ in 0..20 {
        app.poll_run();
        app.poll_message_edit_stop();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.view.running && app.active_run.is_some(), "{}", app.view.status);
    assert_ne!(app.view.status, "Cancelando...");
    assert_eq!(app.view.prompt, "Segundo, editado");
    // An earlier message opens too, once this edit is left.
    click(app, "message_edit_cancel");
    assert_eq!(app.view.prompt, "", "the bottom draft was sent as the running turn");
    click(app, "message_edit:0");
    assert_eq!(app.view.prompt, "Primero");
    assert!(app.view.running);
    click(app, "message_edit_cancel");
    assert_eq!(admitted_prompts(&events(&fixture)).len(), before);
    // The message's own stop still stops the turn.
    click(&mut fixture.app, "turn_stop");
    assert_eq!(fixture.app.view.status, "Cancelando...");
    settle(&mut fixture.app);
}

#[test]
fn sending_an_edit_while_its_turn_runs_stops_the_turn_and_rewinds_only_once_idle() {
    let (_listener, endpoint) = silent_endpoint();
    let mut fixture = live_turn(&endpoint);
    click(&mut fixture.app, "message_edit:1");
    fixture.app.view.prompt = "Segundo, editado".into();
    fixture.app.start_run();
    click(&mut fixture.app, "message_edit_keep_files");
    // The stop was asked for; nothing else happened yet.
    let app = &fixture.app;
    assert_eq!(app.view.status, "Cancelando...");
    assert!(app.view.message_edit.confirm.as_ref().unwrap().stopping);
    assert!(app.view.running && app.view.message_edit.undo.is_none());
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero", "Segundo"]);
    // A second click while it waits is not a second confirmation.
    click(&mut fixture.app, "message_edit_keep_files");

    frames_until(
        &mut fixture,
        |app| app.view.message_edit.editing.is_none(),
        |fixture| {
            // While the stopped turn has not ended, the journal keeps it.
            if fixture.app.view.message_edit.confirm.is_some() {
                assert_eq!(admitted_prompts(&events(fixture)), vec!["Primero", "Segundo"]);
            }
        },
    );
    assert!(fixture.app.view.message_edit.confirm.is_none(), "{}", fixture.app.view.status);
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero", "Segundo, editado"]);
    assert_eq!(fixture.app.view.message_edit.undo.as_ref().unwrap().removed_messages, 1);
    // What was removed is the stopped turn, whole: it had ended.
    let tail = std::fs::read_dir(&fixture.sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().contains(".rewind-"))
        .unwrap();
    let removed = std::fs::read_to_string(tail).unwrap();
    assert!(removed.contains("Segundo") && removed.contains("turn_cancelled"), "{removed}");
    settle(&mut fixture.app);
}

#[test]
fn cancelling_an_edit_that_waits_for_the_stop_leaves_the_turn_stopped_and_the_conversation() {
    let (_listener, endpoint) = silent_endpoint();
    let mut fixture = live_turn(&endpoint);
    click(&mut fixture.app, "message_edit:1");
    fixture.app.view.prompt = "Segundo, editado".into();
    fixture.app.start_run();
    click(&mut fixture.app, "message_edit_revert");
    assert!(fixture.app.view.message_edit.confirm.as_ref().unwrap().stopping);
    click(&mut fixture.app, "message_edit_confirm_cancel");
    assert!(fixture.app.view.message_edit.confirm.is_none());
    frames_until(&mut fixture, |app| !app.view.running && app.active_run.is_none(), |_| {});
    for _ in 0..5 {
        fixture.app.poll_message_edit_stop();
    }
    let app = &fixture.app;
    assert!(app.view.message_edit.editing.is_some() && app.view.message_edit.undo.is_none());
    assert_eq!(app.view.prompt, "Segundo, editado");
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero", "Segundo"]);
}

/// A turn admitted and started, with no end: what a live turn has journaled.
fn seed_open_turn(fixture: &Fixture, prompt: &str) {
    let store = fixture.app.store.as_ref().unwrap();
    let admitted = store
        .admit_native_turn(&fixture.session, &uuid::Uuid::new_v4().to_string(), prompt, "test-model", TurnMode::Build)
        .unwrap();
    store
        .append_native_event(&AgentEvent::new(
            &fixture.session,
            admitted.sequence + 1,
            AgentEventKind::TurnStarted { prompt: prompt.into() },
        ))
        .unwrap();
}

/// Show the session as the host has it while that open turn runs.
fn reopen_running(fixture: &mut Fixture) {
    let session = fixture.session.clone();
    fixture.app.new_session();
    fixture.app.open_session(&session);
    fixture.app.view.running = true;
}

/// The run ends; with `cancelled` it journaled its end first.
fn end_run(fixture: &mut Fixture, cancelled: bool) {
    if cancelled {
        let sequence = events(fixture).last().unwrap().sequence + 1;
        let store = fixture.app.store.as_ref().unwrap();
        store
            .append_native_event(&AgentEvent::new(&fixture.session, sequence, AgentEventKind::TurnCancelled))
            .unwrap();
    }
    fixture.app.view.running = false;
    fixture.app.poll_message_edit_stop();
}

#[test]
fn a_stop_that_leaves_the_turn_unfinished_abandons_the_edit_and_keeps_its_text() {
    let mut fixture = fixture(&["Primero"]);
    seed_open_turn(&fixture, "Segundo");
    reopen_running(&mut fixture);
    click(&mut fixture.app, "message_edit:1");
    assert!(fixture.app.view.message_edit.editing.is_some(), "{}", fixture.app.view.status);
    fixture.app.view.prompt = "Segundo, editado".into();
    fixture.app.start_run();
    click(&mut fixture.app, "message_edit_keep_files");
    assert!(fixture.app.view.message_edit.confirm.as_ref().unwrap().stopping);
    let before = events(&fixture);
    // The process died without journaling the end of its turn.
    end_run(&mut fixture, false);
    let app = &fixture.app;
    assert!(app.view.message_edit.confirm.is_none());
    let error = app.view.composer_error.clone().expect("the edit says why it was abandoned");
    assert!(error.contains("necesita recuperación") && error.contains("no se envió"), "{error}");
    assert!(app.view.message_edit.editing.is_some());
    assert_eq!(app.view.prompt, "Segundo, editado");
    assert!(app.view.message_edit.undo.is_none());
    assert_eq!(events(&fixture), before);
    // Nothing is left waiting.
    assert!(!fixture.app.poll_message_edit_stop());
}

/// `revert_fixture`'s turns, then "Tercero" running. The first message is
/// open for editing and its confirmation is asked while that turn runs.
fn revert_while_running() -> Fixture {
    let mut fixture = revert_fixture();
    click(&mut fixture.app, "message_edit_confirm_cancel");
    click(&mut fixture.app, "message_edit_cancel");
    seed_open_turn(&fixture, "Tercero");
    reopen_running(&mut fixture);
    click(&mut fixture.app, "message_edit:0");
    fixture.app.view.prompt = "Primero, editado".into();
    fixture.app.start_run();
    let confirm = fixture.app.view.message_edit.confirm.clone().unwrap();
    assert!(confirm.stops_turn);
    assert_eq!(confirm.restorable, ["a.txt", "b.txt", "new.txt"]);
    fixture
}

#[test]
fn files_are_checked_again_after_the_stop_and_keeping_them_needs_no_second_answer() {
    let mut fixture = revert_while_running();
    click(&mut fixture.app, "message_edit_keep_files");
    // The disk changes before the turn stops: only a revert would care.
    put(&fixture, "b.txt", "edited by hand\n");
    assert_eq!(admitted_prompts(&events(&fixture)).len(), 3);
    end_run(&mut fixture, true);
    assert!(fixture.app.view.message_edit.editing.is_none(), "{:?}", fixture.app.view.message_edit.confirm);
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero, editado"]);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("agent a\n"));
    assert_eq!(disk(&fixture, "b.txt").as_deref(), Some("edited by hand\n"));
    assert_eq!(fixture.app.view.message_edit.undo.as_ref().unwrap().restored_files, 0);
    settle(&mut fixture.app);
}

#[test]
fn a_revert_chosen_on_lists_that_changed_is_asked_again_before_any_file_is_touched() {
    // Unchanged lists: the choice stands and the revert happens.
    let mut fixture = revert_while_running();
    click(&mut fixture.app, "message_edit_revert");
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("agent a\n"), "nothing is written before the stop");
    end_run(&mut fixture, true);
    assert!(fixture.app.view.message_edit.editing.is_none(), "{:?}", fixture.app.view.message_edit.confirm);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("original a\n"));
    assert_eq!(disk(&fixture, "new.txt"), None);
    settle(&mut fixture.app);

    // A file changed before the stop: the final list is shown first.
    let mut fixture = revert_while_running();
    click(&mut fixture.app, "message_edit_revert");
    put(&fixture, "b.txt", "edited by hand\n");
    end_run(&mut fixture, true);
    let before = events(&fixture);
    let confirm = fixture.app.view.message_edit.confirm.clone().expect("it asks again");
    assert!(confirm.recomputed && !confirm.stopping && !confirm.stops_turn);
    assert_eq!(confirm.restorable, ["a.txt", "new.txt"]);
    assert_eq!(confirm.skipped.len(), 1);
    assert!(fixture.app.view.message_edit.editing.is_some());
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("agent a\n"));
    assert_eq!(disk(&fixture, "new.txt").as_deref(), Some("agent new\n"));
    assert_eq!(admitted_prompts(&before).len(), 3);
    // Answered on the final list, it goes through the idle path.
    click(&mut fixture.app, "message_edit_revert");
    assert!(fixture.app.view.message_edit.editing.is_none(), "{:?}", fixture.app.view.message_edit.confirm);
    assert_eq!(disk(&fixture, "a.txt").as_deref(), Some("original a\n"));
    assert_eq!(disk(&fixture, "b.txt").as_deref(), Some("edited by hand\n"));
    assert_eq!(admitted_prompts(&events(&fixture)), vec!["Primero, editado"]);
    settle(&mut fixture.app);
}
