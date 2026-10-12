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

#[test]
fn ide_switch_previews_the_same_tab_and_returns_to_the_untouched_source() {
    let disk = (0..200).map(|line| format!("Línea {line}\n")).collect::<String>();
    let (_root, mut app) = fixture(&disk);
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    let tab = open(&mut app, "README.md");
    append(&mut app, "\n## Sin guardar\n");
    let editor_id = crate::code_editor_ui::view_id(tab.get());
    let shared = app
        .view
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .shared_buffer();
    shared.lock().unwrap().set_selection(Selection::caret(40)).unwrap();
    let rect = frame(&mut app)
        .regions
        .iter()
        .find(|region| region.id == editor_id)
        .unwrap()
        .rect;
    app.tree.handle_event(UiEvent::ScrollDirect {
        position: Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
        delta_y: -900.0,
    });
    let lines = |app: &mut AgentsWindow| {
        frame(app)
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, position, .. } if text.starts_with("Línea") => {
                    Some((text.clone(), position.y.round() as i32))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let source_lines = lines(&mut app);
    assert!(
        !source_lines.is_empty() && !source_lines.iter().any(|(text, _)| text == "Línea 0"),
        "the source is scrolled"
    );
    let before = snapshot(&app);

    let switch = |key: &str| format!("document_preview_view:{}:{key}", tab.get());
    click(&mut app, &switch("preview"));
    wait_preview(&mut app);
    let view = app.view.document_preview.as_ref().unwrap();
    assert_eq!(view.mode, PreviewMode::Preview);
    let Some(Document::Markdown(document)) = view.document.as_deref() else {
        panic!("Markdown model missing");
    };
    assert!(document.blocks.iter().any(|block| matches!(block,
        openide_preview::markdown::MarkdownBlock::Heading { id, .. } if id == "sin-guardar")));
    let previewed = frame(&mut app);
    for id in [switch("preview"), switch("source"), "editor_copy_relative".into()] {
        assert!(previewed.regions.iter().any(|region| region.id == id), "{id}");
    }
    assert!(!previewed.regions.iter().any(|region| region.id == editor_id
        || region.id.ends_with(":split")
        || region.id.ends_with(":reload")));
    assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, tab);
    assert!(app.view.markdown_preview.is_none());

    click(&mut app, &switch("source"));
    assert_eq!(app.view.document_preview.as_ref().unwrap().mode, PreviewMode::Source);
    assert_eq!(app.editor_services.focus_next.as_deref(), Some(editor_id.as_str()));
    assert_eq!(lines(&mut app), source_lines);
    let after = snapshot(&app);
    assert_eq!(
        (after.document_id(), after.revision(), after.selection()),
        (before.document_id(), before.revision(), before.selection())
    );
    assert!(after.is_dirty() && buffer_text(&app).contains("## Sin guardar"));
    assert_eq!(std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(), disk);
}

const DIAGRAMS: &str = "# Arquitectura\n\nEl flujo:\n\n```mermaid\nflowchart LR\n    A[Editor] --> B[Vista previa]\n```\n\nY el intercambio:\n\n```mermaid\nsequenceDiagram\n    Editor->>Vista previa: Cambia el texto\n    Vista previa-->>Editor: Se redibuja\n```\n\nOtro tipo queda como código:\n\n```mermaid\nclassDiagram\n    Animal <|-- Perro\n```\n";

/// The preview alone, tall enough to hold the whole document.
fn document_frame(app: &mut AgentsWindow) -> Frame {
    let size = Size::new(1200.0, 1600.0);
    app.tree
        .set_root(crate::document_preview_ui::build(&app.view, size.width, size.height).unwrap());
    app.tree.set_reduced_motion(true);
    app.tree.layout(size)
}

/// The whole window, as the app builds it: the preview and what opens over it.
fn window_frame(app: &mut AgentsWindow) -> Frame {
    let size = Size::new(app.viewport_width, app.viewport_height);
    app.tree
        .set_root(crate::view::build_window_view(&app.view, size.width, size.height));
    app.tree.set_reduced_motion(true);
    app.tree.layout(size)
}

fn strokes(frame: &Frame) -> usize {
    frame
        .commands
        .iter()
        .filter(|command| matches!(command, DrawCommand::Polyline { .. }))
        .count()
}

fn painted(frame: &Frame, value: &str) -> bool {
    frame.commands.iter().any(
        |command| matches!(command, DrawCommand::Text { text, .. } if text.contains(value)),
    )
}

fn diagram_actions(frame: &Frame, expand: bool) -> Vec<String> {
    let mut ids: Vec<String> = frame
        .regions
        .iter()
        .filter(|region| {
            region.id.starts_with(crate::rich_text::DIAGRAM_ACTION_PREFIX)
                && region.id.contains("expand:") == expand
        })
        .map(|region| region.id.clone())
        .collect();
    ids.dedup();
    ids
}

/// A pointer press and release on the middle of `id`, through the real tree.
fn press(app: &mut AgentsWindow, frame: &Frame, id: &str) {
    let rect = frame
        .regions
        .iter()
        .rev()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("rendered action missing: {id}"))
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_eq!(frame.hit_test(point), Some(id));
    for event in [UiEvent::PointerDown(point), UiEvent::PointerUp(point)] {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, &egui::Context::default());
        }
    }
}

fn replace(app: &mut AgentsWindow, from: &str, to: &str) {
    let editor = app.view.code_editor.as_ref().unwrap();
    let shared = editor.controller.as_ref().unwrap().shared_buffer();
    let mut buffer = shared.lock().unwrap();
    let snapshot = buffer.snapshot();
    let text = snapshot.text_range(0..snapshot.char_count()).unwrap();
    let start = text[..text.find(from).expect("the text to replace")]
        .chars()
        .count();
    buffer
        .set_selection(Selection {
            anchor: start,
            head: start + from.chars().count(),
        })
        .unwrap();
    buffer.insert(to, EditKind::Paste, 10).unwrap();
    let action = UiAction::EditorChanged {
        id: crate::code_editor_ui::view_id(editor.tab_id),
        document_id: buffer.document_id(),
        revision: buffer.revision(),
    };
    drop(buffer);
    app.apply_action(action, &egui::Context::default());
}

#[test]
fn a_markdown_file_draws_its_flowchart_and_its_sequence_diagram_in_the_preview() {
    let (_root, mut app) = fixture(DIAGRAMS);
    open(&mut app, "README.md");
    action(&mut app, "preview");
    wait_preview(&mut app);
    let frame = document_frame(&mut app);
    // One connector, then two lifelines and two messages.
    assert_eq!(strokes(&frame), 5);
    for text in ["Editor", "Vista previa", "Cambia el texto", "Se redibuja", "Ampliar", "Ver código"] {
        assert!(painted(&frame, text), "{text} is not painted");
    }
    assert!(!painted(&frame, "flowchart LR") && !painted(&frame, "sequenceDiagram"));
    // A kind the app does not draw keeps the preview's own code block.
    assert!(painted(&frame, "classDiagram") && painted(&frame, "Animal"));
    let titles = frame
        .commands
        .iter()
        .filter(|command| matches!(command, DrawCommand::Text { text, .. } if text == "DIAGRAMA"))
        .count();
    assert_eq!(titles, 2);
    assert_eq!(diagram_actions(&frame, true).len(), 2);
    assert_eq!(diagram_actions(&frame, false).len(), 2);
    for region in frame
        .regions
        .iter()
        .filter(|region| region.id.starts_with(crate::rich_text::DIAGRAM_ACTION_PREFIX))
    {
        assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= 1200.0);
    }

    // The switch on a diagram shows its source in place, and draws it again.
    let switch = diagram_actions(&frame, false).remove(1);
    press(&mut app, &frame, &switch);
    let source = document_frame(&mut app);
    assert_eq!(strokes(&source), 1, "only the flowchart is still drawn");
    assert!(painted(&source, "sequenceDiagram") && painted(&source, "Ver diagrama"));
    press(&mut app, &source, &switch);
    assert_eq!(strokes(&document_frame(&mut app)), 5);
}

#[test]
fn a_diagram_in_the_preview_opens_over_the_window_and_closes_again() {
    let (_root, mut app) = fixture(DIAGRAMS);
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    let tab = open(&mut app, "README.md");
    // "Vista previa | Markdown", the switch in the tab's breadcrumb row.
    let source = window_frame(&mut app);
    assert_eq!(strokes(&source), 0, "the source is shown as written");
    press(&mut app, &source, &format!("document_preview_view:{}:preview", tab.get()));
    wait_preview(&mut app);
    let closed = window_frame(&mut app);
    let drawn = strokes(&closed);
    assert!(drawn >= 5, "the preview draws both diagrams in the window");
    assert!(!closed.regions.iter().any(|region| region.id == "chat_diagram_source:close"));
    for (index, extra) in [(0, 1), (1, 4)] {
        // The close control, then Escape.
        for escape in [false, true] {
            let frame = window_frame(&mut app);
            let expand = diagram_actions(&frame, true).remove(index);
            press(&mut app, &frame, &expand);
            let open = window_frame(&mut app);
            assert_eq!(strokes(&open), drawn + extra, "the diagram is drawn again over the window");
            let close = open
                .regions
                .iter()
                .rev()
                .find(|region| region.id == "chat_diagram_source:close" && region.kind == HitKind::Button)
                .expect("the viewer has a close control")
                .rect;
            assert!(close.x >= 0.0 && close.x + close.width <= app.viewport_width && close.y >= 0.0);
            if escape {
                let event = UiEvent::Key { key: Key::Escape, ctrl: false, shift: false };
                for action in app.tree.handle_event(event) {
                    app.apply_action(action, &egui::Context::default());
                }
            } else {
                press(&mut app, &open, "chat_diagram_source:close");
            }
            let after = window_frame(&mut app);
            assert_eq!(strokes(&after), drawn);
            assert!(!after.regions.iter().any(|region| region.id == "chat_diagram_source:close"));
        }
    }
    // The preview under the viewer was never replaced or reloaded.
    let view = app.view.document_preview.as_ref().unwrap();
    assert_eq!(view.mode, PreviewMode::Preview);
    assert!(view.error.is_none() && !view.loading);
}

#[test]
fn the_preview_follows_an_edit_of_a_diagram_fence() {
    // Beside the source being typed in, and alone while the buffer changes under it.
    for mode in ["split", "preview"] {
        follows_an_edit(mode);
    }
}

fn follows_an_edit(mode: &str) {
    let (_root, mut app) = fixture(DIAGRAMS);
    open(&mut app, "README.md");
    action(&mut app, mode);
    wait_preview(&mut app);
    assert_eq!(strokes(&document_frame(&mut app)), 5, "{mode}");

    // A new message is drawn, and so is a new node.
    replace(&mut app, "    Vista previa-->>Editor: Se redibuja\n", "    Vista previa-->>Editor: Se redibuja\n    Editor->>Editor: Guarda el archivo\n");
    replace(&mut app, "B[Vista previa]\n", "B[Vista previa] --> C[Diagramas]\n");
    wait_preview(&mut app);
    let grown = document_frame(&mut app);
    assert_eq!(strokes(&grown), 7);
    assert!(painted(&grown, "Guarda el archivo") && painted(&grown, "Diagramas"));
    assert_eq!(diagram_actions(&grown, true).len(), 2);

    // What the app cannot draw falls back to the code, whole.
    replace(&mut app, "sequenceDiagram\n", "sequenceDiagram\n    create participant Disco\n");
    wait_preview(&mut app);
    let code = document_frame(&mut app);
    assert_eq!(strokes(&code), 2, "only the flowchart is drawn");
    assert!(painted(&code, "sequenceDiagram") && painted(&code, "Guarda el archivo"));
    assert_eq!(diagram_actions(&code, true).len(), 1);

    // And is drawn again once it can be.
    replace(&mut app, "    create participant Disco\n", "");
    wait_preview(&mut app);
    assert_eq!(strokes(&document_frame(&mut app)), 7);
    // Nothing was written to disk on the way.
    assert_eq!(
        std::fs::read_to_string(Path::new(&app.view.workspace).join("README.md")).unwrap(),
        DIAGRAMS
    );
}
