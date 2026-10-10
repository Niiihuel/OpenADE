//! Native worker and document-store integration for the independent preview tab.
use super::*;
use openide_editor::{EditKind, Selection};
use openide_ui::EditorController;
use openide_ui::{Frame, HitKind, Key, Point};

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("README.md"), "# Saved\n").unwrap();
    std::fs::write(workspace.join("other.md"), "# Other\n").unwrap();
    std::fs::write(workspace.join("code.rs"), "fn main() {}\n").unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.viewport_width = 1440.0;
    app.viewport_height = 900.0;
    app.view.workspace = workspace.canonicalize().unwrap().display().to_string();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.selected_session = None;
    app.dock = DockState::default();
    (root, app)
}

fn open_source(app: &mut AgentsWindow, path: &str) -> DockTabId {
    let scope = app.dock_scope();
    let tab = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .view
        .code_editor
        .as_ref()
        .is_some_and(|editor| editor.loading)
    {
        app.poll_code_editor(&egui::Context::default());
        assert!(Instant::now() < deadline, "source loading did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    tab
}

fn controller(app: &AgentsWindow) -> EditorController {
    app.view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .clone()
        .unwrap()
}

fn append(controller: &EditorController, text: &str) {
    let shared = controller.shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let length = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(length)).unwrap();
    buffer.insert(text, EditKind::Paste, 10).unwrap();
}

fn wait_preview(app: &mut AgentsWindow) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_markdown_preview(&egui::Context::default());
        let preview = &app.view.markdown_preview.as_ref().unwrap().preview;
        assert!(
            preview.error.is_none(),
            "preview error: {:?}",
            preview.error
        );
        if !preview.loading && app.markdown_preview.due.is_none() && preview.document.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "preview did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn contains_heading(app: &AgentsWindow, heading: &str) -> bool {
    let tab = app.view.markdown_preview.as_ref().unwrap();
    let Some(Document::Markdown(document)) = tab.preview.document.as_deref() else {
        return false;
    };
    document.blocks.iter().any(|block| {
        matches!(block,
        openide_preview::markdown::MarkdownBlock::Heading { id, .. } if id == heading)
    })
}

fn action(app: &mut AgentsWindow, operation: &str) {
    let id = format!(
        "{}:{operation}",
        app.view.markdown_preview.as_ref().unwrap().action_prefix()
    );
    assert!(app.apply_markdown_preview_action(&UiAction::Clicked { id }, &egui::Context::default()));
}

fn secondary_frame(app: &mut AgentsWindow) -> Frame {
    let tab = app.view.markdown_preview.as_ref().unwrap();
    app.tree
        .set_root(crate::document_preview_ui::secondary(tab, 560.0, 600.0));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(560.0, 600.0))
}

#[test]
fn opening_action_validates_active_source_tab_and_reuses_preview() {
    let (_root, mut app) = fixture();
    let first = open_source(&mut app, "README.md");
    let id = format!("markdown_preview_open:{}", first.get());
    app.apply_action(
        UiAction::Clicked { id: id.clone() },
        &egui::Context::default(),
    );
    let preview = app.view.markdown_preview.as_ref().unwrap().id;
    app.apply_action(
        UiAction::Clicked { id: id.clone() },
        &egui::Context::default(),
    );
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, preview);
    open_source(&mut app, "other.md");
    app.apply_action(UiAction::Clicked { id }, &egui::Context::default());
    assert_eq!(
        app.view
            .markdown_preview
            .as_ref()
            .unwrap()
            .preview
            .scope
            .path,
        Path::new("README.md")
    );
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, preview);
}

#[test]
fn opening_and_selecting_preview_preserve_pointer_and_keyboard_focus() {
    for keyboard in [false, true] {
        let (_root, mut app) = fixture();
        let source = open_source(&mut app, "README.md");
        let opener = format!("markdown_preview_open:{}", source.get());
        app.tree.set_root(
            openide_ui::Element::button(&opener, "Vista previa").size(Some(160.0), Some(32.0)),
        );
        app.tree.layout(Size::new(560.0, 600.0));
        let activate = |app: &mut AgentsWindow, id: &str| {
            let events = if keyboard {
                assert!(app.tree.focus(id));
                vec![UiEvent::Key {
                    key: Key::Enter,
                    ctrl: false,
                    shift: false,
                }]
            } else {
                let frame = app.tree.layout(Size::new(560.0, 600.0));
                let target = frame.regions.iter().find(|region| region.id == id).unwrap();
                let point = Point::new(target.rect.x + 8.0, target.rect.y + 8.0);
                assert_eq!(frame.hit_test(point), Some(id));
                vec![UiEvent::PointerDown(point), UiEvent::PointerUp(point)]
            };
            let mut clicked = false;
            for event in events {
                for action in app.tree.handle_event(event) {
                    if matches!(&action, UiAction::Clicked { id: clicked_id } if clicked_id == id) {
                        clicked = true;
                    }
                    app.apply_action(action, &egui::Context::default());
                }
            }
            assert!(clicked);
        };
        activate(&mut app, &opener);
        let tab = format!(
            "{}:tab",
            app.view.markdown_preview.as_ref().unwrap().action_prefix()
        );
        secondary_frame(&mut app);
        assert!(app.focus_markdown_preview_anchor());
        assert_eq!(app.tree.focused_id(), Some(tab.as_str()));
        assert_eq!(app.tree.has_keyboard_focus(), keyboard);

        activate(&mut app, &tab);
        assert!(app.focus_markdown_preview_anchor());
        assert_eq!(app.tree.has_keyboard_focus(), keyboard);
        // Focus is applied once; later renders must not override Tab navigation.
        app.tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            ctrl: false,
            shift: false,
        });
        assert!(app.tree.has_keyboard_focus());
        assert!(!app.focus_markdown_preview_anchor());
        assert_ne!(app.tree.focused_id(), Some(tab.as_str()));
    }
}

#[test]
fn close_pointer_gesture_survives_render_generation_change_without_touching_dirty_source() {
    let (_root, mut app) = fixture();
    let source_tab = open_source(&mut app, "README.md");
    let source = controller(&app);
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let tab = app.view.markdown_preview.as_ref().unwrap();
    let close_id = format!("{}:close", tab.action_prefix());
    let generation = tab.preview.scope.generation;
    let frame = secondary_frame(&mut app);
    let close = frame
        .regions
        .iter()
        .find(|region| region.id == close_id && region.kind == HitKind::Button)
        .unwrap();
    let point = Point::new(
        close.rect.x + close.rect.width / 2.0,
        close.rect.y + close.rect.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(close_id.as_str()));
    for action in app.tree.handle_event(UiEvent::PointerDown(point)) {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.markdown_preview.is_some());
    append(&source, "\n## Keep this draft\n");
    let before_close = source.snapshot().unwrap();
    app.poll_markdown_preview(&egui::Context::default());
    assert_ne!(
        app.view
            .markdown_preview
            .as_ref()
            .unwrap()
            .preview
            .scope
            .generation,
        generation
    );
    let frame = secondary_frame(&mut app);
    assert_eq!(frame.hit_test(point), Some(close_id.as_str()));
    for action in app.tree.handle_event(UiEvent::PointerUp(point)) {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.markdown_preview.is_none());
    assert!(app.markdown_preview.due.is_none());
    assert_eq!(
        app.dock.active_tab(&app.dock_scope()).unwrap().id,
        source_tab
    );
    let after = source.snapshot().unwrap();
    assert_eq!(after.document_id(), before_close.document_id());
    assert_eq!(after.revision(), before_close.revision());
    assert_eq!(after.selection(), before_close.selection());
    assert!(after.is_dirty());
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        "# Saved\n"
    );
}

#[test]
fn ctrl_s_from_secondary_preview_saves_only_its_source_without_changing_primary_or_focus() {
    let (_root, mut app) = fixture();
    open_source(&mut app, "README.md");
    let source_a = controller(&app);
    append(&source_a, "\n## Draft A\n");
    let a_snapshot = source_a.snapshot().unwrap();
    let expected_a = a_snapshot.text_range(0..a_snapshot.char_count()).unwrap();
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let b_tab = open_source(&mut app, "other.md");
    let source_b = controller(&app);
    append(&source_b, "\n## Draft B\n");
    let before_b = source_b.snapshot().unwrap();
    let tab = app.view.markdown_preview.as_ref().unwrap();
    let focus = format!("{}:tab", tab.action_prefix());
    secondary_frame(&mut app);
    assert!(app.tree.focus(&focus));
    let ctx = egui::Context::default();
    assert!(app.handle_markdown_preview_key(
        &UiEvent::Key {
            key: Key::S,
            ctrl: true,
            shift: false
        },
        &ctx,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while source_a.snapshot().unwrap().is_dirty() {
        app.poll_code_editor(&ctx);
        assert!(
            Instant::now() < deadline,
            "saving preview source A did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        expected_a
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("other.md")).unwrap(),
        "# Other\n"
    );
    assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, b_tab);
    assert_eq!(app.view.code_editor.as_ref().unwrap().tab_id, b_tab.get());
    let after_b = source_b.snapshot().unwrap();
    assert_eq!(after_b.document_id(), before_b.document_id());
    assert_eq!(after_b.revision(), before_b.revision());
    assert_eq!(after_b.selection(), before_b.selection());
    assert!(after_b.is_dirty());
    assert_eq!(app.tree.focused_id(), Some(focus.as_str()));
    assert!(app.editor_services.focus_next.is_none());
}

#[test]
fn pending_document_link_pointer_gesture_cannot_navigate_reordered_rendered_links() {
    let (_root, mut app) = fixture();
    let source_tab = open_source(&mut app, "README.md");
    let source = controller(&app);
    append(&source, "\n[Same](other.md)\n\n[Same](code.rs)\n");
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let preview_id = app.view.markdown_preview.as_ref().unwrap().id;
    {
        let shared = source.shared_buffer();
        let mut buffer = shared.lock().unwrap();
        let length = buffer.snapshot().char_count();
        buffer
            .set_selection(Selection {
                anchor: 0,
                head: length,
            })
            .unwrap();
        buffer
            .insert(
                "# Saved\n\n[Same](code.rs)\n\n[Same](other.md)\n",
                EditKind::Paste,
                20,
            )
            .unwrap();
    }
    app.poll_markdown_preview(&egui::Context::default());
    assert!(app.view.markdown_preview.as_ref().unwrap().preview.loading);
    let frame = secondary_frame(&mut app);
    let region = frame
        .regions
        .iter()
        .find(|region| {
            region.id.contains(":pending:")
                && region.id.ends_with(":link:0")
                && region.kind == HitKind::Button
        })
        .unwrap();
    let old_id = region.id.clone();
    let point = Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(old_id.as_str()));
    for action in app.tree.handle_event(UiEvent::PointerDown(point)) {
        app.apply_action(action, &egui::Context::default());
    }
    app.markdown_preview.due = Some(Instant::now());
    wait_preview(&mut app);
    let frame = secondary_frame(&mut app);
    let new_id = frame.hit_test(point).unwrap();
    assert!(new_id.contains(":document:") && new_id.ends_with(":link:0"));
    assert_ne!(new_id, old_id);
    for action in app.tree.handle_event(UiEvent::PointerUp(point)) {
        app.apply_action(action, &egui::Context::default());
    }
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, preview_id);
    assert_eq!(
        app.view
            .markdown_preview
            .as_ref()
            .unwrap()
            .preview
            .scope
            .path,
        Path::new("README.md")
    );
    assert_eq!(
        app.dock.active_tab(&app.dock_scope()).unwrap().id,
        source_tab
    );
}

#[test]
fn secondary_preview_reuses_its_tab_and_reads_unsaved_source_while_another_file_is_active() {
    let (_root, mut app) = fixture();
    let source_tab = open_source(&mut app, "README.md");
    let source = controller(&app);
    append(&source, "\n## Unsaved\n");
    let before = source.snapshot().unwrap();
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let tab = app.view.markdown_preview.as_ref().unwrap();
    assert_eq!(tab.source_tab_id, source_tab.get());
    let id = tab.id;
    let prefix = tab.action_prefix();
    app.open_markdown_preview(&egui::Context::default());
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, id);
    assert!(contains_heading(&app, "unsaved"));
    let other_tab = open_source(&mut app, "code.rs");
    app.poll_markdown_preview(&egui::Context::default());
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, id);
    append(&source, "\n## Background buffer\n");
    wait_preview(&mut app);
    assert_eq!(
        app.dock.active_tab(&app.dock_scope()).unwrap().id,
        other_tab
    );
    assert!(contains_heading(&app, "background-buffer"));
    assert_eq!(
        app.view.markdown_preview.as_ref().unwrap().action_prefix(),
        prefix
    );
    assert_eq!(
        source.snapshot().unwrap().document_id(),
        before.document_id()
    );
    assert!(source.snapshot().unwrap().is_dirty());
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        "# Saved\n"
    );
}

#[test]
fn closing_secondary_tab_during_render_keeps_source_and_rejects_old_tab_actions() {
    let (_root, mut app) = fixture();
    let source_tab = open_source(&mut app, "README.md");
    let source = controller(&app);
    append(&source, "\n## Keep my draft\n");
    let before = source.snapshot().unwrap();
    app.open_markdown_preview(&egui::Context::default());
    app.poll_markdown_preview(&egui::Context::default());
    let old_id = app.view.markdown_preview.as_ref().unwrap().id;
    action(&mut app, "close");
    assert!(app.view.markdown_preview.is_none());
    assert!(app.markdown_preview.due.is_none());
    assert!(!app.poll_markdown_preview(&egui::Context::default()));
    assert_eq!(
        app.dock.active_tab(&app.dock_scope()).unwrap().id,
        source_tab
    );
    assert_eq!(source.snapshot().unwrap().revision(), before.revision());
    assert_eq!(source.snapshot().unwrap().selection(), before.selection());
    assert!(source.snapshot().unwrap().is_dirty());
    app.open_markdown_preview(&egui::Context::default());
    let new_id = app.view.markdown_preview.as_ref().unwrap().id;
    assert_ne!(old_id, new_id);
    app.apply_markdown_preview_action(
        &UiAction::Clicked {
            id: format!("markdown_preview:{old_id}:close"),
        },
        &egui::Context::default(),
    );
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, new_id);
    wait_preview(&mut app);
    assert!(contains_heading(&app, "keep-my-draft"));
}

#[test]
fn secondary_preview_is_owned_by_workspace_and_task() {
    let (_root, mut app) = fixture();
    open_source(&mut app, "README.md");
    app.open_markdown_preview(&egui::Context::default());
    app.poll_markdown_preview(&egui::Context::default());
    app.view.selected_session = Some("another-task".into());
    assert!(app.poll_markdown_preview(&egui::Context::default()));
    assert!(app.view.markdown_preview.is_none());
    app.view.selected_session = None;
    open_source(&mut app, "README.md");
    app.open_markdown_preview(&egui::Context::default());
    app.view.workspace.push_str("/another-workspace");
    assert!(app.poll_markdown_preview(&egui::Context::default()));
    assert!(app.view.markdown_preview.is_none());
}

#[test]
fn markdown_links_replace_secondary_tab_and_open_source_without_closing_preview() {
    let (_root, mut app) = fixture();
    let original = open_source(&mut app, "README.md");
    append(&controller(&app), "\n[Other](other.md#other)\n");
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let tab = app.view.markdown_preview.as_ref().unwrap();
    let old_id = tab.id;
    let link_id = format!("{}:link:0", tab.document_action_prefix());
    app.apply_markdown_preview_action(
        &UiAction::Clicked { id: link_id },
        &egui::Context::default(),
    );
    let new_id = app.view.markdown_preview.as_ref().unwrap().id;
    assert_ne!(old_id, new_id);
    assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, original);
    // other.md was never opened in CodeEditor; this preview reads it on its worker.
    wait_preview(&mut app);
    assert!(contains_heading(&app, "other"));
    assert_eq!(
        app.markdown_preview.pending_anchor,
        Some((new_id, "other".into()))
    );
    action(&mut app, "source");
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, new_id);
    assert!(
        matches!(&app.dock.active_tab(&app.dock_scope()).unwrap().kind,
        DockTabKind::FilePreview { path } if path == Path::new("other.md"))
    );
    let active = app.dock.active_tab(&app.dock_scope()).unwrap().id.get();
    assert_eq!(
        app.view.markdown_preview.as_ref().unwrap().source_tab_id,
        active
    );
}

#[test]
fn oversized_live_buffer_invalidates_in_flight_preview_before_reporting_error() {
    let (_root, mut app) = fixture();
    open_source(&mut app, "README.md");
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let previous = app
        .view
        .markdown_preview
        .as_ref()
        .unwrap()
        .preview
        .scope
        .clone();
    app.markdown_preview
        .worker
        .as_ref()
        .unwrap()
        .submit(Request {
            scope: previous.clone(),
            source: Some("# Obsolete".into()),
            page: 0,
            render_width: 100,
        })
        .unwrap();
    append(
        &controller(&app),
        &"x".repeat(openide_preview::markdown::MAX_SOURCE_BYTES + 1),
    );
    app.poll_markdown_preview(&egui::Context::default());
    assert_ne!(
        app.view
            .markdown_preview
            .as_ref()
            .unwrap()
            .preview
            .scope
            .generation,
        previous.generation
    );
    app.markdown_preview.due = Some(Instant::now());
    app.poll_markdown_preview(&egui::Context::default());
    for _ in 0..5 {
        app.poll_markdown_preview(&egui::Context::default());
        let preview = &app.view.markdown_preview.as_ref().unwrap().preview;
        assert!(preview
            .error
            .as_ref()
            .is_some_and(|error| error.contains("2 MiB")));
        assert!(!preview.loading && preview.document.is_none());
        assert!(app.markdown_preview.due.is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn failed_source_stops_loading_and_stale_document_links_do_not_navigate() {
    let (_root, mut app) = fixture();
    open_source(&mut app, "README.md");
    append(&controller(&app), "\n[Other](other.md)\n");
    app.open_markdown_preview(&egui::Context::default());
    wait_preview(&mut app);
    let tab = app.view.markdown_preview.as_ref().unwrap();
    let id = tab.id;
    let old_link = format!("{}:link:0", tab.document_action_prefix());
    action(&mut app, "reload");
    app.apply_markdown_preview_action(
        &UiAction::Clicked { id: old_link },
        &egui::Context::default(),
    );
    assert_eq!(app.view.markdown_preview.as_ref().unwrap().id, id);
    assert_eq!(
        app.view
            .markdown_preview
            .as_ref()
            .unwrap()
            .preview
            .scope
            .path,
        Path::new("README.md")
    );
    open_source(&mut app, "missing.md");
    app.open_markdown_preview(&egui::Context::default());
    app.poll_markdown_preview(&egui::Context::default());
    let tab = app.view.markdown_preview.as_ref().unwrap();
    assert!(tab.preview.error.is_some());
    assert!(!tab.preview.loading);
    assert!(app.markdown_preview.due.is_none());
}
