use super::*;
use openide_editor::EditKind;
use openide_ui::{Key, UiEvent};

fn app_at(root: &Path) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.join("data"));
    app.view.workspace = root
        .join("project")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.sidebar_collapsed = true;
    app.viewport_width = 900.0;
    app.init_code_editor_recovery();
    pump(&mut app, |app| !app.code_editor.recovery_loading);
    assert!(
        app.code_editor.recovery_available,
        "{:?}",
        app.code_editor.recovery_error
    );
    app
}
fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::fs::write(root.path().join("project/main.rs"), "base\r\n").unwrap();
    let app = app_at(root.path());
    (root, app)
}
fn pump(app: &mut AgentsWindow, done: impl Fn(&AgentsWindow) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while {
        app.poll_code_editor(&egui::Context::default());
        !done(app)
    } {
        assert!(
            Instant::now() < deadline,
            "worker timed out: {:?}",
            app.view.code_editor
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn open(app: &mut AgentsWindow) {
    let workspace = PathBuf::from(&app.view.workspace);
    app.dock
        .open_file(&workspace, "", Path::new("main.rs"))
        .unwrap();
    app.sync_dock_view();
    pump(app, |app| {
        app.view
            .code_editor
            .as_ref()
            .is_some_and(|view| !view.loading)
    });
}
fn buffer(app: &AgentsWindow) -> EditorController {
    app.view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .clone()
        .unwrap()
}
fn text(app: &AgentsWindow) -> String {
    let snapshot = buffer(app).snapshot().unwrap();
    snapshot.text_range(0..snapshot.char_count()).unwrap()
}
fn edit(app: &mut AgentsWindow, value: &str) {
    // Exercise the same host action admitted by real CodeEditor input.
    let controller = buffer(app);
    let shared = controller.shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let chars = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(chars)).unwrap();
    buffer.insert(value, EditKind::Paste, 1).unwrap();
    let snapshot = buffer.snapshot();
    drop(buffer);
    app.apply_code_editor_action(
        &UiAction::EditorChanged {
            id: code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id),
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
        },
        &egui::Context::default(),
    );
}
fn flush_draft(app: &mut AgentsWindow) {
    app.code_editor.tick_lifecycle(Instant::now());
    app.code_editor
        .tick_lifecycle(Instant::now() + Duration::from_secs(1));
    pump(app, |app| {
        app.code_editor.documents.values().all(|d| {
            let snapshot = d.controller.as_ref().unwrap().snapshot().unwrap();
            d.draft.queued.is_none()
                && d.draft.saved == snapshot.is_dirty().then(|| draft_identity(&snapshot))
        })
    });
}
fn inspect(app: &mut AgentsWindow) {
    for d in app.code_editor.documents.values_mut() {
        d.inspected_at = None;
    }
    app.code_editor.tick_lifecycle(Instant::now());
    pump(app, |app| {
        app.code_editor.documents.values().all(|d| !d.inspecting)
    });
}
fn close_instance(app: AgentsWindow, root: &Path) {
    drop(app);
    // A real restart releases process locks before the next process starts.
    // Wait for the old detached worker's drain to model that boundary exactly.
    let lock = std::fs::File::open(root.join("data/editor-drafts/.lock")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while lock.try_lock().is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn durable_recovery_reopens_unicode_draft_and_selection_without_touching_disk() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "café 🌎\r\n");
    let selection = Selection { anchor: 1, head: 4 };
    buffer(&app)
        .shared_buffer()
        .lock()
        .unwrap()
        .set_selection(selection)
        .unwrap();
    let expected = text(&app);
    flush_draft(&mut app);
    close_instance(app, root.path());
    let mut restarted = app_at(root.path());
    pump(&mut restarted, |app| {
        app.view.code_editor.as_ref().is_some_and(|v| v.recovered)
    });
    assert_eq!(text(&restarted), expected);
    assert_eq!(
        buffer(&restarted).snapshot().unwrap().selection(),
        selection
    );
    assert!(buffer(&restarted).snapshot().unwrap().is_dirty());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
    inspect(&mut restarted);
    let id = code_editor_ui::view_id(restarted.view.code_editor.as_ref().unwrap().tab_id);
    let snapshot = buffer(&restarted).snapshot().unwrap();
    restarted.apply_code_editor_action(
        &UiAction::EditorSaveRequested {
            id,
            document_id: snapshot.document_id(),
            revision: snapshot.revision(),
        },
        &egui::Context::default(),
    );
    pump(&mut restarted, |app| !app.view.code_editor_pending_saves);
    assert!(!buffer(&restarted).snapshot().unwrap().is_dirty());
    flush_draft(&mut restarted);
    close_instance(restarted, root.path());
    let third = app_at(root.path());
    assert!(
        third.code_editor_documents().is_empty(),
        "saved draft must not resurrect"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        expected
    );
}

#[test]
fn external_poll_updates_clean_but_keeps_dirty_buffer_and_supplies_comparison() {
    let (root, mut app) = fixture();
    open(&mut app);
    std::fs::write(root.path().join("project/main.rs"), "external clean\n").unwrap();
    inspect(&mut app);
    assert_eq!(text(&app), "external clean\n");
    edit(&mut app, "my edit");
    std::fs::write(root.path().join("project/main.rs"), "external conflict\n").unwrap();
    inspect(&mut app);
    assert_eq!(text(&app), "external clean\nmy edit");
    let conflict = app.code_editor_conflict().unwrap();
    let disk = conflict.disk.unwrap().snapshot().unwrap();
    assert_eq!(
        disk.text_range(0..disk.char_count()).unwrap(),
        "external conflict\n"
    );
    app.code_editor
        .save(app.store.as_ref().map(|s| s.storage_root().to_path_buf()));
    pump(&mut app, |app| {
        app.code_editor.active_document().unwrap().saving.is_none()
    });
    assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "external conflict\n"
    );
}

#[test]
fn save_as_preserves_newer_edits_and_collision_never_replaces_destination() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "saved version");
    app.code_editor_save_as("copy.rs".into()).unwrap();
    edit(&mut app, " later");
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert_eq!(app.view.code_editor.as_ref().unwrap().path, "copy.rs");
    assert_eq!(text(&app), "base\r\nsaved version later");
    assert!(buffer(&app).snapshot().unwrap().is_dirty());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/copy.rs")).unwrap(),
        "base\r\nsaved version"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
    app.code_editor_save_as("main.rs".into()).unwrap();
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    assert_eq!(text(&app), "base\r\nsaved version later");
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
}

#[test]
fn deleted_clean_document_becomes_recoverable_and_explicit_save_as_recreates_it() {
    let (root, mut app) = fixture();
    open(&mut app);
    std::fs::remove_file(root.path().join("project/main.rs")).unwrap();
    inspect(&mut app);
    assert!(app.view.code_editor.as_ref().unwrap().missing);
    assert!(buffer(&app).snapshot().unwrap().is_dirty());
    flush_draft(&mut app);
    app.code_editor_save_as("main.rs".into()).unwrap();
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(!app.view.code_editor.as_ref().unwrap().missing);
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
    flush_draft(&mut app);
    close_instance(app, root.path());
    assert!(app_at(root.path()).code_editor_documents().is_empty());
}

#[test]
fn create_empty_file_is_explicit_and_rejects_escape_and_existing_target() {
    let (root, mut app) = fixture();
    assert!(
        app.code_editor_create("../outside.rs".into(), "unsafe".into())
            .is_err()
    );
    app.code_editor_create("new.rs".into(), String::new())
        .unwrap();
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert_eq!(
        std::fs::read(root.path().join("project/new.rs")).unwrap(),
        b""
    );
    assert!(!buffer(&app).snapshot().unwrap().is_dirty());
    app.code_editor_create("main.rs".into(), "must not overwrite".into())
        .unwrap();
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
}

#[test]
fn explicit_reload_discards_recovery_but_later_edits_get_a_new_durable_copy() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "discarded");
    flush_draft(&mut app);
    app.code_editor_discard_active().unwrap();
    pump(&mut app, |app| {
        app.view.code_editor.as_ref().is_some_and(|v| !v.loading)
    });
    edit(&mut app, "keep after reload");
    flush_draft(&mut app);
    close_instance(app, root.path());
    let restarted = app_at(root.path());
    assert_eq!(text(&restarted), "base\r\nkeep after reload");
}

#[test]
fn real_editor_keyboard_save_updates_recovery_without_accepting_stale_actions() {
    let (_root, mut app) = fixture();
    open(&mut app);
    let view = app.view.code_editor.clone().unwrap();
    app.tree.set_root(code_editor_ui::build(
        &view,
        &Default::default(),
        360.0,
        app.view.keymap_profile,
        app.view.vim_mode,
    ));
    app.tree.layout(Size::new(360.0, 480.0));
    app.tree.focus(&code_editor_ui::view_id(view.tab_id));
    for event in [
        UiEvent::Text("native ".into()),
        UiEvent::Key {
            key: Key::S,
            ctrl: true,
            shift: false,
        },
    ] {
        for action in app.tree.handle_event(event) {
            app.apply_code_editor_action(&action, &egui::Context::default());
        }
    }
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(!buffer(&app).snapshot().unwrap().is_dirty());
    assert!(text(&app).contains("native "));
}

fn frame(app: &mut AgentsWindow, width: f32) -> openide_ui::Frame {
    app.viewport_width = width;
    app.tree.set_root(build_view(&app.view, width));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 760.0))
}
fn click(app: &mut AgentsWindow, width: f32, id: &str) {
    let mut painted = frame(app, width);
    if app.view.editor_workbench.menu_open && id != "editor_menu" {
        // The menu keeps a bounded viewport. Navigate its real scroll area
        // before clicking commands that live below the initially visible rows.
        let visible = |painted: &openide_ui::Frame| {
            painted
                .regions
                .iter()
                .any(|region| region.id == id && region.kind == openide_ui::HitKind::Button)
        };
        if !visible(&painted) {
            for step in 0..33 {
                let viewport = painted
                    .regions
                    .iter()
                    .find(|region| {
                        region.id == "editor_menu_close:options"
                            && region.kind == openide_ui::HitKind::Scroll
                    })
                    .expect("open menu has a scroll viewport")
                    .rect;
                app.tree.handle_event(UiEvent::ScrollDirect {
                    position: openide_ui::Point::new(viewport.x + 12.0, viewport.y + 12.0),
                    delta_y: if step == 0 { 100_000.0 } else { -100.0 },
                });
                painted = frame(app, width);
                if visible(&painted) {
                    break;
                }
            }
        }
    }
    let region = painted
        .regions
        .iter()
        .find(|r| r.id == id && r.kind == openide_ui::HitKind::Button)
        .unwrap_or_else(|| {
            panic!(
                "missing {id}; visible {:?}",
                painted.regions.iter().map(|r| &r.id).collect::<Vec<_>>()
            )
        });
    let point = openide_ui::Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    assert_eq!(painted.hit_test(point), Some(id));
    app.tree.handle_event(UiEvent::PointerDown(point));
    for action in app.tree.handle_event(UiEvent::PointerUp(point)) {
        app.apply_action(action, &egui::Context::default());
    }
}
fn submit_path(app: &mut AgentsWindow, width: f32, value: &str) {
    frame(app, width);
    assert!(app.tree.focus("editor_form_value"));
    for event in [
        UiEvent::Text(value.into()),
        UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false,
        },
    ] {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, &egui::Context::default());
        }
    }
}

#[test]
fn actual_workbench_forms_create_save_as_and_collision_use_scoped_file_worker() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "from form");
    click(&mut app, 900.0, "editor_menu");
    click(&mut app, 900.0, "editor_save_as");
    submit_path(&mut app, 900.0, "form-copy.rs");
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert_eq!(app.view.code_editor.as_ref().unwrap().path, "form-copy.rs");
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/form-copy.rs")).unwrap(),
        "base\r\nfrom form"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
    click(&mut app, 900.0, "editor_menu");
    click(&mut app, 900.0, "editor_new");
    submit_path(&mut app, 900.0, "from-ui.rs");
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(
        std::fs::read(root.path().join("project/from-ui.rs"))
            .unwrap()
            .is_empty()
    );
    click(&mut app, 900.0, "editor_menu");
    click(&mut app, 900.0, "editor_save_as");
    submit_path(&mut app, 900.0, "main.rs");
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
}

#[test]
fn actual_conflict_button_shows_a_read_only_disk_version_and_keeps_draft() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "my draft");
    std::fs::write(root.path().join("project/main.rs"), "external version").unwrap();
    inspect(&mut app);
    click(&mut app, 900.0, "editor_conflict");
    assert!(app.view.editor_workbench.conflict_open);
    frame(&mut app, 900.0);
    assert!(app.tree.focus("editor_conflict_disk"));
    for action in app.tree.handle_event(UiEvent::Text("must not edit".into())) {
        app.apply_action(action, &egui::Context::default());
    }
    let snapshot = app
        .view
        .editor_workbench
        .conflict_disk
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(
        snapshot.text_range(0..snapshot.char_count()).unwrap(),
        "external version"
    );
    assert_eq!(text(&app), "base\r\nmy draft");
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "external version"
    );
}

#[test]
fn background_language_loads_do_not_cancel_each_other_or_change_active_dirty_editor() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "active dirty");
    for path in ["one.rs", "two.rs"] {
        std::fs::write(root.path().join("project").join(path), path).unwrap();
    }
    let active = buffer(&app).snapshot().unwrap().document_id();
    assert!(
        app.ensure_code_editor_loaded(Path::new("one.rs"))
            .unwrap()
            .is_none()
    );
    assert!(
        app.ensure_code_editor_loaded(Path::new("two.rs"))
            .unwrap()
            .is_none()
    );
    pump(&mut app, |app| {
        app.code_editor_controller(Path::new("one.rs")).is_some()
            && app.code_editor_controller(Path::new("two.rs")).is_some()
    });
    assert_eq!(buffer(&app).snapshot().unwrap().document_id(), active);
    assert_eq!(text(&app), "base\r\nactive dirty");
    assert!(buffer(&app).snapshot().unwrap().is_dirty());
    assert_eq!(app.view.code_editor.as_ref().unwrap().path, "main.rs");
}

#[test]
fn keep_and_close_flushes_the_latest_snapshot_before_restarting_and_cancel_revokes_close() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "not debounced yet");
    app.view.code_editor_quit_confirm = true;
    app.apply_code_editor_action(
        &UiAction::Clicked {
            id: "code_editor_quit_keep".into(),
        },
        &egui::Context::default(),
    );
    app.apply_code_editor_action(
        &UiAction::Clicked {
            id: "code_editor_quit_cancel".into(),
        },
        &egui::Context::default(),
    );
    assert!(!app.code_editor.keep_and_close && !app.code_editor.allow_close);
    app.view.code_editor_quit_confirm = true;
    app.apply_code_editor_action(
        &UiAction::Clicked {
            id: "code_editor_quit_keep".into(),
        },
        &egui::Context::default(),
    );
    assert!(
        !app.code_editor.allow_close,
        "queue admission is not durable completion"
    );
    pump(&mut app, |app| app.code_editor.allow_close);
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "base\r\n"
    );
    close_instance(app, root.path());
    let restarted = app_at(root.path());
    assert_eq!(text(&restarted), "base\r\nnot debounced yet");
}

#[test]
fn explicit_discard_and_close_removes_durable_drafts_before_closing() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "discard this");
    flush_draft(&mut app);
    app.view.code_editor_quit_confirm = true;
    app.apply_code_editor_action(
        &UiAction::Clicked {
            id: "code_editor_quit_discard".into(),
        },
        &egui::Context::default(),
    );
    assert!(!app.code_editor.allow_close);
    pump(&mut app, |app| app.code_editor.allow_close);
    close_instance(app, root.path());
    assert!(app_at(root.path()).code_editor_documents().is_empty());
}

#[test]
fn save_as_completion_keeps_the_original_task_scope_without_stealing_new_task_focus() {
    let (root, mut app) = fixture();
    open(&mut app);
    edit(&mut app, "saved in original task");
    app.code_editor_save_as("background-copy.rs".into())
        .unwrap();
    app.view.selected_session = Some("another-task".into());
    let scope = app.dock_scope();
    app.dock.open(&scope, DockTabKind::Activity);
    app.sync_dock_view();
    pump(&mut app, |app| !app.view.code_editor_pending_saves);
    assert_eq!(app.view.side_tab, SideTab::Activity);
    assert!(
        !app.view
            .dock_tabs
            .iter()
            .any(|tab| tab.title.contains("background-copy"))
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/background-copy.rs")).unwrap(),
        "base\r\nsaved in original task"
    );
    app.view.selected_session = None;
    app.sync_dock_view();
    assert!(
        app.view
            .dock_tabs
            .iter()
            .any(|tab| tab.title.contains("background-copy"))
    );
}
