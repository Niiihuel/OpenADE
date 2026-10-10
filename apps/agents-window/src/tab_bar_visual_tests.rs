//! Opt-in review of shared tab chrome in the real IDE and Agents workspaces.
//! Existing editor, transcript and PTY snapshots supply the surrounding UI;
//! this export never opens files, starts shells or invokes a provider.
use std::path::Path;
use std::time::{Duration, Instant};

use openide_ui::{Element, Frame, HitKind, Point, Rect, Size, UiAction, UiEvent, UiTree};

use crate::{
    ide_workspace::WindowMode,
    theme,
    view::{self, DockTabView, SideTab, ViewState},
};

const DIRECTORY: &str = "/tmp/openade-tab-bar-review";

fn fixture(mode: WindowMode) -> ViewState {
    let mut state = view::ide_visual_tests::fixture();
    state.ide.mode = mode;
    state.show_side_pane = true;
    state.show_terminal_panel = true;
    state.dock_tabs.extend(
        [
            "README.md",
            "editor_workbench_app.rs",
            "document_markdown_ui.rs",
            "sidebar_chrome.rs",
            "terminal_dock_ui.rs",
            "architecture.md",
            "workspace_layout_tests.rs",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, title)| DockTabView {
            id: 100 + index as u64,
            title: title.into(),
            full_path: Some(format!("crates/openide-ui/src/{title}")),
            active: false,
            kind: SideTab::FilePreview,
            closable: true,
        }),
    );
    state.terminal_tabs.extend(
        [
            "cargo test · UI",
            "cargo test · editor",
            "cargo check · workspace",
            "Servidor de desarrollo",
            "Build de escritorio",
            "Validación de Markdown",
            "Pruebas de integración",
            "Revisión de estilos",
            "Servidor de lenguaje",
            "Documentación local",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, title)| DockTabView {
            id: 200 + index as u64,
            title: title.into(),
            full_path: Some(state.workspace.clone()),
            active: false,
            kind: SideTab::Terminal,
            closable: true,
        }),
    );
    state
}

fn bounds(frame: &Frame, id: &str) -> Option<Rect> {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .map(|region| region.rect)
}

fn target_point(frame: &Frame, id: &str) -> Option<Point> {
    let rect = bounds(frame, id)?;
    // The tab owns its label/icon; its close is an independent nested target.
    [0.5, 0.2, 0.8].into_iter().find_map(|fraction| {
        let point = Point::new(rect.x + rect.width * fraction, rect.y + rect.height / 2.0);
        (frame.hit_test(point) == Some(id)).then_some(point)
    })
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new(DIRECTORY);
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}-{:.0}x{:.0}.svg", size.width, size.height));
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let svg = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &svg,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.save_png(path.with_extension("png")).unwrap();
}

fn hover_export(tree: &mut UiTree, size: Size, at: Instant, id: &str, name: &str) -> bool {
    let frame = tree.layout_at(size, at);
    let Some(point) = target_point(&frame, id) else {
        return false;
    };
    let before = bounds(&frame, id).unwrap();
    tree.handle_event_at(UiEvent::PointerMove(point), at);
    let hovered = tree.layout_at(size, at + Duration::from_millis(16));
    assert_eq!(tree.hovered_id(), Some(id));
    assert_eq!(
        bounds(&hovered, id),
        Some(before),
        "hover must not move a tab or its close target"
    );
    export(&hovered, size, name);
    tree.handle_event_at(UiEvent::PointerDown(point), at + Duration::from_millis(32));
    let actions = tree.handle_event_at(UiEvent::PointerUp(point), at + Duration::from_millis(48));
    assert!(
        actions.contains(&UiAction::Clicked { id: id.into() }),
        "{id} remains an independent native action"
    );
    true
}

fn scroll_strip(tree: &mut UiTree, size: Size, at: Instant, id: &str) -> bool {
    let frame = tree.layout_at(size, at);
    let Some(strip) = bounds(&frame, id) else {
        return false;
    };
    tree.handle_event_at(
        UiEvent::Scroll2D {
            position: Point::new(strip.x + strip.width / 2.0, strip.y + strip.height / 2.0),
            delta_x: -620.0,
            delta_y: 0.0,
            shift: false,
            direct: true,
        },
        at,
    );
    true
}

fn workspace_review(state: &ViewState, size: Size, name: &str) {
    let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
        state,
        size.width,
        size.height,
    ));
    let now = Instant::now();
    let idle = tree.layout_at(size, now);
    assert!(
        bounds(&idle, "dock_tab:71").is_some(),
        "real editor tabs are visible at {size:?}"
    );
    assert!(
        bounds(&idle, "terminal_panel_hide").is_some(),
        "real terminal header is visible"
    );
    export(&idle, size, &format!("{name}-idle"));
    assert!(hover_export(
        &mut tree,
        size,
        now + Duration::from_millis(100),
        "dock_tab:71",
        &format!("{name}-hover-editor")
    ));
    assert!(hover_export(
        &mut tree,
        size,
        now + Duration::from_millis(200),
        "dock_close:71",
        &format!("{name}-hover-close")
    ));
    let terminal_tab = if state.ide.mode == WindowMode::Agents {
        "terminal_tab:81"
    } else {
        "ide_bottom:terminal"
    };
    assert!(hover_export(
        &mut tree,
        size,
        now + Duration::from_millis(300),
        terminal_tab,
        &format!("{name}-hover-terminal")
    ));

    let toolbar = bounds(&idle, "dock_tabs_menu").unwrap();
    assert!(scroll_strip(
        &mut tree,
        size,
        now + Duration::from_millis(400),
        "right_dock_tabs"
    ));
    if state.ide.mode == WindowMode::Agents {
        assert!(scroll_strip(
            &mut tree,
            size,
            now + Duration::from_millis(450),
            "terminal_tabs"
        ));
    }
    tree.handle_event_at(UiEvent::PointerLeave, now + Duration::from_millis(480));
    let scrolled = tree.layout_at(size, now + Duration::from_millis(500));
    assert_eq!(
        bounds(&scrolled, "dock_tabs_menu"),
        Some(toolbar),
        "group tools remain fixed while tabs scroll"
    );
    assert!(
        scrolled.regions.iter().any(|region| {
            region.kind == HitKind::Button && region.id.starts_with("dock_tab:10")
        }),
        "manual overflow scroll reveals later files"
    );
    export(&scrolled, size, &format!("{name}-overflow"));
    let inactive = scrolled
        .regions
        .iter()
        .find(|region| {
            region.kind == HitKind::Button
                && region.id.starts_with("dock_tab:")
                && region.id != "dock_tab:71"
                && target_point(&scrolled, &region.id).is_some()
        })
        .map(|region| region.id.clone());
    if let Some(id) = inactive {
        hover_export(
            &mut tree,
            size,
            now + Duration::from_millis(600),
            &id,
            &format!("{name}-hover-overflow-tab"),
        );
    }
    context.end_pass().textures_delta.clear();
}

fn compact_headers_review() {
    let state = fixture(WindowMode::Agents);
    let size = Size::new(320.0, 192.0);
    let caption = |value| {
        Element::text(value)
            .font_family(openide_ui::FontFamily::Sans)
            .font_size(11.0)
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(None, Some(24.0))
    };
    let root = Element::column(vec![
        caption("Dock de archivos · Agentes"),
        view::dock_chrome::header(&state, size.width),
        Element::spacer().size(None, Some(24.0)),
        caption("Terminal · Agentes"),
        crate::terminal_dock_ui::header(&state, size.width),
        Element::spacer().flex(1.0),
    ])
    .background(theme::BACKGROUND)
    .size(Some(size.width), Some(size.height));
    let (context, mut tree) = view::ide_visual_tests::native_tree(root);
    let now = Instant::now();
    export(&tree.layout_at(size, now), size, "headers-idle");
    assert!(hover_export(
        &mut tree,
        size,
        now + Duration::from_millis(100),
        "dock_close:71",
        "headers-hover-close"
    ));
    assert!(hover_export(
        &mut tree,
        size,
        now + Duration::from_millis(200),
        "terminal_close:81",
        "headers-hover-terminal-close"
    ));
    assert!(scroll_strip(
        &mut tree,
        size,
        now + Duration::from_millis(300),
        "right_dock_tabs"
    ));
    assert!(scroll_strip(
        &mut tree,
        size,
        now + Duration::from_millis(350),
        "terminal_tabs"
    ));
    tree.handle_event_at(UiEvent::PointerLeave, now + Duration::from_millis(400));
    export(
        &tree.layout_at(size, now + Duration::from_millis(450)),
        size,
        "headers-overflow",
    );
    context.end_pass().textures_delta.clear();
}

#[test]
#[ignore = "exports integrated native tab chrome, hover and overflow with bundled fonts"]
fn export_tab_bar_visual_review() {
    for (mode, name) in [(WindowMode::Ide, "ide"), (WindowMode::Agents, "agents")] {
        let state = fixture(mode);
        for size in [Size::new(1440.0, 900.0), Size::new(1040.0, 720.0)] {
            workspace_review(&state, size, name);
        }
    }
    compact_headers_review();
}

#[test]
#[ignore = "exports native breadcrumb and footer cell hovers alongside the shared panel tabs"]
fn export_bar_hover_visual_review() {
    let state = view::ide_visual_tests::fixture();
    let symbol = state
        .editor_workbench
        .analysis
        .as_ref()
        .unwrap()
        .symbols
        .first()
        .unwrap();
    state
        .code_editor
        .as_ref()
        .unwrap()
        .controller
        .as_ref()
        .unwrap()
        .shared_buffer()
        .lock()
        .unwrap()
        .set_selection(openide_editor::Selection::caret(symbol.range.start + 1))
        .unwrap();
    let size = Size::new(1440.0, 900.0);
    let (context, mut tree) = view::ide_visual_tests::native_tree(view::build_window_view(
        &state,
        size.width,
        size.height,
    ));
    let now = Instant::now();
    let initial = tree.layout_at(size, now);
    export(&initial, size, "bar-cells-idle");
    for (id, name) in [
        ("editor_copy_relative", "breadcrumb-file"),
        ("editor_symbol:0", "breadcrumb-symbol"),
        ("ide_git_review", "footer-branch"),
        ("ide_editor_diagnostics", "footer-diagnostics"),
        ("ide_editor_cursor", "footer-cursor"),
    ] {
        let point = target_point(&initial, id).unwrap_or_else(|| panic!("missing {id}"));
        tree.handle_event_at(UiEvent::PointerMove(point), now);
        let frame = tree.layout_at(size, now + Duration::from_millis(200));
        assert_eq!(tree.hovered_id(), Some(id));
        export(&frame, size, name);
    }
    context.end_pass().textures_delta.clear();
}
