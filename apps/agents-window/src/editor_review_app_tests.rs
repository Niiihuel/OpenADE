//! Real journal, editor worker and review worker integration, with private files only.
use super::*;
use crate::{code_snapshot_ui::CodeSnapshotContent, editor_review_ui, transcript::ActivityStatus};
use openide_editor::{EditKind, Selection};
use openide_ui::CodeDiffLineKind;
use serde_json::json;

const OWNER: &str = "editor-review-host";

struct Fixture {
    temporary: tempfile::TempDir,
    app: AgentsWindow,
    events: Vec<AgentEvent>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_main("fn before() {}\n")
    }

    fn with_main(before: &str) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        for (path, content) in [
            ("main.rs", before),
            ("other.rs", "fn other_before() {}\n"),
            ("deleted.rs", "fn removed() {}\n"),
        ] {
            std::fs::write(workspace.join(path), content).unwrap();
        }
        let mut app = AgentsWindow::with_data_root(temporary.path().join("data"));
        Self::configure(&mut app, &workspace, 1);
        let events = vec![AgentEvent::new(
            OWNER,
            1,
            AgentEventKind::SessionStarted {
                workspace: workspace.canonicalize().unwrap(),
            },
        )];
        session_changes::capture_baseline(
            app.store.as_ref().unwrap().storage_root(),
            OWNER,
            &workspace,
            &events,
        )
        .unwrap();
        Self {
            temporary,
            app,
            events,
        }
    }

    fn configure(app: &mut AgentsWindow, workspace: &Path, sequence: u64) {
        app.view.workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.selected_session = Some(OWNER.into());
        app.view.sidebar_collapsed = true;
        app.view.show_settings = false;
        app.view.main_page = MainPage::Workspace;
        app.last_sequence = sequence;
    }

    fn workspace(&self) -> PathBuf {
        PathBuf::from(&self.app.view.workspace)
    }

    fn storage(&self) -> PathBuf {
        self.app
            .store
            .as_ref()
            .unwrap()
            .storage_root()
            .to_path_buf()
    }

    fn write(&mut self, call: &str, path: &str, content: &str) {
        std::fs::write(self.workspace().join(path), content).unwrap();
        self.confirm(ToolCall {
            id: call.into(),
            name: "write_file".into(),
            arguments: json!({"path":path,"content":content}),
        });
    }

    fn delete(&mut self, call: &str, path: &str) {
        std::fs::remove_file(self.workspace().join(path)).unwrap();
        self.confirm(ToolCall {
            id: call.into(),
            name: "apply_patch".into(),
            arguments: json!({"patch":format!("*** Begin Patch\n*** Delete File: {path}\n*** End Patch\n")}),
        });
    }

    fn confirm(&mut self, call: ToolCall) {
        self.events.push(AgentEvent::new(
            OWNER,
            self.events.len() as u64 + 1,
            AgentEventKind::ToolStarted {
                tool_call: call.clone(),
            },
        ));
        self.events.push(AgentEvent::new(
            OWNER,
            self.events.len() as u64 + 1,
            AgentEventKind::ToolFinished {
                tool_call_id: call.id.clone(),
                output: "Confirmed fixture operation".into(),
                is_error: false,
            },
        ));
        let mut journal = Vec::new();
        for event in &self.events {
            serde_json::to_writer(&mut journal, event).unwrap();
            journal.push(b'\n');
        }
        std::fs::write(self.storage().join(format!("{OWNER}.jsonl")), journal).unwrap();
        self.app.last_sequence = self.events.last().unwrap().sequence;
        self.app.view.transcript.push(TranscriptItem::Tool {
            id: call.id,
            name: call.name,
            arguments: call.arguments,
            output: Some("Confirmed fixture operation".into()),
            is_error: false,
            activity: ActivityStatus::Completed,
        });
    }

    fn reopen_application(&mut self) {
        let transcript = self.app.view.transcript.clone();
        let mut app = AgentsWindow::with_data_root(self.temporary.path().join("data"));
        Self::configure(&mut app, &self.workspace(), self.app.last_sequence);
        app.view.transcript = transcript;
        self.app = app;
    }

    fn persisted_review(&self, call: &str, path: &str) -> FileReview {
        session_changes::load_file_review(&self.storage(), OWNER, &self.workspace(), call, path)
            .unwrap()
    }
}

fn wait(app: &mut AgentsWindow) {
    let context = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&context);
        app.poll_editor_review(&context);
        let editor_ready = app
            .view
            .code_editor
            .as_ref()
            .is_none_or(|view| !view.loading && !view.saving && view.controller.is_some());
        if editor_ready
            && !app.view.code_editor_pending_saves
            && app.editor_review.pending.is_none()
            && app.editor_review.key == app.editor_review_key()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "editor/review worker did not settle: {}",
            app.view.status
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn open(app: &mut AgentsWindow, path: &str) {
    let scope = app.dock_scope();
    let tab = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    assert!(app.dock.activate(&scope, tab));
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    wait(app);
    assert_eq!(app.view.code_editor.as_ref().unwrap().path, path);
}

fn click(app: &mut AgentsWindow, id: String) {
    app.apply_action(UiAction::Clicked { id }, &egui::Context::default());
}

fn accept_action(app: &AgentsWindow) -> String {
    file_decision_action(app, "accept_file")
}

fn file_decision_action(app: &AgentsWindow, operation: &str) -> String {
    let state = app
        .view
        .editor_workbench
        .review
        .as_ref()
        .or_else(|| {
            app.view
                .code_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.review.as_ref())
        })
        .expect("pending review is visible");
    editor_review_ui::action(&state.token, operation)
}

#[test]
fn acceptance_clears_editor_markers_reopens_durably_and_a_new_edit_is_pending() {
    let mut fixture = Fixture::new();
    fixture.write("first", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert!(review.can_accept && !review.busy);
    assert_eq!((review.additions, review.deletions), (1, 1));
    assert_eq!(review.hunks.len(), 1);
    let old_version = fixture
        .app
        .editor_review
        .loaded
        .as_ref()
        .unwrap()
        .version
        .clone();
    let journal_path = fixture.storage().join(format!("{OWNER}.jsonl"));
    let journal = std::fs::read(&journal_path).unwrap();
    let action = accept_action(&fixture.app);
    click(&mut fixture.app, action);
    assert!(
        fixture.app.editor_review.pending.is_some(),
        "acceptance is off-thread"
    );
    assert!(
        fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .busy
    );
    wait(&mut fixture.app);
    assert!(
        fixture.app.view.editor_workbench.review.is_none(),
        "accepted decorations disappear"
    );
    assert!(fixture.persisted_review("first", "main.rs").accepted);
    assert_eq!(std::fs::read(&journal_path).unwrap(), journal);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        "fn after() {}\n"
    );

    // No in-memory host state survives reopening the application.
    fixture.reopen_application();
    open(&mut fixture.app, "main.rs");
    assert!(fixture.app.editor_review.loaded.as_ref().unwrap().accepted);
    assert!(fixture.app.view.editor_workbench.review.is_none());

    fixture.write("second", "main.rs", "fn newer() {}\n");
    fixture.app.refresh_clean_code_editor();
    wait(&mut fixture.app);
    let loaded = fixture.app.editor_review.loaded.as_ref().unwrap();
    assert_eq!(loaded.tool_call_id, "second");
    assert!(!loaded.accepted);
    assert_ne!(loaded.version, old_version);
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert!(review.can_accept && !review.busy);
    assert_eq!(
        review.hunks[0].removed_lines.as_ref(),
        &["fn after() {}".to_owned()]
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        "fn newer() {}\n"
    );
}

#[test]
fn old_tab_action_running_turn_and_unsaved_buffer_cannot_accept_another_version() {
    let mut fixture = Fixture::new();
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    fixture.write("other-edit", "other.rs", "fn other_after() {}\n");
    open(&mut fixture.app, "main.rs");
    let stale_action = accept_action(&fixture.app);
    open(&mut fixture.app, "other.rs");
    let active_token = fixture.app.editor_review.token.clone();
    click(&mut fixture.app, stale_action);
    assert!(fixture.app.editor_review.pending.is_none());
    assert_eq!(fixture.app.editor_review.token, active_token);
    assert!(!fixture.persisted_review("main-edit", "main.rs").accepted);
    assert!(!fixture.persisted_review("other-edit", "other.rs").accepted);

    // The same action becomes invalid immediately when a run begins, before
    // another frame has repainted its disabled button.
    let action_before_run = accept_action(&fixture.app);
    fixture.app.view.running = true;
    click(&mut fixture.app, action_before_run);
    assert!(fixture.app.editor_review.pending.is_none());
    wait(&mut fixture.app);
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert!(!review.can_accept);
    assert!(review.reason.as_deref().unwrap().contains("ejecución"));
    let disabled_action = accept_action(&fixture.app);
    click(&mut fixture.app, disabled_action);
    assert!(fixture.app.editor_review.pending.is_none());
    fixture.app.view.running = false;
    wait(&mut fixture.app);
    assert!(
        fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .can_accept
    );

    let action_before_typing = accept_action(&fixture.app);
    let controller = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .clone();
    let shared = controller.shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let end = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(end)).unwrap();
    buffer
        .insert("// my unsaved edit\n", EditKind::Paste, 10)
        .unwrap();
    drop(buffer);
    click(&mut fixture.app, action_before_typing);
    assert!(fixture.app.editor_review.pending.is_none());
    wait(&mut fixture.app);
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert!(!review.can_accept && review.hunks.is_empty());
    assert!(controller.snapshot().unwrap().is_dirty());
    let disabled_action = accept_action(&fixture.app);
    click(&mut fixture.app, disabled_action);
    assert!(fixture.app.editor_review.pending.is_none());
    assert!(!fixture.persisted_review("other-edit", "other.rs").accepted);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("other.rs")).unwrap(),
        "fn other_after() {}\n"
    );
}

#[test]
fn deleted_file_opens_confirmed_snapshot_and_acceptance_never_recreates_it() {
    let mut fixture = Fixture::new();
    fixture.delete("delete", "deleted.rs");
    click(
        &mut fixture.app,
        crate::file_activity_ui::open_action(OWNER, "delete", 0),
    );
    wait(&mut fixture.app);
    assert!(fixture.app.view.code_editor.is_none());
    let snapshot = fixture.app.view.code_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.path, "deleted.rs");
    let review = snapshot.review.as_ref().unwrap();
    assert!(review.can_accept && !review.busy);
    assert_eq!((review.additions, review.deletions), (0, 1));
    let CodeSnapshotContent::Diff { lines, .. } = &snapshot.content else {
        panic!("confirmed deletion must display the immutable deleted lines");
    };
    assert!(lines
        .iter()
        .any(|line| line.kind == CodeDiffLineKind::Removed
            && line.text == "fn removed() {}"
            && line.old_line == Some(1)));
    let action = accept_action(&fixture.app);
    click(&mut fixture.app, action);
    assert!(
        fixture
            .app
            .view
            .code_snapshot
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
            .busy
    );
    wait(&mut fixture.app);
    let snapshot = fixture.app.view.code_snapshot.as_ref().unwrap();
    assert!(snapshot.review.is_none());
    assert!(snapshot.caption.contains("revisados"));
    assert!(fixture.persisted_review("delete", "deleted.rs").accepted);
    assert!(!fixture.workspace().join("deleted.rs").exists());
    assert!(
        matches!(&snapshot.content, CodeSnapshotContent::Diff { lines, .. }
        if lines.iter().any(|line| line.kind == CodeDiffLineKind::Removed))
    );
}

#[test]
fn block_decisions_preserve_other_edits_reload_a_clean_buffer_and_survive_reopening() {
    let before = "fn first_before() {}\n// one\n// two\n// three\n// four\n// five\n// six\n// seven\n// eight\nfn last_before() {}\n";
    let after = before
        .replace("first_before", "first_after")
        .replace("last_before", "last_after");
    let expected = before.replace("first_before", "first_after");
    let mut fixture = Fixture::with_main(before);
    fixture.write("two-blocks", "main.rs", &after);
    open(&mut fixture.app, "main.rs");
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert_eq!(
        review.actions.len(),
        2,
        "separated source changes are independent decisions"
    );
    let accept = review.actions[0].accept_id.clone();
    let stale_reject = review.actions[1].reject_id.clone();
    let journal = std::fs::read(fixture.storage().join(format!("{OWNER}.jsonl"))).unwrap();
    click(&mut fixture.app, accept);
    assert!(
        fixture.app.editor_review.pending.is_some(),
        "block acceptance runs on its worker"
    );
    wait(&mut fixture.app);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        after,
        "accepting only folds a block into the comparison base"
    );
    let pending = fixture.persisted_review("two-blocks", "main.rs");
    assert_eq!(pending.blocks.len(), 1);
    assert!(!pending.resolved);
    assert_eq!(pending.before.as_deref(), Some(before));
    assert_eq!(
        pending.after.as_deref(),
        Some(after.as_str()),
        "immutable evidence is retained"
    );
    click(&mut fixture.app, stale_reject);
    assert!(
        fixture.app.editor_review.pending.is_none(),
        "a pre-decision block identity cannot decide a remapped block"
    );

    fixture.reopen_application();
    open(&mut fixture.app, "main.rs");
    let review = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert_eq!(review.actions.len(), 1);
    assert!(review.can_reject && review.actions[0].reject_enabled);
    let reject = review.actions[0].reject_id.clone();
    click(&mut fixture.app, reject);
    assert!(fixture.app.editor_review.pending.is_some());
    wait(&mut fixture.app);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        expected,
        "rejecting the pending block preserves the accepted one"
    );
    let snapshot = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert!(
        !snapshot.is_dirty(),
        "the clean editor reloads the confirmed replacement"
    );
    assert_eq!(
        snapshot.text_range(0..snapshot.char_count()).unwrap(),
        expected
    );
    assert!(fixture.app.view.editor_workbench.review.is_none());
    let resolved = fixture.persisted_review("two-blocks", "main.rs");
    assert!(resolved.resolved && !resolved.accepted);
    assert!(resolved.blocks.is_empty());
    assert_eq!(resolved.current.as_deref(), Some(expected.as_str()));
    assert_eq!(
        std::fs::read(fixture.storage().join(format!("{OWNER}.jsonl"))).unwrap(),
        journal
    );

    fixture.reopen_application();
    open(&mut fixture.app, "main.rs");
    assert!(fixture.app.view.editor_workbench.review.is_none());
    let snapshot = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert!(!snapshot.is_dirty());
    assert_eq!(
        snapshot.text_range(0..snapshot.char_count()).unwrap(),
        expected
    );
}

#[test]
fn stale_tab_and_new_dirty_revision_block_hunk_decisions_without_disk_or_draft_mutation() {
    let mut fixture = Fixture::new();
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    let before = fixture.persisted_review("main-edit", "main.rs");
    let stale = fixture
        .app
        .view
        .editor_workbench
        .review
        .as_ref()
        .unwrap()
        .actions[0]
        .clone();
    open(&mut fixture.app, "other.rs");
    for id in [stale.accept_id, stale.reject_id] {
        click(&mut fixture.app, id);
        assert!(fixture.app.editor_review.pending.is_none());
    }
    assert_eq!(
        fixture
            .persisted_review("main-edit", "main.rs")
            .decision_version,
        before.decision_version
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        "fn after() {}\n"
    );

    open(&mut fixture.app, "main.rs");
    let prior = fixture
        .app
        .view
        .editor_workbench
        .review
        .as_ref()
        .unwrap()
        .actions[0]
        .clone();
    let controller = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .clone();
    let shared = controller.shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let end = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(end)).unwrap();
    buffer
        .insert("// café 🧠 unsaved\n", EditKind::Paste, 15)
        .unwrap();
    drop(buffer);
    let draft = controller.snapshot().unwrap();
    for id in [prior.reject_id, prior.accept_id] {
        click(&mut fixture.app, id);
        assert!(
            fixture.app.editor_review.pending.is_none(),
            "no stale hunk starts a write or metadata worker"
        );
    }
    wait(&mut fixture.app);
    let current = controller.snapshot().unwrap();
    assert_eq!(
        (current.revision(), current.selection()),
        (draft.revision(), draft.selection())
    );
    assert!(current.is_dirty());
    assert_eq!(
        current.text_range(0..current.char_count()).unwrap(),
        draft.text_range(0..draft.char_count()).unwrap()
    );
    assert_eq!(
        fixture
            .persisted_review("main-edit", "main.rs")
            .decision_version,
        before.decision_version
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        "fn after() {}\n"
    );
    let disabled = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert!(!disabled.can_accept && !disabled.can_reject && disabled.actions.is_empty());
}

#[test]
fn rejection_delivered_after_switching_tabs_refreshes_only_the_affected_cached_document() {
    let mut fixture = Fixture::new();
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    let action = file_decision_action(&fixture.app, "reject_file");
    click(&mut fixture.app, action);
    assert!(fixture.app.editor_review.pending.is_some());

    // `open` changes the dock selection before its first worker poll. The
    // response for A must reconcile A's cache without opening it over B.
    open(&mut fixture.app, "other.rs");
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "other.rs"
    );
    let other = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        other.text_range(0..other.char_count()).unwrap(),
        "fn other_before() {}\n"
    );
    assert!(!other.is_dirty());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        "fn before() {}\n"
    );
    assert!(fixture.persisted_review("main-edit", "main.rs").resolved);

    open(&mut fixture.app, "main.rs");
    let restored = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        restored.text_range(0..restored.char_count()).unwrap(),
        "fn before() {}\n",
        "reopening A must not reuse the old post-agent contents"
    );
    assert!(!restored.is_dirty());
    assert!(fixture
        .app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .error
        .is_none());
    assert!(fixture.app.view.editor_workbench.review.is_none());
}

#[test]
fn rejecting_a_new_file_closes_its_clean_tab_and_reopening_reads_new_disk_content() {
    let mut fixture = Fixture::new();
    fixture.write("create", "new.rs", "fn generated() {}\n");
    open(&mut fixture.app, "new.rs");
    let created = fixture.app.view.code_editor.as_ref().unwrap();
    let tab = created.tab_id;
    let document = created
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap()
        .document_id();
    let action = file_decision_action(&fixture.app, "reject_file");
    click(&mut fixture.app, action);
    wait(&mut fixture.app);
    assert!(!fixture.workspace().join("new.rs").exists());
    assert!(
        !fixture.app.dock.contains_tab(tab),
        "a rejected creation has no editable file tab"
    );
    assert!(
        fixture
            .app
            .code_editor
            .view_for_path(&fixture.workspace(), Path::new("new.rs"))
            .is_none(),
        "retire the clean cached document as well as its tab"
    );
    assert!(fixture
        .app
        .view
        .code_editor
        .as_ref()
        .is_none_or(|view| view.path != "new.rs"));
    assert!(fixture.persisted_review("create", "new.rs").resolved);

    // A later manual creation is a different document, never a resurrection
    // of the rejected agent buffer retained under the same path.
    std::fs::write(
        fixture.workspace().join("new.rs"),
        "fn manually_recreated() {}\n",
    )
    .unwrap();
    open(&mut fixture.app, "new.rs");
    let reopened = fixture.app.view.code_editor.as_ref().unwrap();
    let snapshot = reopened.controller.as_ref().unwrap().snapshot().unwrap();
    assert_ne!(snapshot.document_id(), document);
    assert_ne!(reopened.tab_id, tab);
    assert!(!snapshot.is_dirty());
    assert_eq!(
        snapshot.text_range(0..snapshot.char_count()).unwrap(),
        "fn manually_recreated() {}\n"
    );
    assert!(reopened.error.is_none());
}

#[test]
fn rejecting_a_deleted_file_from_its_snapshot_opens_the_restored_editable_document() {
    let mut fixture = Fixture::new();
    fixture.delete("delete", "deleted.rs");
    click(
        &mut fixture.app,
        crate::file_activity_ui::open_action(OWNER, "delete", 0),
    );
    wait(&mut fixture.app);
    let historical_tab = fixture.app.view.code_snapshot.as_ref().unwrap().tab_id;
    assert!(fixture.app.view.code_editor.is_none());
    let action = file_decision_action(&fixture.app, "reject_file");
    click(&mut fixture.app, action);
    wait(&mut fixture.app);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("deleted.rs")).unwrap(),
        "fn removed() {}\n"
    );
    assert!(
        fixture.app.view.code_snapshot.is_none(),
        "show the restored document after the explicit rejection"
    );
    let view = fixture
        .app
        .view
        .code_editor
        .as_ref()
        .expect("restored file opens in the ordinary editor");
    assert_eq!(view.path, "deleted.rs");
    assert_ne!(view.tab_id, historical_tab);
    assert!(view.error.is_none());
    let snapshot = view.controller.as_ref().unwrap().snapshot().unwrap();
    assert!(!snapshot.is_dirty());
    assert_eq!(
        snapshot.text_range(0..snapshot.char_count()).unwrap(),
        "fn removed() {}\n"
    );
    assert!(fixture.app.view.editor_workbench.review.is_none());
    let review = fixture.persisted_review("delete", "deleted.rs");
    assert!(review.resolved && !review.accepted);
    assert_eq!(review.current.as_deref(), Some("fn removed() {}\n"));
    assert!(
        fixture.app.dock.contains_tab(historical_tab),
        "the recorded deletion remains available as history"
    );
    assert!(
        matches!(&fixture.app.code_snapshots[&historical_tab].content,
        CodeSnapshotContent::Diff { lines, .. } if lines.iter().any(|line| line.kind == CodeDiffLineKind::Removed
            && line.text == "fn removed() {}"))
    );
}

/// Deliver the same scoped background snapshot used when the Changes tab is
/// closed. The review entry point must request it without opening that tab.
fn wait_for_review_files(app: &mut AgentsWindow, count: usize) {
    let context = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update_environment(&context);
        app.poll_code_editor(&context);
        app.poll_editor_review(&context);
        if !app.view.session_changes.loading
            && app
                .view
                .editor_workbench
                .review
                .as_ref()
                .is_some_and(|review| review.file_count == count)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "confirmed review files did not arrive"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn file_navigation_uses_confirmed_session_changes_and_stops_at_both_edges() {
    let mut fixture = Fixture::new();
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    fixture.write("other-edit", "other.rs", "fn other_after() {}\n");
    std::fs::write(
        fixture.workspace().join("unrelated.rs"),
        "// human change\n",
    )
    .unwrap();
    open(&mut fixture.app, "main.rs");
    assert!(
        fixture.app.view.session_changes.loading,
        "direct file review requests the existing worker"
    );
    wait_for_review_files(&mut fixture.app, 2);
    let state = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert_eq!((state.current_file, state.file_count), (0, 2));
    let tab = fixture.app.view.code_editor.as_ref().unwrap().tab_id;
    let previous = file_decision_action(&fixture.app, "previous_file");
    click(&mut fixture.app, previous);
    assert_eq!(fixture.app.view.code_editor.as_ref().unwrap().tab_id, tab);
    let stale_next = file_decision_action(&fixture.app, "next_file");
    click(&mut fixture.app, stale_next.clone());
    wait(&mut fixture.app);
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "other.rs"
    );
    let state = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert_eq!((state.current_file, state.file_count), (1, 2));
    click(&mut fixture.app, stale_next);
    let next = file_decision_action(&fixture.app, "next_file");
    click(&mut fixture.app, next);
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "other.rs"
    );
    let previous = file_decision_action(&fixture.app, "previous_file");
    click(&mut fixture.app, previous);
    wait(&mut fixture.app);
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "main.rs"
    );
    assert_eq!(
        fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .current_file,
        0
    );
    assert!(!fixture.persisted_review("main-edit", "main.rs").accepted);
    assert!(!fixture.persisted_review("other-edit", "other.rs").accepted);
}

#[test]
fn file_navigation_opens_deleted_files_as_confirmed_snapshots_and_returns_to_editor() {
    let mut fixture = Fixture::new();
    fixture.delete("delete", "deleted.rs");
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    wait_for_review_files(&mut fixture.app, 2);
    let previous = file_decision_action(&fixture.app, "previous_file");
    click(&mut fixture.app, previous);
    wait(&mut fixture.app);
    let snapshot = fixture.app.view.code_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.path, "deleted.rs");
    let review = snapshot.review.as_ref().unwrap();
    assert_eq!((review.current_file, review.file_count), (0, 2));
    assert!(!fixture.workspace().join("deleted.rs").exists());
    let next = file_decision_action(&fixture.app, "next_file");
    click(&mut fixture.app, next);
    wait(&mut fixture.app);
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "main.rs"
    );
    assert_eq!(
        fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .current_file,
        1
    );
}

#[test]
fn file_navigation_rejects_loading_foreign_pending_and_dirty_surfaces() {
    let mut fixture = Fixture::new();
    fixture.write("main-edit", "main.rs", "fn after() {}\n");
    fixture.write("other-edit", "other.rs", "fn other_after() {}\n");
    open(&mut fixture.app, "main.rs");
    wait_for_review_files(&mut fixture.app, 2);
    let next = file_decision_action(&fixture.app, "next_file");
    let key = fixture.app.editor_review_key().unwrap();
    fixture.app.view.session_changes.loading = true;
    click(&mut fixture.app, next.clone());
    assert_eq!(fixture.app.editor_review_file_position(&key), (1, 0));
    fixture.app.view.session_changes.loading = false;
    fixture.app.view.session_changes.session_id = Some("another-chat".into());
    click(&mut fixture.app, next.clone());
    fixture.app.view.session_changes.session_id = Some(OWNER.into());
    let snapshot = fixture.app.view.session_changes.result.clone();
    let Some(Ok(changes)) = fixture.app.view.session_changes.result.as_mut() else {
        panic!("snapshot");
    };
    Arc::make_mut(changes).session_id = "another-chat".into();
    click(&mut fixture.app, next.clone());
    fixture.app.view.session_changes.result = snapshot.clone();
    let Some(Ok(changes)) = fixture.app.view.session_changes.result.as_mut() else {
        panic!("snapshot");
    };
    Arc::make_mut(changes).workspace = fixture.temporary.path().join("another-workspace");
    click(&mut fixture.app, next.clone());
    fixture.app.view.session_changes.result = snapshot;
    let (_tx, rx) = std::sync::mpsc::channel();
    fixture.app.editor_review.pending = Some(rx);
    click(&mut fixture.app, next.clone());
    fixture.app.editor_review.pending = None;
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "main.rs"
    );
    for field in 0..3 {
        let editor = fixture.app.view.code_editor.as_mut().unwrap();
        editor.saving = field == 0;
        editor.loading = field == 1;
        editor.conflict = field == 2;
        click(&mut fixture.app, next.clone());
        assert_eq!(
            fixture.app.view.code_editor.as_ref().unwrap().path,
            "main.rs"
        );
    }
    let editor = fixture.app.view.code_editor.as_mut().unwrap();
    editor.saving = false;
    editor.loading = false;
    editor.conflict = false;
    let controller = editor.controller.as_ref().unwrap().clone();
    controller
        .shared_buffer()
        .lock()
        .unwrap()
        .insert("// unsaved\n", EditKind::Paste, 30)
        .unwrap();
    wait(&mut fixture.app);
    click(&mut fixture.app, next);
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "main.rs"
    );
    assert!(controller.snapshot().unwrap().is_dirty());
}

#[test]
fn hunk_navigation_has_unique_inline_actions_and_does_not_wrap() {
    let before = "fn first_before() {}\n// one\n// two\n// three\n// four\n// five\n// six\n// seven\n// eight\nfn last_before() {}\n";
    let after = before
        .replace("first_before", "first_after")
        .replace("last_before", "last_after");
    let mut fixture = Fixture::with_main(before);
    fixture.write("two-blocks", "main.rs", &after);
    open(&mut fixture.app, "main.rs");
    let state = fixture.app.view.editor_workbench.review.as_ref().unwrap();
    assert_eq!(state.hunk_count, 2);
    let inline_next = state.actions[0].next_id.clone().unwrap();
    assert_ne!(inline_next, file_decision_action(&fixture.app, "next"));
    let previous = file_decision_action(&fixture.app, "previous");
    click(&mut fixture.app, previous);
    assert_eq!(fixture.app.editor_review.current_hunk, 0);
    click(&mut fixture.app, inline_next.clone());
    assert_eq!(fixture.app.editor_review.current_hunk, 1);
    click(&mut fixture.app, inline_next);
    assert_eq!(
        fixture.app.editor_review.current_hunk, 1,
        "old inline block cannot navigate a newly active block"
    );
    let next = file_decision_action(&fixture.app, "next");
    click(&mut fixture.app, next);
    assert_eq!(
        fixture.app.editor_review.current_hunk, 1,
        "last hunk does not wrap"
    );
    let previous = fixture
        .app
        .view
        .editor_workbench
        .review
        .as_ref()
        .unwrap()
        .actions[1]
        .previous_id
        .clone()
        .unwrap();
    click(&mut fixture.app, previous);
    assert_eq!(fixture.app.editor_review.current_hunk, 0);
}

fn layout_review_keys(app: &mut AgentsWindow, width: f32) -> String {
    let (id, surface) = if let Some(editor) = &app.view.code_editor {
        (
            crate::code_editor_ui::view_id(editor.tab_id),
            crate::code_editor_ui::build(
                editor,
                &app.view.editor_workbench,
                width,
                app.view.keymap_profile,
                app.view.vim_mode,
            ),
        )
    } else {
        let snapshot = app.view.code_snapshot.as_ref().unwrap();
        (
            crate::code_snapshot_ui::view_id(snapshot.tab_id),
            crate::code_snapshot_ui::build(snapshot, width),
        )
    };
    // Other focus owners coexist with the editor; the terminal target needs no
    // real process to verify that review routing leaves its native events alone.
    app.tree.set_root(openide_ui::Element::column(vec![
        surface.flex(1.0),
        openide_ui::Element::text_input("prompt", "draft remains", ""),
        openide_ui::Element::text_input("editor_query", "needle", ""),
        openide_ui::Element::button(app.active_terminal_screen_id(), "terminal focus fixture"),
    ]));
    app.tree.layout(openide_ui::Size::new(width, 640.0));
    assert!(app.tree.focus(&id), "review surface is focusable: {id}");
    id
}

fn review_key(key: egui::Key, primary: bool, shift: bool, alt: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            ctrl: primary && !cfg!(target_os = "macos"),
            mac_cmd: primary && cfg!(target_os = "macos"),
            command: primary,
            shift,
            alt,
        },
    }
}

fn dispatch_review_key(
    app: &mut AgentsWindow,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    modal: bool,
    menu: bool,
) -> (bool, Vec<egui::Event>) {
    ctx.begin_pass(egui::RawInput {
        events,
        ..Default::default()
    });
    let changed = app.handle_keymap_shortcuts(ctx, modal, menu).0;
    let remaining = ctx.input(|input| input.events.clone());
    ctx.end_pass().textures_delta.clear();
    (changed, remaining)
}

#[test]
fn review_shortcuts_decide_only_the_current_file_in_ide_and_agents() {
    for ide in [false, true] {
        for reject in [false, true] {
            let mut fixture = Fixture::new();
            fixture.write("first", "main.rs", "fn after() {}\n");
            fixture.write("other", "other.rs", "fn other_after() {}\n");
            if ide {
                fixture.app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
            }
            open(&mut fixture.app, "main.rs");
            layout_review_keys(&mut fixture.app, 620.0);
            let ctx = egui::Context::default();
            let event = review_key(
                if reject {
                    egui::Key::Backspace
                } else {
                    egui::Key::Enter
                },
                true,
                reject,
                false,
            );
            let (handled, remaining) =
                dispatch_review_key(&mut fixture.app, &ctx, vec![event], false, false);
            assert!(handled && remaining.is_empty());
            assert!(fixture.app.editor_review.pending.is_some());
            wait(&mut fixture.app);
            let reviewed = fixture.persisted_review("first", "main.rs");
            assert!(reviewed.resolved);
            assert_eq!(reviewed.accepted, !reject);
            assert!(!fixture.persisted_review("other", "other.rs").resolved);
            assert_eq!(
                std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
                if reject {
                    "fn before() {}\n"
                } else {
                    "fn after() {}\n"
                }
            );
            assert_eq!(
                std::fs::read_to_string(fixture.workspace().join("other.rs")).unwrap(),
                "fn other_after() {}\n"
            );
        }
    }
}

#[test]
fn review_shortcuts_reject_deleted_snapshot_via_the_guarded_restore_worker() {
    let mut fixture = Fixture::new();
    fixture.delete("delete", "deleted.rs");
    click(
        &mut fixture.app,
        crate::file_activity_ui::open_action(OWNER, "delete", 0),
    );
    wait(&mut fixture.app);
    layout_review_keys(&mut fixture.app, 360.0);
    let ctx = egui::Context::default();
    let result = dispatch_review_key(
        &mut fixture.app,
        &ctx,
        vec![review_key(egui::Key::Backspace, true, true, false)],
        false,
        false,
    );
    assert!(result.0 && result.1.is_empty());
    wait(&mut fixture.app);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("deleted.rs")).unwrap(),
        "fn removed() {}\n"
    );
    assert_eq!(
        fixture.app.view.code_editor.as_ref().unwrap().path,
        "deleted.rs"
    );
}

#[test]
fn review_shortcuts_preserve_editing_chords_and_extra_modifiers() {
    let mut fixture = Fixture::new();
    fixture.write("edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    layout_review_keys(&mut fixture.app, 620.0);
    for (key, primary, shift, alt) in [
        (egui::Key::Enter, false, false, false),
        (egui::Key::Enter, true, true, false),
        (egui::Key::Enter, true, false, true),
        (egui::Key::Backspace, true, false, false),
        (egui::Key::Backspace, true, true, true),
        (egui::Key::Backspace, false, true, false),
        (egui::Key::F7, true, false, false),
        (egui::Key::F7, false, false, true),
    ] {
        let event = review_key(key, primary, shift, alt);
        let (handled, remaining) = dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event.clone()],
            false,
            false,
        );
        assert!(!handled, "unexpected review chord {event:?}");
        assert_eq!(remaining, vec![event]);
    }
    assert!(fixture.app.editor_review.pending.is_none());
    assert!(!fixture.persisted_review("edit", "main.rs").resolved);
}

#[test]
fn review_shortcuts_do_not_steal_composer_search_terminal_or_overlay_keys() {
    let mut fixture = Fixture::new();
    fixture.write("edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    let editor_id = layout_review_keys(&mut fixture.app, 620.0);
    let event = review_key(egui::Key::Enter, true, false, false);
    for focus in [
        "prompt".to_owned(),
        "editor_query".to_owned(),
        fixture.app.active_terminal_screen_id(),
    ] {
        assert!(fixture.app.tree.focus(&focus));
        let (handled, remaining) = dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event.clone()],
            false,
            false,
        );
        assert!(
            !handled && remaining == vec![event.clone()],
            "focus owner {focus}"
        );
    }
    assert!(fixture.app.tree.focus(&editor_id));
    for (modal, menu) in [(true, false), (false, true)] {
        let (handled, remaining) = dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event.clone()],
            modal,
            menu,
        );
        assert!(!handled && remaining == vec![event.clone()]);
    }
    fixture.app.view.show_command_center = true;
    assert!(
        !dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event.clone()],
            false,
            false
        )
        .0
    );
    fixture.app.view.show_command_center = false;
    fixture.app.view.show_settings = true;
    assert!(
        !dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event.clone()],
            false,
            false
        )
        .0
    );
    fixture.app.view.show_settings = false;
    fixture.app.view.editor_workbench.review = None;
    assert!(
        !dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![event],
            false,
            false
        )
        .0
    );
    assert!(fixture.app.editor_review.pending.is_none());
    assert!(!fixture.persisted_review("edit", "main.rs").resolved);
}

#[test]
fn review_shortcut_autorepeat_and_release_cannot_leak_after_focus_changes() {
    let mut fixture = Fixture::new();
    fixture.write("edit", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    layout_review_keys(&mut fixture.app, 620.0);
    let ctx = egui::Context::default();
    let event = review_key(egui::Key::Enter, true, false, false);
    assert!(dispatch_review_key(&mut fixture.app, &ctx, vec![event.clone()], false, false).0);
    wait(&mut fixture.app);
    assert!(fixture.app.tree.focus("prompt"));
    let mut repeat = event.clone();
    if let egui::Event::Key { repeat, .. } = &mut repeat {
        *repeat = true;
    }
    let mut release = event;
    if let egui::Event::Key { pressed, .. } = &mut release {
        *pressed = false;
    }
    let (handled, remaining) =
        dispatch_review_key(&mut fixture.app, &ctx, vec![repeat, release], true, false);
    assert!(
        !handled && remaining.is_empty(),
        "held review key never becomes a prompt submission"
    );
}

#[test]
fn review_shortcuts_navigate_real_hunks_and_keep_disabled_decisions_guarded() {
    let before = "fn first_before() {}\n// one\n// two\n// three\n// four\n// five\n// six\n// seven\n// eight\nfn last_before() {}\n";
    let after = before
        .replace("first_before", "first_after")
        .replace("last_before", "last_after");
    let mut fixture = Fixture::with_main(before);
    fixture.write("edit", "main.rs", &after);
    open(&mut fixture.app, "main.rs");
    layout_review_keys(&mut fixture.app, 620.0);
    for (shift, expected) in [(false, 1), (false, 1), (true, 0)] {
        let (handled, remaining) = dispatch_review_key(
            &mut fixture.app,
            &egui::Context::default(),
            vec![review_key(egui::Key::F7, false, shift, false)],
            false,
            false,
        );
        assert!(handled && remaining.is_empty());
        assert_eq!(
            fixture
                .app
                .view
                .editor_workbench
                .review
                .as_ref()
                .unwrap()
                .current_hunk,
            expected
        );
    }
    fixture.app.view.running = true;
    fixture.app.poll_editor_review(&egui::Context::default());
    // Complete the read-only refresh before checking the disabled action.
    wait(&mut fixture.app);
    assert!(
        !fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .can_accept
    );
    let (handled, remaining) = dispatch_review_key(
        &mut fixture.app,
        &egui::Context::default(),
        vec![review_key(egui::Key::Backspace, true, true, false)],
        false,
        false,
    );
    assert!(handled && remaining.is_empty());
    assert!(fixture.app.editor_review.pending.is_none());
    assert!(!fixture.persisted_review("edit", "main.rs").resolved);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("main.rs")).unwrap(),
        after
    );
}

#[test]
fn review_navigation_menu_is_scoped_dismissible_and_closes_before_modal_input() {
    let mut fixture = Fixture::new();
    fixture.write("edit", "main.rs", "fn after() {}\n");
    fixture.write("other", "other.rs", "fn other_after() {}\n");
    open(&mut fixture.app, "main.rs");
    wait_for_review_files(&mut fixture.app, 2);
    let toggle = file_decision_action(&fixture.app, "menu_toggle");
    click(&mut fixture.app, toggle.clone());
    assert!(
        fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .menu_open
    );
    // Render the real overflow popup in a narrow editor. Escape remains a
    // shared DropdownMenu action, scoped to the currently displayed token.
    let editor = fixture.app.view.code_editor.as_ref().unwrap();
    fixture.app.tree.set_root(crate::code_editor_ui::build(
        editor,
        &fixture.app.view.editor_workbench,
        280.0,
        fixture.app.view.keymap_profile,
        false,
    ));
    fixture.app.tree.layout(openide_ui::Size::new(280.0, 480.0));
    let actions = fixture.app.tree.handle_event(openide_ui::UiEvent::Key {
        key: openide_ui::Key::Escape,
        shift: false,
        ctrl: false,
    });
    let dismiss = file_decision_action(&fixture.app, "menu_dismiss");
    assert!(actions
        .iter()
        .any(|action| matches!(action, UiAction::Clicked { id } if id == &dismiss)));
    for action in actions {
        fixture.app.apply_action(action, &egui::Context::default());
    }
    assert!(
        !fixture
            .app
            .view
            .editor_workbench
            .review
            .as_ref()
            .unwrap()
            .menu_open
    );
    click(&mut fixture.app, toggle.clone());
    click(&mut fixture.app, "editor_find".into());
    assert!(!fixture.app.editor_review.menu_open);
    assert!(fixture.app.view.editor_workbench.search_open);
    assert_eq!(
        fixture.app.editor_services.focus_next.as_deref(),
        Some("editor_query"),
        "closing the overflow must preserve the action's destination focus"
    );
    click(&mut fixture.app, "editor_search_close".into());
    click(&mut fixture.app, toggle.clone());
    let event = review_key(egui::Key::Enter, true, false, false);
    let (_, remaining) = dispatch_review_key(
        &mut fixture.app,
        &egui::Context::default(),
        vec![event.clone()],
        true,
        false,
    );
    assert_eq!(remaining, vec![event], "modal keeps its confirmation key");
    assert!(!fixture.app.editor_review.menu_open);
    click(&mut fixture.app, toggle.clone());
    open(&mut fixture.app, "other.rs");
    assert!(!fixture.app.editor_review.menu_open);
    click(&mut fixture.app, toggle);
    assert!(
        !fixture.app.editor_review.menu_open,
        "old tab cannot reopen its menu"
    );
}

#[test]
fn review_overflow_closes_before_opening_a_markdown_preview() {
    let mut fixture = Fixture::new();
    fixture.write("markdown", "notes.md", "# Review notes\n");
    open(&mut fixture.app, "notes.md");
    let toggle = file_decision_action(&fixture.app, "menu_toggle");
    click(&mut fixture.app, toggle);
    assert!(fixture.app.editor_review.menu_open);
    let tab = fixture.app.view.code_editor.as_ref().unwrap().tab_id;
    click(&mut fixture.app, format!("markdown_preview_open:{tab}"));
    assert!(!fixture.app.editor_review.menu_open);
    assert!(fixture.app.view.markdown_preview.is_some());
    assert!(
        !fixture.persisted_review("markdown", "notes.md").resolved,
        "opening a preview must not decide the pending changes"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("notes.md")).unwrap(),
        "# Review notes\n"
    );
}

#[test]
fn review_navigation_menu_does_not_return_after_its_overflow_anchor_disappears() {
    let mut fixture = Fixture::new();
    fixture.app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    fixture.write("main", "main.rs", "fn after() {}\n");
    fixture.write("other", "other.rs", "fn other_after() {}\n");
    open(&mut fixture.app, "main.rs");
    wait_for_review_files(&mut fixture.app, 2);
    let toggle = file_decision_action(&fixture.app, "menu_toggle");
    let render = |app: &mut AgentsWindow, width| {
        app.tree.set_root(crate::code_editor_ui::build_ide(
            app.view.code_editor.as_ref().unwrap(),
            &app.view.editor_workbench,
            width,
            app.view.keymap_profile,
            false,
        ));
        app.tree.layout(openide_ui::Size::new(width, 480.0));
    };
    render(&mut fixture.app, 280.0);
    assert!(fixture
        .app
        .tree
        .frame()
        .regions
        .iter()
        .any(|region| region.id == toggle));
    click(&mut fixture.app, toggle.clone());
    fixture.app.poll_editor_review(&egui::Context::default());
    assert!(
        fixture.app.editor_review.menu_open,
        "closed trigger is a valid anchor before popup layout"
    );
    render(&mut fixture.app, 280.0);
    assert!(fixture.app.tree.has_open_popover());
    render(&mut fixture.app, 900.0);
    assert!(!fixture
        .app
        .tree
        .frame()
        .regions
        .iter()
        .any(|region| region.id == toggle));
    fixture.app.poll_editor_review(&egui::Context::default());
    assert!(!fixture.app.editor_review.menu_open);
    render(&mut fixture.app, 280.0);
    assert!(
        !fixture.app.tree.has_open_popover(),
        "resizing back never resurrects overflow"
    );
}

#[test]
fn editor_menu_entry_uses_the_combined_agents_overflow_across_resizes() {
    let mut fixture = Fixture::new();
    fixture.write("main", "main.rs", "fn after() {}\n");
    open(&mut fixture.app, "main.rs");
    click(&mut fixture.app, "editor_menu".into());
    assert!(fixture.app.editor_review.menu_open);
    assert!(!fixture.app.view.editor_workbench.menu_open);
    for width in [280.0, 900.0, 280.0] {
        fixture.app.tree.set_root(crate::code_editor_ui::build(
            fixture.app.view.code_editor.as_ref().unwrap(),
            &fixture.app.view.editor_workbench,
            width,
            fixture.app.view.keymap_profile,
            false,
        ));
        fixture
            .app
            .tree
            .layout(openide_ui::Size::new(width, 480.0));
        fixture.app.poll_editor_review(&egui::Context::default());
        assert!(fixture.app.editor_review.menu_open);
        assert!(fixture.app.tree.has_open_popover());
        assert!(fixture
            .app
            .tree
            .frame()
            .regions
            .iter()
            .any(|region| region.id == "editor_find"));
    }
    click(&mut fixture.app, "editor_menu".into());
    assert!(!fixture.app.editor_review.menu_open);
    assert!(!fixture.app.view.editor_workbench.menu_open);

    fixture.app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    click(&mut fixture.app, "editor_menu".into());
    assert!(fixture.app.view.editor_workbench.menu_open);
    assert!(!fixture.app.editor_review.menu_open);
}
