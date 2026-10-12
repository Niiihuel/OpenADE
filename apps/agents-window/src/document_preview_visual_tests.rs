//! Opt-in native document QA: real worker, parser, rasterization and IDE chrome.
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use openide_editor::TextBuffer;
use openide_preview::{Document, PreviewKind, Request, Scope, Worker};
use openide_ui::{DrawCommand, EditorController, FontFamily, Frame, Point, Rect, Size, UiEvent};

use crate::{
    code_editor_ui::CodeEditorView,
    document_preview::{MarkdownPreviewTab, PreviewMode, PreviewView},
    editor_workbench_ui::EditorWorkbenchView,
    ide_workspace::{IdePanel, WindowMode},
    view::{self, DockTabView, FileRowView, SideTab, ViewState},
};

const DIRECTORY: &str = "/tmp/openade-document-preview-review";
const SOURCE: &str = "# Un proyecto, todas sus vistas\n\nEl editor y la vista previa comparten **el mismo archivo**. Consultá [la guía del proyecto](docs/guide.md) o saltá a [validación](#validacion).\n\n![Distribución del workspace](assets/workspace.png)\n\n## Recursos\n\n| Archivo | Vista |\n| --- | --- |\n| README.md | Markdown editable |\n| workspace.png | Imagen nativa |\n| workspace.pdf | Documento PDF |\n\n- El árbol conserva el contexto del proyecto.\n- El agente acompaña la edición del documento.\n\n## Validación\n\n```rust\nlet workspace = Workspace::open(path)?;\nworkspace.open_file(\"README.md\");\n```\n\n> Los cambios de texto se reflejan en la vista previa.\n";

/// A small picture of the workspace, as an SVG file and as the PNG drawn from it.
pub(crate) const PICTURE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="640" height="160" viewBox="0 0 640 160">
      <rect width="640" height="160" rx="10" fill="#292929"/>
      <rect x="12" y="28" width="116" height="120" rx="6" fill="#333333"/>
      <rect x="138" y="28" width="308" height="120" rx="6" fill="#151515"/>
      <rect x="456" y="28" width="172" height="120" rx="6" fill="#151515"/>
      <g fill="#d5d5d5" font-family="sans-serif" font-size="11"><text x="15" y="18">openADE</text><text x="26" y="50">Proyecto</text><text x="154" y="50">README.md</text><text x="472" y="50">Agente</text></g>
      <g fill="#9cb2bb"><rect x="25" y="68" width="12" height="9" rx="2"/><rect x="25" y="90" width="12" height="9" rx="2"/><rect x="25" y="112" width="12" height="9" rx="2"/></g>
      <g fill="#777"><rect x="44" y="71" width="55" height="4" rx="2"/><rect x="44" y="93" width="42" height="4" rx="2"/><rect x="44" y="115" width="61" height="4" rx="2"/></g>
      <g fill="#b991d4"><rect x="154" y="72" width="136" height="5" rx="2"/><rect x="154" y="91" width="242" height="4" rx="2"/><rect x="154" y="110" width="196" height="4" rx="2"/></g>
      <rect x="470" y="68" width="142" height="29" rx="7" fill="#292929"/><rect x="470" y="110" width="142" height="23" rx="9" fill="#292929"/><circle cx="598" cy="121" r="7" fill="#2874ff"/>
    </svg>"##;

fn write_fixture_files(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("assets")).unwrap();
    std::fs::create_dir_all(workspace.join("docs")).unwrap();
    std::fs::write(workspace.join("README.md"), SOURCE).unwrap();
    std::fs::write(workspace.join("docs/guide.md"), "# Guía del proyecto\n").unwrap();
    // A small known PNG, decoded by the same image worker as a real project asset.
    let parsed =
        resvg::usvg::Tree::from_str(PICTURE, &crate::settings_ui::visual_qa::native_svg_options())
            .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(640, 160).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels
        .save_png(workspace.join("assets/workspace.png"))
        .unwrap();
    std::fs::write(workspace.join("docs/workspace.pdf"), fixture_pdf()).unwrap();
}

fn fixture_pdf() -> Vec<u8> {
    // Standard PDF objects/xref, with real vector shapes and a built-in font.
    let streams = [
        "0.97 0.97 0.97 rg 0 0 300 400 re f 0.12 0.12 0.12 rg 20 318 260 62 re f BT /F1 20 Tf 1 1 1 rg 34 350 Td (openADE) Tj /F1 10 Tf 0 -18 Td (Workspace review - 01) Tj ET 0.2 0.2 0.2 rg BT /F1 14 Tf 20 285 Td (Native document preview) Tj /F1 10 Tf 0 -26 Td (Markdown, images and PDF in one workspace.) Tj 0 -20 Td (Editor source stays in its original tab.) Tj ET 0.2 0.45 0.95 rg 20 135 260 74 re f 0.88 0.88 0.88 rg 20 45 122 70 re f 158 45 122 70 re f",
        "1 1 1 rg 0 0 300 400 re f 0.12 0.12 0.12 rg BT /F1 20 Tf 20 350 Td (Validation) Tj /F1 11 Tf 0 -40 Td (01 - Workspace-scoped loading) Tj 0 -28 Td (02 - Native text and image rendering) Tj 0 -28 Td (03 - Shared compact toolbar) Tj ET 0.2 0.65 0.4 rg 20 90 260 95 re f",
    ];
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", streams[0].len(), streams[0]),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", streams[1].len(), streams[1]),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
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

fn loaded(
    worker: &Worker,
    workspace: &Path,
    path: &str,
    tab_id: u64,
    kind: PreviewKind,
    mode: PreviewMode,
) -> PreviewView {
    let scope = Scope {
        workspace: workspace.into(),
        path: path.into(),
        tab_id,
        generation: 1,
    };
    worker
        .submit(Request {
            scope: scope.clone(),
            source: (kind == PreviewKind::Markdown).then(|| SOURCE.into()),
            page: 0,
            render_width: 1200,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let response = loop {
        if let Some(response) = worker.try_recv() {
            break response;
        }
        assert!(
            Instant::now() < deadline,
            "preview worker timed out for {path}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(response.scope, scope);
    let document = response.result.unwrap();
    let (raster, page, pages) = match &document {
        Document::Markdown(document) => {
            assert!(
                document.images.iter().any(|image| image.image.is_some()),
                "the worker must load the local PNG"
            );
            (None, 0, 0)
        }
        Document::Image(image) => (
            crate::view::document_markdown_ui::image_to_raster(image, path),
            0,
            0,
        ),
        Document::Pdf { image, page, pages } => (
            crate::view::document_markdown_ui::image_to_raster(image, path),
            *page,
            *pages,
        ),
    };
    PreviewView {
        scope,
        kind,
        mode,
        document: Some(Arc::new(document)),
        raster,
        loading: false,
        error: None,
        page,
        pages,
        zoom: None,
    }
}

fn fixture(mut preview: PreviewView) -> ViewState {
    let mut state = view::ide_visual_tests::fixture();
    state.ide.mode = WindowMode::Ide;
    state.ide.explorer_open = true;
    state.ide.agent_open = true;
    state.ide.bottom_open = false;
    state.show_terminal_panel = false;
    state.workspace = preview.scope.workspace.to_string_lossy().into_owned();
    state.workspace_draft = state.workspace.clone();
    state.selected_session_title = Some("Documentar el workspace".into());
    state.prompt = "Revisá la guía y agregá un ejemplo de uso.".into();
    state.file_rows = [
        ("assets", 0, true, true),
        ("workspace.png", 1, false, false),
        ("docs", 0, true, true),
        ("guide.md", 1, false, false),
        ("workspace.pdf", 1, false, false),
        ("README.md", 0, false, false),
    ]
    .into_iter()
    .map(|(name, depth, is_dir, expanded)| FileRowView {
        name: name.into(),
        depth,
        is_dir,
        expanded,
        can_load_more: false,
    })
    .collect();
    let controller = (preview.kind == PreviewKind::Markdown)
        .then(|| EditorController::new(TextBuffer::new(SOURCE).unwrap()));
    state.editor_workbench = EditorWorkbenchView {
        document_id: controller
            .as_ref()
            .map(|value| value.snapshot().unwrap().document_id()),
        ..Default::default()
    };
    state.code_editor = (preview.kind == PreviewKind::Markdown).then(|| CodeEditorView {
        tab_id: preview.scope.tab_id,
        path: preview.scope.path.to_string_lossy().into_owned(),
        controller,
        loading: false,
        saving: false,
        error: None,
        notice: None,
        conflict: false,
        recovered: false,
        missing: false,
    });
    state.dock_tabs = [
        (101, "README.md"),
        (102, "assets/workspace.png"),
        (103, "docs/workspace.pdf"),
    ]
    .into_iter()
    .map(|(id, path)| DockTabView {
        id,
        title: Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        full_path: Some(path.into()),
        active: id == preview.scope.tab_id,
        kind: SideTab::FilePreview,
        closable: true,
    })
    .collect();
    state.side_tab = SideTab::FilePreview;
    state.markdown_preview =
        if preview.kind == PreviewKind::Markdown && preview.mode != PreviewMode::Source {
            let mut secondary = preview.clone();
            secondary.mode = PreviewMode::Preview;
            Some(MarkdownPreviewTab {
                id: 201,
                source_tab_id: preview.scope.tab_id,
                preview: secondary,
            })
        } else {
            None
        };
    if preview.kind == PreviewKind::Markdown {
        preview.mode = PreviewMode::Source;
    }
    state.document_preview = Some(preview);
    state
}

fn markdown_geometry_fixture() -> ViewState {
    fixture(PreviewView {
        scope: Scope {
            workspace: "/workspace/openADE".into(),
            path: "README.md".into(),
            tab_id: 101,
            generation: 7,
        },
        kind: PreviewKind::Markdown,
        mode: PreviewMode::Preview,
        document: Some(Arc::new(Document::Markdown(
            openide_preview::markdown::parse(SOURCE),
        ))),
        raster: None,
        loading: false,
        error: None,
        page: 0,
        pages: 0,
        zoom: None,
    })
}

fn region(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing native region {id}"))
        .rect
}

#[test]
fn markdown_preview_has_its_own_tab_close_and_toolbar_above_the_breadcrumb() {
    let state = markdown_geometry_fixture();
    for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let frame = tree.layout(size);
        let source_tab = region(&frame, "dock_tab:101");
        let source = region(&frame, "code_editor:101");
        let preview_tab = region(&frame, "markdown_preview:201:tab");
        let close = region(&frame, "markdown_preview:201:close");
        let preview = region(&frame, "markdown_preview_scroll:201");
        let center = Point::new(close.x + close.width / 2.0, close.y + close.height / 2.0);
        assert_eq!(frame.hit_test(center), Some("markdown_preview:201:close"));
        assert!(
            close.x >= preview_tab.x
                && close.x + close.width <= preview_tab.x + preview_tab.width + 0.1
        );
        assert!(
            (close.y + close.height / 2.0 - (preview_tab.y + crate::dock_tab_ui::TOOL_SIZE / 2.0))
                .abs()
                < 1.0
        );
        for id in ["editor_find", "markdown_preview_open:101", "editor_menu"] {
            let tool = region(&frame, id);
            assert!(
                (tool.y + tool.height / 2.0 - (source_tab.y + crate::dock_tab_ui::TOOL_SIZE / 2.0))
                    .abs()
                    < 1.0,
                "{id} shares the source tab row"
            );
            assert!(
                tool.y + tool.height < source.y,
                "{id} must not consume the breadcrumb row"
            );
        }
        let expected_body_offset =
            crate::dock_tab_ui::HEADER_HEIGHT + crate::code_editor_ui::MARKDOWN_ROW_HEIGHT;
        let source_group_top =
            source_tab.y - (crate::dock_tab_ui::HEADER_HEIGHT - crate::dock_tab_ui::TAB_HEIGHT);
        let preview_group_top =
            preview_tab.y - (crate::dock_tab_ui::HEADER_HEIGHT - crate::dock_tab_ui::TAB_HEIGHT);
        assert!((source.y - source_group_top - expected_body_offset).abs() < 0.1);
        assert!((preview.y - preview_group_top - expected_body_offset).abs() < 0.1);
        if size.width >= 640.0 {
            assert!(
                (source_tab.y - preview_tab.y).abs() < 0.1,
                "desktop group tab strips align"
            );
            assert!(
                preview.x >= source.x + source.width,
                "desktop preview is a separate adjacent group"
            );
        } else {
            assert!((source.x - preview.x).abs() < 0.1);
            assert!(
                preview_tab.y >= source.y + source.height,
                "compact preview has a separate lower tab strip"
            );
        }
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::Text { position, text, font_size, font_family: FontFamily::Monospace, .. }
                if source.contains(*position) && text.contains("proyecto") && (*font_size - 13.0).abs() < 0.1
        )), "the original Markdown source keeps its editor font size");
        context.end_pass().textures_delta.clear();
    }
}

#[test]
fn preview_uses_the_full_group_when_the_last_source_tab_closes() {
    let mut state = markdown_geometry_fixture();
    let size = Size::new(1440.0, 900.0);
    let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
        &state,
        size.width,
        size.height,
    ));
    let split = tree.layout(size);
    let split_width = region(&split, "markdown_preview_scroll:201").width;
    state.dock_tabs.clear();
    state.code_editor = None;
    state.document_preview = None;
    tree.set_root(view::build_window_view(&state, size.width, size.height));
    let full = tree.layout(size);
    assert!(region(&full, "markdown_preview_scroll:201").width > split_width * 1.8);
    assert!(!full.regions.iter().any(|r| r.id == "code_editor:101"));
    assert!(full
        .regions
        .iter()
        .any(|r| r.id == "markdown_preview:201:close"));
    context.end_pass().textures_delta.clear();
}

#[test]
fn closing_secondary_preview_returns_the_available_space_to_the_same_editor() {
    let mut state = markdown_geometry_fixture();
    let controller = state
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .clone()
        .unwrap();
    let original = controller.snapshot().unwrap();
    for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
        state.markdown_preview = markdown_geometry_fixture().markdown_preview;
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let before = tree.layout(size);
        let split = region(&before, "code_editor:101");
        state.markdown_preview = None;
        tree.set_root(view::build_window_view(&state, size.width, size.height));
        let after = tree.layout(size);
        let full = region(&after, "code_editor:101");
        assert!(!after
            .regions
            .iter()
            .any(|region| region.id.starts_with("markdown_preview:201:")));
        if size.width >= 640.0 {
            assert!(
                full.width > split.width * 1.8,
                "closing restores full editor width"
            );
            assert!((full.height - split.height).abs() < 0.1);
        } else {
            assert!(
                full.height > split.height * 1.8,
                "closing restores full editor height on compact windows"
            );
            assert!((full.width - split.width).abs() < 0.1);
        }
        let current = state
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
        assert_eq!(
            current.snapshot().unwrap().document_id(),
            original.document_id()
        );
        assert_eq!(current.snapshot().unwrap().revision(), original.revision());
        context.end_pass().textures_delta.clear();
    }
}

fn export(frame: &Frame, size: Size, name: &str) {
    export_to(Path::new(DIRECTORY), frame, size, name);
}

pub(crate) fn export_to(directory: &Path, frame: &Frame, size: Size, name: &str) {
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
}

/// Real source, parser and shared UI widgets for reviewing the compact editor.
#[test]
#[ignore = "exports native editor chrome, minimap and footer menus for visual review"]
fn export_editor_chrome_visual_review() {
    let source = include_str!("../../../docs/approvals-and-pending-selection.md");
    let path = "docs/approvals-and-pending-selection.md";
    let mut state = markdown_geometry_fixture();
    state.markdown_preview = None;
    state.document_preview.as_mut().unwrap().mode = PreviewMode::Source;
    state.document_preview.as_mut().unwrap().scope.path = path.into();
    state.ide.agent_open = false;
    let mut buffer = TextBuffer::new(source).unwrap();
    let offset = source.find("## Elección").unwrap_or(0);
    buffer
        .set_selection(openide_editor::Selection::caret(
            source[..offset].chars().count(),
        ))
        .unwrap();
    let controller = EditorController::new(buffer);
    let snapshot = controller.snapshot().unwrap();
    state.editor_workbench.document_id = Some(snapshot.document_id());
    state.editor_workbench.analysis = Some(Arc::new(
        openide_language::ParserEngine::default()
            .analyze(
                &snapshot,
                openide_language::Language::Markdown,
                &openide_language::Cancellation::default(),
            )
            .unwrap(),
    ));
    state.editor_workbench.highlight_spans = state
        .editor_workbench
        .analysis
        .as_ref()
        .unwrap()
        .highlights
        .iter()
        .map(|span| openide_ui::CodeHighlight {
            range: span.range.clone(),
            color: match span.kind {
                openide_language::HighlightKind::Comment => openide_ui::Color::rgb(126, 151, 119),
                openide_language::HighlightKind::String => openide_ui::Color::rgb(206, 163, 127),
                openide_language::HighlightKind::Keyword => openide_ui::Color::rgb(184, 143, 216),
                openide_language::HighlightKind::Number => openide_ui::Color::rgb(151, 193, 169),
                openide_language::HighlightKind::Type => openide_ui::Color::rgb(225, 196, 131),
                openide_language::HighlightKind::Function => openide_ui::Color::rgb(143, 185, 218),
                _ => crate::theme::FOREGROUND,
            },
        })
        .collect::<Vec<_>>()
        .into();
    let editor = state.code_editor.as_mut().unwrap();
    editor.path = path.into();
    editor.controller = Some(controller);
    state.dock_tabs = [(101, path), (104, "docs/language-services.md")]
        .into_iter()
        .map(|(id, path)| DockTabView {
            id,
            title: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            full_path: Some(path.into()),
            active: id == 101,
            kind: SideTab::FilePreview,
            closable: true,
        })
        .collect();
    state.file_rows = [
        ("apps", 0, true, false),
        ("crates", 0, true, false),
        ("docs", 0, true, true),
        ("alerts.md", 1, false, false),
        ("approvals-and-pending-selection.md", 1, false, false),
        ("architecture.md", 1, false, false),
        ("ide-workspace.md", 1, false, false),
        ("language-services.md", 1, false, false),
        ("native-code-editor-design.md", 1, false, false),
        ("native-document-preview.md", 1, false, false),
        ("README.md", 0, false, false),
    ]
    .into_iter()
    .map(|(name, depth, is_dir, expanded)| FileRowView {
        name: name.into(),
        depth,
        is_dir,
        expanded,
        can_load_more: false,
    })
    .collect();
    for (name, width, minimap, menu) in [
        ("editor-minimap", 1440.0, true, None),
        ("editor-no-minimap", 1440.0, false, None),
        ("editor-compact", 520.0, true, None),
        (
            "editor-language-menu",
            1440.0,
            true,
            Some(crate::ide_status_ui::StatusMenu::Language),
        ),
        (
            "editor-ending-menu",
            1440.0,
            true,
            Some(crate::ide_status_ui::StatusMenu::Ending),
        ),
    ] {
        let size = Size::new(width, if width < 600.0 { 680.0 } else { 900.0 });
        state.editor_workbench.minimap = minimap;
        state.ide_status = crate::ide_status_ui::IdeStatusView {
            menu,
            scope: menu.and_then(|_| crate::ide_status_ui::document_scope(&state)),
            workspace: menu.map(|_| state.workspace.clone()),
            ..Default::default()
        };
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let frame = tree.layout(size);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "code_editor:101"));
        assert!(
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::Text { text, color, .. } if text.starts_with("# Apro")
                    && *color == openide_ui::Color::rgb(225, 196, 131)
            )),
            "native source headings must carry the parser's highlight spans"
        );
        export(&frame, size, name);
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "exports native Markdown source and independent preview groups, images and PDFs"]
fn export_document_previews_visual_review() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("openADE");
    write_fixture_files(&workspace);
    let worker = Worker::new().unwrap();
    for (name, path, tab, kind, mode) in [
        (
            "markdown-source",
            "README.md",
            101,
            PreviewKind::Markdown,
            PreviewMode::Source,
        ),
        (
            "markdown-preview",
            "README.md",
            101,
            PreviewKind::Markdown,
            PreviewMode::Preview,
        ),
        (
            "image",
            "assets/workspace.png",
            102,
            PreviewKind::Image,
            PreviewMode::Preview,
        ),
        (
            "pdf",
            "docs/workspace.pdf",
            103,
            PreviewKind::Pdf,
            PreviewMode::Preview,
        ),
    ] {
        let state = fixture(loaded(&worker, &workspace, path, tab, kind, mode));
        for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
            let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
                &state,
                size.width,
                size.height,
            ));
            let frame = tree.layout(size);
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.id.ends_with(":reload")
                        || (kind == PreviewKind::Markdown
                            && region.id == "markdown_preview_open:101")),
                "actual preview toolbar is mounted"
            );
            if kind != PreviewKind::Markdown {
                assert!(
                    frame
                        .commands
                        .iter()
                        .any(|command| matches!(command, DrawCommand::RasterImage { .. })),
                    "worker output must be painted"
                );
            }
            if kind == PreviewKind::Markdown {
                assert!(
                    frame
                        .regions
                        .iter()
                        .any(|region| region.id == format!("code_editor:{tab}")),
                    "the primary group retains the original editable source"
                );
                if state.markdown_preview.is_some() {
                    assert!(
                        frame
                            .regions
                            .iter()
                            .any(|region| region.id == "markdown_preview:201:close"),
                        "secondary preview has an independent close"
                    );
                }
            }
            export(&frame, size, name);
            context.end_pass().textures_delta.clear();
            // An image only fits its panel; a PDF's page is zoomed and panned.
            if kind == PreviewKind::Pdf {
                let mut zoomed = state.document_preview.as_ref().unwrap().clone();
                zoomed.zoom = Some(1.25);
                let scroll_id = zoomed.scroll_id();
                let zoomed = fixture(zoomed);
                let (context, mut tree) = view::ide_visual_tests::native_tree(
                    view::build_window_view(&zoomed, size.width, size.height),
                );
                let frame = tree.layout(size);
                let viewport = frame
                    .regions
                    .iter()
                    .find(|r| r.id == scroll_id)
                    .unwrap()
                    .rect;
                let image_bounds = |frame: &Frame| {
                    frame
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            DrawCommand::RasterImage { rect, .. } => Some(*rect),
                            _ => None,
                        })
                        .unwrap()
                };
                let before = image_bounds(&frame);
                export(&frame, size, &format!("{name}-zoom"));
                tree.handle_event(UiEvent::Scroll2D {
                    position: Point::new(
                        viewport.x + viewport.width / 2.0,
                        viewport.y + viewport.height / 2.0,
                    ),
                    delta_x: -140.0,
                    delta_y: -120.0,
                    shift: false,
                    direct: true,
                });
                let moved = tree.layout(size);
                let after = image_bounds(&moved);
                assert!(
                    after.x < before.x,
                    "horizontal pan reaches the zoomed asset"
                );
                assert!(
                    after.y < before.y,
                    "the same gesture also pans the tall PDF vertically"
                );
                assert_eq!(
                    (after.width, after.height),
                    (before.width, before.height),
                    "panning preserves the image's scale and proportions"
                );
                export(&moved, size, &format!("{name}-zoom-pan"));
                context.end_pass().textures_delta.clear();
            }
            // Compact IDE uses its normal drawers instead of squeezing three
            // unusable columns into 520 px. Review both available drawers too.
            if size.width == 520.0 {
                for (suffix, panel) in [("files", IdePanel::Explorer), ("agent", IdePanel::Agent)] {
                    let mut expanded = fixture(state.document_preview.as_ref().unwrap().clone());
                    expanded.markdown_preview = state.markdown_preview.clone();
                    expanded.ide.compact_panel = Some(panel);
                    let (context, mut tree) = view::ide_visual_tests::native_tree(
                        view::build_window_view(&expanded, size.width, size.height),
                    );
                    export(&tree.layout(size), size, &format!("{name}-{suffix}"));
                    context.end_pass().textures_delta.clear();
                }
            }
        }
    }
}

#[test]
#[ignore = "exports the Markdown breadcrumb switch in both states as PNG under docs/ui-separation/shots/markdown-switch"]
fn export_markdown_switch_visual_review() {
    let directory = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/ui-separation/shots/markdown-switch"
    ));
    for (name, mode) in [("source", PreviewMode::Source), ("preview", PreviewMode::Preview)] {
        let mut state = markdown_geometry_fixture();
        state.markdown_preview = None;
        state.document_preview.as_mut().unwrap().mode = mode;
        for size in [Size::new(1100.0, 600.0), Size::new(520.0, 680.0)] {
            let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
                &state,
                size.width,
                size.height,
            ));
            let frame = tree.layout(size);
            let [preview, source] =
                ["preview", "source"].map(|key| region(&frame, &format!("document_preview_view:101:{key}")));
            let path = region(&frame, "editor_copy_relative");
            assert!(path.x + path.width <= preview.x, "the path ends before the switch");
            assert!(preview.x + preview.width <= source.x);
            assert_eq!(
                frame.regions.iter().any(|region| region.id == "code_editor:101"),
                mode == PreviewMode::Source
            );
            export_to(directory, &frame, size, name);
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
#[ignore = "exports a Markdown preview with drawn diagrams as PNG under /tmp/openade-sequence-diagrams-review"]
fn export_markdown_diagrams_visual_review() {
    let directory = Path::new("/tmp/openade-sequence-diagrams-review");
    let source = "# Inicio de sesión\n\nEl formulario envía las credenciales y la app guarda el token que recibe.\n\n\
         ```mermaid\nsequenceDiagram\n    participant U as Usuario\n    participant A as App\n    participant S as Servidor\n    \
         U->>A: Ingresa sus credenciales\n    A->>+S: POST /login\n    S-->>-A: 200 OK con el token\n    A-->>U: Muestra el panel\n```\n\n\
         Los módulos que intervienen:\n\n\
         ```mermaid\nflowchart LR\n    form[Formulario] --> api[Cliente de la API] --> store[(Sesión)]\n    api -.->|falla| error[Aviso de error]\n```\n\n\
         Un tipo que la app no dibuja queda como código:\n\n```mermaid\nclassDiagram\n    Sesion <|-- SesionRemota\n```\n";
    let mut state = markdown_geometry_fixture();
    state.markdown_preview = None;
    let preview = state.document_preview.as_mut().unwrap();
    preview.mode = PreviewMode::Preview;
    preview.document = Some(Arc::new(Document::Markdown(openide_preview::markdown::parse(source))));
    for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let frame = tree.layout(size);
        let strokes = frame
            .commands
            .iter()
            .filter(|command| matches!(command, DrawCommand::Polyline { .. }))
            .count();
        // Three lifelines and four messages, then three connectors.
        assert_eq!(strokes, 10);
        export_to(directory, &frame, size, "p1-preview");
        let expand = frame
            .regions
            .iter()
            .find(|region| region.id.contains("expand:"))
            .expect("Ampliar")
            .id
            .clone();
        context.end_pass().textures_delta.clear();
        assert!(crate::rich_text::toggle_diagram_source(&expand));
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        export_to(directory, &tree.layout(size), size, "p2-preview-expanded");
        context.end_pass().textures_delta.clear();
        assert!(crate::rich_text::toggle_diagram_source("chat_diagram_source:close"));
    }
}

#[test]
#[ignore = "exports a Markdown preview of prose, lists, a quote, a table and code as PNG under docs/ui-separation/shots/markdown-reading"]
fn export_markdown_reading_visual_review() {
    let directory = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/ui-separation/shots/markdown-reading"
    ));
    let elements = "# Aprobaciones y selección pendiente\n\n\
        Las decisiones son permitir una vez, denegar y permitir la misma operación en la sesión. Esta última conserva el alcance \
        existente del harness: herramienta, argumentos y contexto; no equivale a autorizar todas las llamadas de una herramienta.\n\n\
        El host valida conversación, proyecto, subagente y solicitud antes de cada decisión.\n\n\
        ## Elección para el siguiente mensaje\n\n\
        El selector de modelo, esfuerzo y permisos sigue disponible durante una respuesta. La elección se presenta como «Para el \
        próximo mensaje», con «Deshacer». Volver a elegir reemplaza la selección pendiente.\n\n\
        - Se aplica cuando termina el worker del turno.\n- Antes de aplicar se vuelven a validar el catálogo y los permisos guardados, \
        y el consentimiento de acceso completo, que es lo que más tarda.\n  - Si ya no es válida, queda un aviso en el composer.\n\
        - Cambiar de conversación la descarta.\n\n\
        1. Abrí el selector.\n2. Elegí un modelo.\n3. Enviá el mensaje.\n\n\
        - [x] Selector pendiente\n- [ ] Validación antes de aplicar\n\n\
        > La decisión siempre corresponde a los argumentos completos.\n\n\
        ### Historial y cierre\n\n\
        `DismissButton` reutiliza `Kbd` para el cierre visible `Esc` y restaura el foco. Más en la [documentación](./alerts.md).\n\n\
        ```rust\nfn decide(request: &Request) -> Decision {\n    Decision::AllowOnce\n}\n```\n\n\
        | Decisión | Alcance |\n|---|---|\n| Permitir una vez | Esta llamada |\n| Permitir en la sesión | La misma operación |\n\n\
        ---\n\nLas pruebas cubren estados de permiso y replay.\n";
    let prose = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/approvals-and-pending-selection.md"
    ))
    .unwrap();
    for (name, source) in [("elements", elements), ("prose", prose.as_str())] {
        let mut state = markdown_geometry_fixture();
        state.markdown_preview = None;
        let preview = state.document_preview.as_mut().unwrap();
        preview.mode = PreviewMode::Preview;
        preview.document = Some(Arc::new(Document::Markdown(openide_preview::markdown::parse(source))));
        for size in [Size::new(1440.0, 900.0), Size::new(520.0, 680.0)] {
            let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
                &state,
                size.width,
                size.height,
            ));
            export_to(directory, &tree.layout(size), size, name);
            context.end_pass().textures_delta.clear();
        }
    }
}

#[test]
fn a_markdown_document_keeps_to_an_answers_reading_width_centered_in_its_pane() {
    let mut state = markdown_geometry_fixture();
    state.markdown_preview = None;
    state.document_preview.as_mut().unwrap().mode = PreviewMode::Preview;
    for size in [Size::new(2400.0, 900.0), Size::new(520.0, 680.0)] {
        let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
            &state,
            size.width,
            size.height,
        ));
        let frame = tree.layout(size);
        let pane = region(&frame, "document_preview_scroll:101");
        let heading = frame
            .regions
            .iter()
            .find(|region| region.id.contains(":heading:"))
            .expect("the document's title")
            .rect;
        // As wide as an answer, or the pane inside a page's margins.
        let width = (pane.width - 48.0).min(crate::session_surface::READING_WIDTH);
        assert_eq!(heading.width, width, "{size:?}");
        let (left, right) = (heading.x - pane.x, pane.x + pane.width - heading.x - heading.width);
        assert!((left - right).abs() < 0.5 && left >= 24.0, "{left} {right}");
        assert_eq!(heading.y - pane.y, 24.0);
        context.end_pass().textures_delta.clear();
    }
}
