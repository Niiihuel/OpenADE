//! Real file/buffer/worker flows; no renderer or document service is mocked.
use super::*;
use openide_editor::{BufferSnapshot, EditKind, Selection};
use openide_ui::{DrawCommand, Frame, HitKind, Key, Point};

fn fixture(source: &str) -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("README.md"), source).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.viewport_width = 1440.0;
    app.viewport_height = 900.0;
    app.view.workspace = workspace.canonicalize().unwrap().display().to_string();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.project_branch = None;
    app.dock = DockState::default();
    (root, app)
}

fn open(app: &mut AgentsWindow, path: &str) -> DockTabId {
    let scope = app.dock_scope();
    let tab = app
        .dock
        .open(&scope, DockTabKind::FilePreview { path: path.into() });
    app.dock.activate(&scope, tab);
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
        assert!(Instant::now() < deadline, "editor load did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    tab
}

fn snapshot(app: &AgentsWindow) -> BufferSnapshot {
    app.view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap()
}

fn buffer_text(app: &AgentsWindow) -> String {
    let snapshot = snapshot(app);
    snapshot.text_range(0..snapshot.char_count()).unwrap()
}

fn append(app: &mut AgentsWindow, value: &str) {
    let editor = app.view.code_editor.as_ref().unwrap();
    let shared = editor.controller.as_ref().unwrap().shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let length = buffer.snapshot().char_count();
    buffer.set_selection(Selection::caret(length)).unwrap();
    buffer.insert(value, EditKind::Paste, 10).unwrap();
    let action = UiAction::EditorChanged {
        id: crate::code_editor_ui::view_id(editor.tab_id),
        document_id: buffer.document_id(),
        revision: buffer.revision(),
    };
    drop(buffer);
    app.apply_action(action, &egui::Context::default());
}

fn action(app: &mut AgentsWindow, operation: &str) {
    let prefix = app.view.document_preview.as_ref().unwrap().action_prefix();
    app.apply_action(
        UiAction::Clicked {
            id: format!("{prefix}:{operation}"),
        },
        &egui::Context::default(),
    );
}

fn wait_preview(app: &mut AgentsWindow) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_document_preview(&ctx);
        let view = app.view.document_preview.as_ref().unwrap();
        assert!(view.error.is_none(), "preview failed: {:?}", view.error);
        if !view.loading && app.document_preview.due.is_none() && view.document.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "preview worker did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn frame(app: &mut AgentsWindow) -> Frame {
    app.tree
        .set_root(crate::document_preview_ui::build(&app.view, 760.0, 520.0).unwrap());
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(760.0, 520.0))
}

fn click(app: &mut AgentsWindow, id: &str) {
    let frame = frame(app);
    let region = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("rendered action missing: {id}"));
    let point = Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    assert_eq!(frame.hit_test(point), Some(id));
    for event in [UiEvent::PointerDown(point), UiEvent::PointerUp(point)] {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, &egui::Context::default());
        }
    }
}

#[test]
fn inline_markdown_presentations_preserve_the_shared_unsaved_buffer() {
    let disk = "# Saved\r\n\r\nOriginal disk content.\r\n";
    let (_root, mut app) = fixture(disk);
    let tab = open(&mut app, "README.md");
    append(&mut app, "\r\n## Unsaved 🌎\r\n");
    let controller = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .clone()
        .unwrap();
    let before = controller.snapshot().unwrap();
    action(&mut app, "preview");
    wait_preview(&mut app);
    let view = app.view.document_preview.as_ref().unwrap();
    assert_eq!(view.mode, PreviewMode::Preview);
    let Some(Document::Markdown(document)) = view.document.as_deref() else {
        panic!("Markdown model missing");
    };
    assert!(document.blocks.iter().any(|block| matches!(block,
        openide_preview::markdown::MarkdownBlock::Heading { id, .. } if id == "unsaved-")));
    assert!(
        !frame(&mut app)
            .regions
            .iter()
            .any(|region| region.id == crate::code_editor_ui::view_id(tab.get()))
    );
    action(&mut app, "split");
    wait_preview(&mut app);
    assert!(
        frame(&mut app)
            .regions
            .iter()
            .any(|region| region.id == crate::code_editor_ui::view_id(tab.get()))
    );
    let current = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(
        &controller.shared_buffer(),
        &current.shared_buffer()
    ));
    assert_eq!(snapshot(&app).document_id(), before.document_id());
    assert_eq!(snapshot(&app).revision(), before.revision());
    assert_eq!(snapshot(&app).selection(), before.selection());
    assert!(snapshot(&app).is_dirty());
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        disk
    );
    action(&mut app, "source");
    assert_eq!(
        app.view.document_preview.as_ref().unwrap().mode,
        PreviewMode::Source
    );
    assert_eq!(snapshot(&app).revision(), before.revision());
    assert!(buffer_text(&app).contains("Unsaved 🌎"));
}

#[test]
fn failed_markdown_load_surfaces_an_error_and_stops_repaint_retries() {
    let (_root, mut app) = fixture("# Existing");
    open(&mut app, "missing.md");
    assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    action(&mut app, "preview");
    app.poll_document_preview(&egui::Context::default());
    let preview = app.view.document_preview.as_ref().unwrap();
    assert!(preview.error.is_some());
    assert!(!preview.loading && app.document_preview.due.is_none());
    let text = frame(&mut app)
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!text.contains("Preparando"));

    // A terminal load failure without a diagnostic still cannot spin forever.
    app.view.code_editor.as_mut().unwrap().error = None;
    action(&mut app, "reload");
    app.poll_document_preview(&egui::Context::default());
    assert!(
        app.view
            .document_preview
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .is_some_and(|error| error.contains("buffer"))
    );
    assert!(app.document_preview.due.is_none());
}

#[test]
fn stale_preview_generation_and_workspace_actions_cannot_change_current_presentation() {
    let (_root, mut app) = fixture("# Current");
    open(&mut app, "README.md");
    let stale = app.view.document_preview.as_ref().unwrap().action_prefix();
    action(&mut app, "preview");
    wait_preview(&mut app);
    assert_ne!(
        stale,
        app.view.document_preview.as_ref().unwrap().action_prefix()
    );
    for operation in ["source", "split", "reload", "link:0"] {
        assert!(app.apply_document_preview_action(
            &UiAction::Clicked {
                id: format!("{stale}:{operation}")
            },
            &egui::Context::default()
        ));
    }
    assert_eq!(
        app.view.document_preview.as_ref().unwrap().mode,
        PreviewMode::Preview
    );
    assert!(app.document_preview.due.is_none());
    let current = app.view.document_preview.as_ref().unwrap().action_prefix();
    let original = app.view.workspace.clone();
    app.view.workspace = format!("{original}/other-project");
    app.apply_document_preview_action(
        &UiAction::Clicked {
            id: format!("{current}:source"),
        },
        &egui::Context::default(),
    );
    assert_eq!(
        app.view.document_preview.as_ref().unwrap().mode,
        PreviewMode::Preview
    );
}

#[test]
fn hidden_markdown_source_rejects_residual_edit_actions() {
    let (_root, mut app) = fixture("# Keep this line\n");
    open(&mut app, "README.md");
    let original = snapshot(&app);
    action(&mut app, "preview");
    wait_preview(&mut app);
    frame(&mut app);
    app.apply_action(
        UiAction::Clicked {
            id: "editor_duplicate".into(),
        },
        &egui::Context::default(),
    );
    assert!(!app.handle_editor_workbench_key(
        &UiEvent::Key {
            key: Key::D,
            ctrl: true,
            shift: true
        },
        &egui::Context::default()
    ));
    assert_eq!(snapshot(&app).revision(), original.revision());
    assert_eq!(buffer_text(&app), "# Keep this line\n");
    assert!(!snapshot(&app).is_dirty());
}

#[test]
fn markdown_heading_link_moves_the_real_scroll_viewport_without_opening_a_tab() {
    let source = format!(
        "[Jump to destination](#destination)\n\n{}# Destination\n\nThe end.",
        "Long paragraph before target.\n\n".repeat(40)
    );
    let (_root, mut app) = fixture(&source);
    let tab = open(&mut app, "README.md");
    action(&mut app, "preview");
    wait_preview(&mut app);
    let prefix = app.view.document_preview.as_ref().unwrap().action_prefix();
    click(&mut app, &format!("{prefix}:link:0"));
    assert!(app.focus_document_preview_anchor());
    let revealed = frame(&mut app);
    let view = app.view.document_preview.as_ref().unwrap();
    assert!(app.tree.scroll_offset(&view.scroll_id()) > 0.0);
    assert!(
        revealed
            .regions
            .iter()
            .any(|region| region.id == format!("{prefix}:heading:destination"))
    );
    assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, tab);
    assert!(app.document_preview.pending_anchor.is_none());
}

fn two_page_pdf() -> Vec<u8> {
    let streams = [
        "0.1 0.3 0.7 rg 10 10 80 100 re f",
        "0.2 0.6 0.3 rg 15 15 70 90 re f",
    ];
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 120] /Contents 4 0 R >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            streams[0].len(),
            streams[0]
        ),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 120] /Contents 6 0 R >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            streams[1].len(),
            streams[1]
        ),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn returning_to_pdf_restores_page_and_zoom_after_visiting_markdown() {
    let (_root, mut app) = fixture("# Other file");
    std::fs::write(
        Path::new(&app.view.workspace).join("report.pdf"),
        two_page_pdf(),
    )
    .unwrap();
    let pdf_tab = open(&mut app, "report.pdf");
    wait_preview(&mut app);
    assert!(app.view.code_editor.is_none());
    assert_eq!(app.view.document_preview.as_ref().unwrap().pages, 2);
    action(&mut app, "next");
    wait_preview(&mut app);
    assert_eq!(app.view.document_preview.as_ref().unwrap().page, 1);
    frame(&mut app);
    action(&mut app, "zoom_in");
    let zoom = app.view.document_preview.as_ref().unwrap().zoom;
    assert!(zoom.is_some());
    open(&mut app, "README.md");
    assert!(app.view.code_editor.is_some());
    assert_eq!(open(&mut app, "report.pdf"), pdf_tab);
    assert_eq!(app.view.document_preview.as_ref().unwrap().page, 1);
    assert_eq!(app.view.document_preview.as_ref().unwrap().zoom, zoom);
    wait_preview(&mut app);
    assert!(matches!(
        app.view
            .document_preview
            .as_ref()
            .unwrap()
            .document
            .as_deref(),
        Some(Document::Pdf {
            page: 1,
            pages: 2,
            ..
        })
    ));
}

#[test]
fn oversized_live_markdown_invalidates_queued_render_and_keeps_the_error() {
    let (_root, mut app) = fixture("# Last valid render\n");
    open(&mut app, "README.md");
    action(&mut app, "preview");
    // Leave a real valid worker response queued/in flight while the live buffer
    // grows beyond the preview limit; the host has not polled its response yet.
    app.submit_document_preview();
    let previous_scope = app.view.document_preview.as_ref().unwrap().scope.clone();
    assert!(app.view.document_preview.as_ref().unwrap().loading);
    append(
        &mut app,
        &"x".repeat(openide_preview::markdown::MAX_SOURCE_BYTES),
    );
    let revision = snapshot(&app).revision();
    app.submit_document_preview();
    let failed = app.view.document_preview.as_ref().unwrap();
    assert_ne!(failed.scope.generation, previous_scope.generation);
    assert!(
        failed
            .error
            .as_ref()
            .is_some_and(|error| error.contains("2 MiB"))
    );
    assert!(!failed.loading && failed.document.is_none());
    assert!(app.document_preview.due.is_none());
    assert!(
        app.document_preview
            .worker
            .as_ref()
            .unwrap()
            .try_recv()
            .is_none()
    );
    // Poll after the worker has had opportunities to finish its old request.
    // Neither that completion nor observing the oversized revision can clear
    // the diagnostic or restore the outdated document.
    for _ in 0..12 {
        app.poll_document_preview(&egui::Context::default());
        let view = app.view.document_preview.as_ref().unwrap();
        assert!(
            view.error
                .as_ref()
                .is_some_and(|error| error.contains("2 MiB"))
        );
        assert!(!view.loading && view.document.is_none());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(snapshot(&app).revision(), revision);
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        "# Last valid render\n"
    );
}

#[test]
fn pending_navigation_to_previewed_markdown_reveals_source_and_applies_utf16_position() {
    let (_root, mut app) = fixture("# Heading\n\n🌎 target line\n");
    let tab = open(&mut app, "README.md");
    action(&mut app, "preview");
    wait_preview(&mut app);
    frame(&mut app);
    let original = snapshot(&app);
    app.editor_services.pending_navigation = Some((
        PathBuf::from("README.md"),
        openide_language::Position {
            line: 2,
            character: 3,
        },
    ));
    app.poll_editor_workbench(&egui::Context::default());
    assert_eq!(
        app.view.document_preview.as_ref().unwrap().mode,
        PreviewMode::Source
    );
    assert!(app.editor_services.pending_navigation.is_none());
    let selected = snapshot(&app);
    let position = selected.position_of(selected.selection().head).unwrap();
    assert_eq!(position.row, 2);
    assert_eq!(
        position.column, 2,
        "UTF-16 surrogate pair is one editor character"
    );
    assert_eq!(selected.document_id(), original.document_id());
    assert_eq!(selected.revision(), original.revision());
    assert!(!selected.is_dirty());
    assert!(
        frame(&mut app)
            .regions
            .iter()
            .any(|region| region.id == crate::code_editor_ui::view_id(tab.get()))
    );
}

#[test]
fn ctrl_s_in_markdown_preview_saves_draft_without_revealing_or_focusing_source() {
    let (_root, mut app) = fixture("# Original\n");
    open(&mut app, "README.md");
    append(&mut app, "\nDraft kept in preview.\n");
    let expected = buffer_text(&app);
    action(&mut app, "preview");
    wait_preview(&mut app);
    frame(&mut app);
    let focus = format!(
        "{}:preview",
        app.view.document_preview.as_ref().unwrap().chrome_action_prefix()
    );
    assert!(app.tree.focus(&focus));
    let ctx = egui::Context::default();
    assert!(app.handle_editor_workbench_key(
        &UiEvent::Key {
            key: Key::S,
            ctrl: true,
            shift: false
        },
        &ctx
    ));
    assert!(app.editor_services.focus_next.is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_code_editor(&ctx);
        if !snapshot(&app).is_dirty() && !app.view.code_editor.as_ref().unwrap().saving {
            break;
        }
        assert!(Instant::now() < deadline, "preview save did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        expected
    );
    assert_eq!(
        app.view.document_preview.as_ref().unwrap().mode,
        PreviewMode::Preview
    );
    assert!(app.editor_services.focus_next.is_none());
    assert_eq!(app.tree.focused_id(), Some(focus.as_str()));
}
