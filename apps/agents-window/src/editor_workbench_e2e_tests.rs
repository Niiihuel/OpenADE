//! Offline end-to-end flows through real editor controls, host dispatch and buffer.
use super::*;
use openide_editor::{EditorCommand, Position, Selection};
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point};

fn fixture(source: &str) -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("main.rs"), source).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.viewport_width = 1600.0;
    app.view.workspace = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.sidebar_collapsed = true;
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.project_branch = None;
    open(&mut app, "main.rs");
    (root, app)
}
fn open(app: &mut AgentsWindow, path: &str) {
    let scope = app.dock_scope();
    let id = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    app.dock.activate(&scope, id);
    app.dock.set_collapsed(&scope, false);
    app.dock.set_width_px(&scope.workspace_id, 780.0);
    app.sync_dock_view();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&egui::Context::default());
        if app.view.code_editor.as_ref().is_some_and(|v| !v.loading) {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    app.poll_editor_workbench(&egui::Context::default());
    frame(app);
}
fn frame(app: &mut AgentsWindow) -> Frame {
    app.tree.set_root(build_view(&app.view, 1600.0));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(1600.0, 1100.0))
}
fn snapshot(app: &AgentsWindow) -> openide_editor::BufferSnapshot {
    app.active_editor().unwrap().3
}
fn text(app: &AgentsWindow) -> String {
    let s = snapshot(app);
    s.text_range(0..s.char_count()).unwrap()
}
fn dispatch(app: &mut AgentsWindow, event: UiEvent) {
    let ctx = egui::Context::default();
    if !app.handle_editor_workbench_key(&event, &ctx) {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, &ctx);
        }
    }
}
fn key(app: &mut AgentsWindow, key: Key, ctrl: bool, shift: bool) {
    dispatch(app, UiEvent::Key { key, ctrl, shift });
}
fn focus_editor(app: &mut AgentsWindow) {
    frame(app);
    let id = crate::code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id);
    assert!(app.tree.focus(&id));
}
fn click(app: &mut AgentsWindow, id: &str) {
    if matches!(id, "editor_wrap" | "editor_outline" | "editor_settings")
        && !app.view.editor_workbench.menu_open
    {
        click(app, "editor_menu");
    }
    let mut painted = frame(app);
    // Commands remain in a bounded menu: move its real viewport until the
    // requested row is visible, just as a pointer user does.
    if app.view.editor_workbench.menu_open && id != "editor_menu" {
        if !painted
            .regions
            .iter()
            .any(|r| r.id == id && r.kind == HitKind::Button)
        {
            let viewport = painted
                .regions
                .iter()
                .find(|r| r.id == "editor_menu_close:options" && r.kind == HitKind::Scroll)
                .expect("open editor menu has its own scroll viewport")
                .rect;
            dispatch(
                app,
                UiEvent::ScrollDirect {
                    position: Point::new(viewport.x + 12.0, viewport.y + 12.0),
                    delta_y: 100_000.0,
                },
            );
            painted = frame(app);
        }
        for _ in 0..32 {
            if painted
                .regions
                .iter()
                .any(|r| r.id == id && r.kind == HitKind::Button)
            {
                break;
            }
            let viewport = painted
                .regions
                .iter()
                .find(|r| r.id == "editor_menu_close:options" && r.kind == HitKind::Scroll)
                .expect("open editor menu has its own scroll viewport")
                .rect;
            dispatch(
                app,
                UiEvent::ScrollDirect {
                    position: Point::new(viewport.x + 12.0, viewport.y + 12.0),
                    delta_y: -100.0,
                },
            );
            painted = frame(app);
        }
    }
    let r = painted
        .regions
        .iter()
        .find(|r| r.id == id && r.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing rendered action {id}"));
    let p = Point::new(
        r.rect.x + r.rect.width / 2.0,
        r.rect.y + r.rect.height / 2.0,
    );
    assert_eq!(painted.hit_test(p), Some(id));
    dispatch(app, UiEvent::PointerDown(p));
    dispatch(app, UiEvent::PointerUp(p));
}
fn field(app: &mut AgentsWindow, id: &str, value: &str) {
    let painted = frame(app);
    assert!(painted
        .regions
        .iter()
        .any(|r| r.id == id && matches!(r.kind, HitKind::TextInput | HitKind::TextArea)));
    assert!(app.tree.focus(id));
    key(app, Key::A, true, false);
    dispatch(app, UiEvent::Text(value.into()));
}
fn search_done(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_editor_workbench(&egui::Context::default());
        if !app.view.editor_workbench.search_pending && app.view.editor_workbench.search.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "search failed: {:?}",
            app.view.editor_workbench.search_error
        );
        std::thread::sleep(Duration::from_millis(3));
    }
    frame(app);
}
fn count(app: &AgentsWindow) -> usize {
    app.view
        .editor_workbench
        .search
        .as_ref()
        .unwrap()
        .matches
        .len()
}
fn menu_action(app: &mut AgentsWindow, label: &str) {
    focus_editor(app);
    dispatch(app, UiEvent::ContextMenuKeyboard);
    let painted = frame(app);
    assert!(app.tree.context_menu_open());
    let point = painted
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Text {
                text,
                position,
                font_size,
                ..
            } if text == label => Some(Point::new(position.x + 3.0, position.y + font_size / 2.0)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing menu label {label}"));
    let hit = painted.hit_test(point).expect("menu text hit").to_owned();
    assert!(hit.starts_with("__context_menu:item:"));
    dispatch(app, UiEvent::PointerDown(point));
    dispatch(app, UiEvent::PointerUp(point));
    assert!(!app.tree.context_menu_open());
}

#[test]
fn editor_search_replace_options_next_and_goto_use_rendered_controls() {
    let (_root, mut app) = fixture("foo FOO food\r\nfoo\r\n");
    focus_editor(&mut app);
    key(&mut app, Key::H, true, false);
    assert!(app.view.editor_workbench.replace_open);
    field(&mut app, "editor_query", "foo");
    search_done(&mut app);
    assert_eq!(count(&app), 4);
    click(&mut app, "editor_word");
    search_done(&mut app);
    assert_eq!(count(&app), 3);
    click(&mut app, "editor_case");
    search_done(&mut app);
    assert_eq!(count(&app), 2);
    click(&mut app, "editor_search_next");
    assert_eq!(snapshot(&app).selection().range(), 0..3);
    click(&mut app, "editor_search_next");
    assert_eq!(snapshot(&app).selection().range(), 14..17);
    field(&mut app, "editor_replacement", "東京");
    click(&mut app, "editor_replace_one");
    assert_eq!(text(&app), "foo FOO food\r\n東京\r\n");
    focus_editor(&mut app);
    key(&mut app, Key::Z, true, false);
    assert_eq!(text(&app), "foo FOO food\r\nfoo\r\n");
    search_done(&mut app);
    click(&mut app, "editor_replace_all");
    assert_eq!(text(&app), "東京 FOO food\r\n東京\r\n");
    focus_editor(&mut app);
    key(&mut app, Key::Z, true, false);
    assert_eq!(text(&app), "foo FOO food\r\nfoo\r\n");
    click(&mut app, "editor_regex");
    field(&mut app, "editor_query", "f(o+)");
    search_done(&mut app);
    assert_eq!(count(&app), 2);
    app.editor_command(
        EditorCommand::SetSelection(Selection { anchor: 0, head: 3 }),
        &egui::Context::default(),
    )
    .unwrap();
    click(&mut app, "editor_scope");
    search_done(&mut app);
    assert_eq!(count(&app), 1);
    focus_editor(&mut app);
    key(&mut app, Key::G, true, false);
    assert!(app.view.editor_workbench.search_open);
    assert!(
        !frame(&mut app)
            .regions
            .iter()
            .any(|region| region.id == "editor_query"),
        "the search surface must yield to the goto form"
    );
    field(&mut app, "editor_form_value", "2:2");
    click(&mut app, "editor_form_apply");
    assert!(
        frame(&mut app)
            .regions
            .iter()
            .any(|region| region.id == "editor_query"),
        "closing the goto form must restore the search surface"
    );
    assert_eq!(
        snapshot(&app)
            .position_of(snapshot(&app).selection().head)
            .unwrap(),
        Position { row: 1, column: 1 }
    );
    assert!(app.view.editor_workbench.error.is_none());
}

#[test]
fn editor_expanding_replace_with_empty_query_keeps_search_ready() {
    let (_root, mut app) = fixture("foo foo\n");
    click(&mut app, "editor_find");
    click(&mut app, "editor_replace_toggle");
    frame(&mut app);
    app.poll_editor_workbench(&egui::Context::default());
    assert!(app.view.editor_workbench.replace_open);
    assert_eq!(app.tree.focused_id(), Some("editor_query"));
    dispatch(&mut app, UiEvent::Text("foo".into()));
    search_done(&mut app);
    assert_eq!(app.view.editor_workbench.query, "foo");
    assert!(app.view.editor_workbench.replacement.is_empty());
    assert_eq!(count(&app), 2);
}

#[test]
fn editor_replace_enter_edits_selected_match_and_keeps_replacement_focus() {
    let source = "foo foo\n";
    let (_root, mut app) = fixture(source);
    focus_editor(&mut app);
    key(&mut app, Key::H, true, false);
    field(&mut app, "editor_query", "foo");
    search_done(&mut app);
    click(&mut app, "editor_search_next");
    field(&mut app, "editor_replacement", "東京");
    key(&mut app, Key::Enter, false, false);
    assert_eq!(text(&app), "東京 foo\n");
    assert_eq!(app.tree.focused_id(), Some("editor_replacement"));
    assert!(app.view.editor_workbench.search_error.is_none());
    assert!(app.view.editor_workbench.error.is_none());

    // The first replacement invalidates the old match snapshot. Repeating
    // Enter must neither edit another match nor report an invalid-match error.
    key(&mut app, Key::Enter, false, false);
    search_done(&mut app);
    key(&mut app, Key::Enter, false, false);
    assert_eq!(text(&app), "東京 foo\n");
    assert_eq!(app.tree.focused_id(), Some("editor_replacement"));
    assert!(app.view.editor_workbench.search_error.is_none());

    focus_editor(&mut app);
    key(&mut app, Key::Z, true, false);
    assert_eq!(text(&app), source);
}

#[test]
fn editor_replace_enter_ignores_pending_or_unselected_matches() {
    let source = "foo foo\n";
    let (_root, mut app) = fixture(source);
    focus_editor(&mut app);
    key(&mut app, Key::H, true, false);
    field(&mut app, "editor_query", "foo");
    search_done(&mut app);
    field(&mut app, "editor_replacement", "bar");
    key(&mut app, Key::Enter, false, false);
    assert_eq!(text(&app), source);
    assert!(app.view.editor_workbench.search_error.is_none());

    click(&mut app, "editor_search_next");
    field(&mut app, "editor_replacement", "bar");
    app.view.editor_workbench.search_pending = true;
    key(&mut app, Key::Enter, false, false);
    assert_eq!(text(&app), source);
    assert_eq!(app.tree.focused_id(), Some("editor_replacement"));
    assert!(app.view.editor_workbench.search_error.is_none());
    app.view.editor_workbench.search_pending = false;

    field(&mut app, "editor_query", "missing");
    search_done(&mut app);
    assert_eq!(count(&app), 0);
    field(&mut app, "editor_replacement", "bar");
    key(&mut app, Key::Enter, false, false);
    assert_eq!(text(&app), source);
    assert_eq!(app.tree.focused_id(), Some("editor_replacement"));
    assert!(app.view.editor_workbench.search_error.is_none());
    assert!(app.view.editor_workbench.error.is_none());
}

#[test]
fn editor_search_toggle_and_pointer_navigation_keep_keyboard_search_ready() {
    let source = "foo foo foo\n";
    let (_root, mut app) = fixture(source);
    click(&mut app, "editor_find");
    field(&mut app, "editor_query", "foo");
    search_done(&mut app);
    assert!(!app.view.editor_workbench.replace_open);

    click(&mut app, "editor_replace_toggle");
    frame(&mut app);
    app.poll_editor_workbench(&egui::Context::default());
    assert_eq!(app.tree.focused_id(), Some("editor_replacement"));
    field(&mut app, "editor_replacement", "bar");
    click(&mut app, "editor_replace_toggle");
    let painted = frame(&mut app);
    app.poll_editor_workbench(&egui::Context::default());
    assert_eq!(app.tree.focused_id(), Some("editor_query"));
    assert!(!app.view.editor_workbench.replace_open);
    assert!(!painted.regions.iter().any(|r| r.id == "editor_replacement"));
    assert_eq!(app.view.editor_workbench.query, "foo");
    assert_eq!(app.view.editor_workbench.replacement, "bar");

    click(&mut app, "editor_search_next");
    assert_eq!(snapshot(&app).selection().range(), 0..3);
    assert_eq!(app.tree.focused_id(), Some("editor_query"));
    key(&mut app, Key::Enter, false, false);
    assert_eq!(snapshot(&app).selection().range(), 4..7);
    key(&mut app, Key::Enter, false, true);
    assert_eq!(snapshot(&app).selection().range(), 0..3);
    click(&mut app, "editor_search_prev");
    assert_eq!(snapshot(&app).selection().range(), 8..11);
    assert_eq!(app.tree.focused_id(), Some("editor_query"));
    key(&mut app, Key::Enter, false, false);
    assert_eq!(snapshot(&app).selection().range(), 0..3);
    assert_eq!(text(&app), source);

    click(&mut app, "editor_replace_toggle");
    assert!(app.view.editor_workbench.replace_open);
    assert_eq!(app.view.editor_workbench.query, "foo");
    assert_eq!(app.view.editor_workbench.replacement, "bar");
}

#[test]
fn editor_search_escape_from_each_control_restores_editor_focus() {
    let (_root, mut app) = fixture("foo foo\n");
    let editor_id = crate::code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id);
    for id in [
        "editor_query",
        "editor_replacement",
        "editor_search_next",
        "editor_search_prev",
        "editor_search_close",
        "editor_case",
        "editor_word",
        "editor_regex",
        "editor_scope",
        "editor_replace_toggle",
        "editor_replace_one",
        "editor_replace_all",
    ] {
        focus_editor(&mut app);
        key(&mut app, Key::H, true, false);
        field(&mut app, "editor_query", "foo");
        search_done(&mut app);
        click(&mut app, "editor_search_next");
        frame(&mut app);
        assert!(app.tree.focus(id), "missing search control {id}");
        key(&mut app, Key::Escape, false, false);
        assert!(!app.view.editor_workbench.search_open, "Escape from {id}");
        frame(&mut app);
        app.poll_editor_workbench(&egui::Context::default());
        assert_eq!(app.tree.focused_id(), Some(editor_id.as_str()), "{id}");
        assert_eq!(app.view.editor_workbench.query, "foo");
    }
    assert_eq!(text(&app), "foo foo\n");
}

#[test]
fn editor_invalid_regex_and_pending_navigation_stay_inside_search_and_recover() {
    let (_root, mut app) = fixture("foo foo\n");
    let editor_id = crate::code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id);
    let editor_rect = frame(&mut app)
        .regions
        .iter()
        .find(|region| region.id == editor_id && region.kind == HitKind::CodeEditor)
        .unwrap()
        .rect;
    click(&mut app, "editor_find");
    click(&mut app, "editor_regex");
    field(&mut app, "editor_query", "[");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_editor_workbench(&egui::Context::default());
        if !app.view.editor_workbench.search_pending
            && app.view.editor_workbench.search_error.is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "invalid regex never reported an error"
        );
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(app.view.editor_workbench.search.is_none());
    assert!(app.view.editor_workbench.error.is_none());
    assert_eq!(
        frame(&mut app)
            .regions
            .iter()
            .find(|region| region.id == editor_id && region.kind == HitKind::CodeEditor)
            .unwrap()
            .rect,
        editor_rect,
        "a regex error must not push the document down"
    );

    field(&mut app, "editor_query", "foo");
    app.poll_editor_workbench(&egui::Context::default());
    assert!(app.view.editor_workbench.search_pending);
    assert!(app.view.editor_workbench.search_error.is_none());
    for shift in [false, true] {
        key(&mut app, Key::Enter, false, shift);
        assert!(app.view.editor_workbench.search_error.is_some());
        assert!(app.view.editor_workbench.error.is_none());
    }
    assert_eq!(
        frame(&mut app)
            .regions
            .iter()
            .find(|region| region.id == editor_id && region.kind == HitKind::CodeEditor)
            .unwrap()
            .rect,
        editor_rect,
        "pending navigation must report inside the search surface"
    );
    search_done(&mut app);
    assert_eq!(count(&app), 2);
    assert!(app.view.editor_workbench.search_error.is_none());
    assert!(app.view.editor_workbench.error.is_none());
    key(&mut app, Key::Enter, false, false);
    assert_eq!(snapshot(&app).selection().range(), 0..3);
    assert_eq!(text(&app), "foo foo\n");
}

#[test]
fn editor_multicursor_comment_snippet_and_undo_pass_through_host_and_context_menu() {
    let (_root, mut app) = fixture("foo\r\nfoo\r\n");
    app.editor_command(
        EditorCommand::SetSelection(Selection { anchor: 0, head: 3 }),
        &egui::Context::default(),
    )
    .unwrap();
    focus_editor(&mut app);
    key(&mut app, Key::D, true, false);
    assert_eq!(snapshot(&app).selections().count(), 2);
    dispatch(&mut app, UiEvent::Text("café".into()));
    assert_eq!(text(&app), "café\r\ncafé\r\n");
    key(&mut app, Key::Z, true, false);
    assert_eq!(text(&app), "foo\r\nfoo\r\n");
    menu_action(&mut app, "Comentar / descomentar");
    assert_eq!(text(&app), "// foo\r\n// foo\r\n");
    menu_action(&mut app, "Deshacer");
    assert_eq!(text(&app), "foo\r\nfoo\r\n");
    app.editor_command(
        EditorCommand::SetSelection(Selection {
            anchor: 0,
            head: snapshot(&app).char_count(),
        }),
        &egui::Context::default(),
    )
    .unwrap();
    app.editor_command(
        EditorCommand::InsertSnippet("let ${1:name} = $1; ${2:value}$0".into()),
        &egui::Context::default(),
    )
    .unwrap();
    focus_editor(&mut app);
    dispatch(&mut app, UiEvent::Text("東京".into()));
    key(&mut app, Key::Tab, false, false);
    dispatch(&mut app, UiEvent::Text("42".into()));
    key(&mut app, Key::Tab, false, false);
    assert_eq!(text(&app), "let 東京 = 東京; 42");
    key(&mut app, Key::Z, true, false);
    assert_eq!(text(&app), "let 東京 = 東京; value");
}

#[test]
fn editor_wrap_folds_outline_and_duplicate_basename_tabs_are_functional() {
    let source =
        "fn main() {\n    let hidden_marker = 1;\n    hidden_marker();\n}\nfn second() {}\n";
    let (_root, mut app) = fixture(source);
    click(&mut app, "editor_wrap");
    assert!(app.view.editor_workbench.soft_wrap);
    let analysis = openide_language::ParserEngine::default()
        .analyze(
            &snapshot(&app),
            openide_language::Language::Rust,
            &openide_language::Cancellation::default(),
        )
        .unwrap();
    assert!(!analysis.folds.is_empty());
    app.view.editor_workbench.analysis = Some(std::sync::Arc::new(analysis));
    click(&mut app, "editor_menu");
    click(&mut app, "editor_fold");
    assert!(!app.view.editor_workbench.folds.is_empty());
    let painted = frame(&mut app);
    assert!(!painted
        .commands
        .iter()
        .any(|c| matches!(c,DrawCommand::Text{text,..}if text.contains("hidden_marker"))));
    click(&mut app, "editor_menu");
    click(&mut app, "editor_unfold");
    assert!(app.view.editor_workbench.folds.is_empty());
    click(&mut app, "editor_outline");
    let painted = frame(&mut app);
    assert!(painted
        .regions
        .iter()
        .any(|r| r.id.starts_with("editor_symbol:")));
    click(&mut app, "editor_symbol:1");
    assert!(snapshot(&app).selection().head > 0);
    std::fs::create_dir(PathBuf::from(&app.view.workspace).join("nested")).unwrap();
    std::fs::write(
        PathBuf::from(&app.view.workspace).join("nested/main.rs"),
        "fn other() {}\n",
    )
    .unwrap();
    open(&mut app, "nested/main.rs");
    let titles: Vec<_> = app
        .view
        .dock_tabs
        .iter()
        .filter(|t| t.kind == SideTab::FilePreview)
        .map(|t| t.title.clone())
        .collect();
    assert_eq!(titles.len(), 2);
    assert_ne!(
        titles[0], titles[1],
        "duplicate basenames need distinguishable labels"
    );
}

#[test]
fn conflict_read_only_shortcuts_never_edit_the_active_document() {
    let (root, mut app) = fixture("fn main() {}\n");
    focus_editor(&mut app);
    dispatch(&mut app, UiEvent::Text("// my draft\n".into()));
    let original = snapshot(&app);
    let original_text = text(&app);
    let disk_text = "// externally changed\nfn main() {}\n";
    std::fs::write(root.path().join("project/main.rs"), disk_text).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.view.code_editor.as_ref().unwrap().conflict {
        app.poll_code_editor(&egui::Context::default());
        assert!(
            Instant::now() < deadline,
            "external change was not detected"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    click(&mut app, "editor_conflict");
    frame(&mut app);
    assert!(app.tree.focus("editor_conflict_disk"));
    let disk = app.view.editor_workbench.conflict_disk.clone().unwrap();
    let disk_before = disk.snapshot().unwrap();
    for command in [Key::K, Key::D] {
        let event = UiEvent::Key {
            key: command,
            shift: true,
            ctrl: true,
        };
        assert!(
            !app.handle_editor_workbench_key(&event, &egui::Context::default()),
            "read-only focus must not route editing commands to the active buffer"
        );
        dispatch(&mut app, event);
        assert_eq!(app.tree.focused_id(), Some("editor_conflict_disk"));
        assert_eq!(text(&app), original_text);
        assert_eq!(snapshot(&app).revision(), original.revision());
        assert_eq!(snapshot(&app).selection(), original.selection());
        let after = disk.snapshot().unwrap();
        assert_eq!(after.text_range(0..after.char_count()).unwrap(), disk_text);
        assert_eq!(after.revision(), disk_before.revision());
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        disk_text
    );
    // Delete-lines remains available once the actual editable surface has focus.
    // Duplicating now dispatches through the native profile registry.
    focus_editor(&mut app);
    key(&mut app, Key::K, true, true);
    assert_ne!(text(&app), original_text);
}

#[test]
fn language_service_popover_preserves_editor_geometry_and_routes_typing_to_its_field() {
    let (_root, mut app) = fixture("fn main() {}\n");
    let original = snapshot(&app);
    let original_text = text(&app);
    let editor_id = crate::code_editor_ui::view_id(app.view.code_editor.as_ref().unwrap().tab_id);
    let before = frame(&mut app);
    let editor_rect = before
        .regions
        .iter()
        .find(|r| r.id == editor_id)
        .unwrap()
        .rect;

    click(&mut app, "ide_editor_language_service");
    let scope = crate::ide_status_ui::document_scope(&app.view).unwrap();
    let input_id = format!("{}:server_path", scope.prefix());
    let opened = frame(&mut app);
    assert_eq!(
        app.view.ide_status.menu,
        Some(crate::ide_status_ui::StatusMenu::Services)
    );
    assert!(!app.view.editor_workbench.settings_open);
    assert_eq!(
        opened
            .regions
            .iter()
            .find(|r| r.id == editor_id)
            .unwrap()
            .rect,
        editor_rect
    );
    assert_eq!(app.tree.focused_id(), Some(input_id.as_str()));
    assert!(!opened.regions.iter().any(|r| r.id == "editor_server_path"));

    key(&mut app, Key::A, true, false);
    dispatch(
        &mut app,
        UiEvent::Text("/tmp/servidores/lengüaje de prueba".into()),
    );
    frame(&mut app);
    assert_eq!(
        app.tree.input_value(&input_id),
        Some("/tmp/servidores/lengüaje de prueba")
    );
    assert_eq!(
        app.view.editor_workbench.server_path,
        "/tmp/servidores/lengüaje de prueba"
    );
    key(&mut app, Key::ArrowLeft, false, false);
    frame(&mut app);
    dispatch(&mut app, UiEvent::Text("X".into()));
    frame(&mut app);
    assert_eq!(
        app.tree.input_value(&input_id),
        Some("/tmp/servidores/lengüaje de pruebXa")
    );
    assert_eq!(text(&app), original_text);
    assert_eq!(snapshot(&app).revision(), original.revision());
    assert_eq!(snapshot(&app).selection(), original.selection());

    key(&mut app, Key::Escape, false, false);
    frame(&mut app);
    assert!(app.view.ide_status.menu.is_none());
    assert_eq!(app.tree.focused_id(), Some("ide_editor_language_service"));
}

#[test]
fn editor_options_persist_from_rendered_controls_and_survive_reopening_a_document() {
    let (root, mut app) = fixture("fn main() {}\n");
    let original = text(&app);
    click(&mut app, "editor_wrap");
    // General editing preferences remain in the editor's More menu. LSP has
    // its own status popover and must not expand this settings strip.
    click(&mut app, "editor_settings");
    assert!(app.view.editor_workbench.settings_open);
    click(&mut app, "editor_indent_kind");
    click(&mut app, "editor_indent_width");
    click(&mut app, "editor_pairs");
    assert!(app.view.editor_workbench.soft_wrap);
    assert_eq!(
        app.view.editor_workbench.indent,
        openide_editor::IndentStyle::Tabs(8)
    );
    assert!(!app.view.editor_workbench.auto_pairs);
    assert_eq!(text(&app), original);
    assert!(
        !snapshot(&app).is_dirty(),
        "preferences must not edit the file"
    );
    let path = root.path().join("data/editor-preferences.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_editor_workbench(&egui::Context::default());
        let stored = crate::editor_preferences::Prefs::load(&path);
        if stored.soft_wrap && stored.use_tabs && stored.tab_width == 8 && !stored.auto_pairs {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "latest preferences were not persisted"
        );
        assert!(app.view.editor_workbench.error.is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    let workspace = app.view.workspace.clone();
    drop(app);
    // Model process exit: the recovery worker must release its lease before restart.
    let lease = std::fs::File::open(root.path().join("data/editor-drafts/.lock")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while lease.try_lock().is_err() {
        assert!(
            Instant::now() < deadline,
            "old instance retained its draft lease"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(lease);
    let mut restarted = AgentsWindow::with_data_root(root.path().join("data"));
    restarted.viewport_width = 1600.0;
    restarted.view.workspace = workspace.clone();
    restarted.view.workspace_draft = workspace;
    restarted.view.sidebar_collapsed = true;
    restarted.view.show_settings = false;
    restarted.view.main_page = MainPage::Workspace;
    assert!(restarted.view.editor_workbench.soft_wrap);
    assert_eq!(
        restarted.view.editor_workbench.indent,
        openide_editor::IndentStyle::Tabs(8)
    );
    assert!(!restarted.view.editor_workbench.auto_pairs);
    open(&mut restarted, "main.rs");
    assert!(restarted.view.editor_workbench.soft_wrap);
    assert_eq!(
        restarted.view.editor_workbench.indent,
        openide_editor::IndentStyle::Tabs(8)
    );
    assert!(!restarted.view.editor_workbench.auto_pairs);
    assert_eq!(text(&restarted), original);
    assert!(!snapshot(&restarted).is_dirty());
}

#[cfg(feature = "editor-e2e")]
#[test]
fn editor_e2e_bootstrap_focus_survives_loading_until_the_editor_is_visible() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".openide-editor-fixture"), "test fixture").unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::fs::write(root.path().join("project/main.rs"), "fn main() {}\n").unwrap();
    let mut app = AgentsWindow::editor_e2e(root.path());
    app.viewport_width = 1600.0;
    let view = app.view.code_editor.as_ref().unwrap();
    assert!(view.loading);
    let expected_focus = crate::code_editor_ui::view_id(view.tab_id);
    assert_eq!(
        app.editor_services.focus_next.as_deref(),
        Some(expected_focus.as_str())
    );
    frame(&mut app);
    app.poll_editor_workbench(&egui::Context::default());
    assert_eq!(
        app.editor_services.focus_next.as_deref(),
        Some(expected_focus.as_str()),
        "polling while the file worker is loading must retain the focus request"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&egui::Context::default());
        frame(&mut app);
        app.poll_editor_workbench(&egui::Context::default());
        if app.tree.focused_id() == Some(expected_focus.as_str()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "bootstrap never focused the loaded editor"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.editor_services.focus_next.is_none());
    dispatch(&mut app, UiEvent::Text("// focused\n".into()));
    assert_eq!(text(&app), "// focused\nfn main() {}\n");
    assert!(snapshot(&app).is_dirty());
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/main.rs")).unwrap(),
        "fn main() {}\n"
    );
}
