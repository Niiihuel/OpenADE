//! Opt-in layout export using the application's real fonts and draw commands.
use super::*;
use eframe::egui;
use openide_ui::{Size, UiTree};

#[path = "hover_surface_tests.rs"]
mod hover_surface_tests;
#[path = "dock_document_status_tests.rs"]
mod dock_document_status_tests;
#[path = "dock_problems_tests.rs"]
mod dock_problems_tests;

pub(super) fn export(root: Element, width: f32, height: f32, name: &str) {
    export_with_pointer(root, width, height, name, None);
}

fn export_with_pointer(
    root: Element,
    width: f32,
    height: f32,
    name: &str,
    pointer: Option<openide_ui::Point>,
) {
    export_with_interaction(
        root,
        width,
        height,
        name,
        pointer.map(ExportInteraction::Pointer),
    );
}

pub(super) enum ExportInteraction {
    Pointer(openide_ui::Point),
    Hover(&'static str),
    Focus(&'static str),
    Scroll(&'static str, f32),
}

pub(super) fn export_with_interaction(
    root: Element,
    width: f32,
    height: f32,
    name: &str,
    interaction: Option<ExportInteraction>,
) -> openide_ui::Frame {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(
        Element::panel(root)
            .background(theme::BACKGROUND)
            .size(Some(width), Some(height)),
    );
    tree.set_reduced_motion(true);
    let measurer = ctx.clone();
    tree.set_text_measurer(move |text, size, family| {
        measurer.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::new(
                        size,
                        match family {
                            FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                            FontFamily::Monospace => egui::FontFamily::Monospace,
                        },
                    ),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    });
    let started = std::time::Instant::now();
    let size = Size::new(width, height);
    let initial = tree.layout_at(size, started);
    let delay = match interaction {
        Some(ExportInteraction::Pointer(point)) => {
            tree.handle_event_at(openide_ui::UiEvent::PointerMove(point), started);
            std::time::Duration::from_millis(16)
        }
        Some(ExportInteraction::Hover(id)) => {
            let rect = initial
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing hover target {id}"))
                .rect;
            tree.handle_event_at(
                openide_ui::UiEvent::PointerMove(openide_ui::Point::new(
                    rect.x + rect.width / 2.0,
                    rect.y + rect.height / 2.0,
                )),
                started,
            );
            tree.layout_at(size, started);
            std::time::Duration::from_millis(600)
        }
        Some(ExportInteraction::Focus(id)) => {
            assert!(tree.focus(id), "missing focus target {id}");
            std::time::Duration::from_millis(16)
        }
        Some(ExportInteraction::Scroll(id, delta_y)) => {
            let rect = initial
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap_or_else(|| panic!("missing scroll target {id}"))
                .rect;
            tree.handle_event_at(
                openide_ui::UiEvent::ScrollDirect {
                    position: openide_ui::Point::new(
                        rect.x + rect.width / 2.0,
                        rect.y + rect.height / 2.0,
                    ),
                    delta_y,
                },
                started,
            );
            std::time::Duration::from_millis(16)
        }
        None => std::time::Duration::ZERO,
    };
    let frame = tree.layout_at(size, started + delay);
    let directory = std::path::PathBuf::from("/tmp/openade-dock-review");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}.svg"));
    crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height).unwrap();
    let options = crate::settings_ui::visual_qa::native_svg_options();
    let svg =
        resvg::usvg::Tree::from_str(&std::fs::read_to_string(&path).unwrap(), &options).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
    resvg::render(
        &svg,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
    ctx.end_pass().textures_delta.clear();
    frame
}

fn rasterize_workspace_frame(frame: &openide_ui::Frame, size: Size) -> resvg::tiny_skia::Pixmap {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("workspace.svg");
    crate::settings_ui::visual_qa::write_native_frame_svg(frame, &path, size.width, size.height)
        .unwrap();
    let parsed = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &parsed,
        resvg::tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels
}

fn assert_pixel(pixels: &resvg::tiny_skia::Pixmap, point: openide_ui::Point, color: Color) {
    let pixel = pixels.pixel(point.x as u32, point.y as u32).unwrap();
    assert_eq!(
        (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
        (color.r, color.g, color.b, color.a),
        "pixel {point:?}"
    );
}

#[test]
fn shared_workspace_rounds_opaque_children_at_all_corners_and_keeps_the_split_square() {
    use openide_ui::{DrawCommand, Point};
    let top = Color::rgb(74, 92, 111);
    let bottom = Color::rgb(112, 83, 64);
    let size = Size::new(480.0, 320.0);
    let mut tree = UiTree::new(
        Element::panel(workspace_frame(Element::column(vec![
            Element::clickable("editor", Element::spacer())
                .background(top)
                .flex(1.0),
            Element::spacer()
                .background(theme::BORDER)
                .size(None, Some(1.0)),
            Element::clickable("terminal", Element::spacer())
                .background(bottom)
                .size(None, Some(96.0)),
        ])))
        .padding(16.0)
        .background(theme::WINDOW),
    );
    let frame = tree.layout(size);
    let outer = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::StrokeRect { rect, radius, .. } if *radius == WORKSPACE_RADIUS => {
                Some(*rect)
            }
            _ => None,
        })
        .unwrap();
    let pixels = rasterize_workspace_frame(&frame, size);
    for x in [outer.x + 1.0, outer.x + outer.width - 2.0] {
        for y in [outer.y + 1.0, outer.y + outer.height - 2.0] {
            assert_pixel(&pixels, Point::new(x, y), theme::WINDOW);
        }
    }
    assert_pixel(&pixels, Point::new(outer.x + 16.0, outer.y + 3.0), top);
    assert_pixel(
        &pixels,
        Point::new(outer.x + 16.0, outer.y + outer.height - 4.0),
        bottom,
    );
    let terminal = frame
        .regions
        .iter()
        .find(|region| region.id == "terminal")
        .unwrap()
        .rect;
    assert_pixel(&pixels, Point::new(outer.x + 2.0, terminal.y), bottom);
    assert_pixel(
        &pixels,
        Point::new(outer.x + outer.width - 3.0, terminal.y),
        bottom,
    );
    assert_eq!(
        frame.hit_test(Point::new(outer.x + 8.0, terminal.y + 8.0)),
        Some("terminal")
    );
    assert_eq!(
        frame.regions.len(),
        2,
        "rounding adds no focus or pointer targets"
    );
}

#[test]
fn workspace_corner_finish_stays_below_popovers_that_cross_the_outer_edge() {
    use openide_ui::{DrawCommand, Point, UiAction, UiEvent};
    let menu_color = Color::rgb(70, 110, 130);
    let popup = Element::popover(
        Element::button("menu", "…").size(Some(32.0), Some(32.0)),
        Element::clickable("menu_command", Element::spacer())
            .background(menu_color)
            .size(Some(280.0), Some(240.0)),
        "dismiss",
        PopoverPlacement::BottomEnd,
    );
    let root = Element::column(vec![
        Element::row(vec![
            workspace_frame(Element::column(vec![
                Element::row(vec![Element::spacer().flex(1.0), popup]),
                Element::spacer().flex(1.0),
            ]))
            .size(Some(280.0), Some(200.0)),
            Element::spacer().flex(1.0),
        ])
        .size(None, Some(200.0)),
        Element::spacer().flex(1.0),
    ])
    .padding(24.0)
    .background(theme::WINDOW);
    let size = Size::new(360.0, 320.0);
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    let frame = tree.layout(size);
    let (border_index, outer) = frame
        .commands
        .iter()
        .enumerate()
        .find_map(|(index, command)| match command {
            DrawCommand::StrokeRect { rect, radius, .. } if *radius == WORKSPACE_RADIUS => {
                Some((index, *rect))
            }
            _ => None,
        })
        .unwrap();
    let menu = frame
        .regions
        .iter()
        .find(|region| region.id == "menu_command")
        .unwrap()
        .rect;
    let corner = Point::new(outer.x + outer.width - 2.0, outer.y + outer.height - 2.0);
    assert!(menu.contains(corner));
    assert!(menu.y + menu.height > outer.y + outer.height);
    let menu_paint = frame
        .commands
        .iter()
        .position(|command| {
            matches!(command,
                DrawCommand::FillRect { color, .. } if *color == menu_color
            )
        })
        .unwrap();
    assert!(menu_paint > border_index);
    assert_pixel(&rasterize_workspace_frame(&frame, size), corner, menu_color);
    let point = Point::new(menu.x + 30.0, outer.y + outer.height + 16.0);
    assert_eq!(frame.hit_test(point), Some("menu_command"));
    tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(
        tree.handle_event(UiEvent::PointerUp(point)),
        vec![UiAction::Clicked {
            id: "menu_command".into()
        }]
    );
}

fn rounded_workspace_fixture() -> ViewState {
    let mut state = super::tests::empty_state();
    state.workspace = "/workspace/openADE".into();
    state.show_side_pane = true;
    state.side_tab = SideTab::FilePreview;
    state.dock_tabs = vec![DockTabView {
        id: 91,
        title: "alerts.md".into(),
        kind: SideTab::FilePreview,
        active: true,
        closable: true,
        full_path: Some("docs/alerts.md".into()),
    }];
    state.code_editor = Some(crate::code_editor_ui::CodeEditorView {
        tab_id: 91,
        path: "docs/alerts.md".into(),
        controller: Some(openide_ui::EditorController::new(
            openide_editor::TextBuffer::new("# Alertas\n\nLos cambios aparecen en el editor.\n")
                .unwrap(),
        )),
        loading: false,
        saving: false,
        error: None,
        notice: None,
        conflict: false,
        recovered: false,
        missing: false,
    });
    state.terminal_panel_height = 240.0;
    state.terminal_tabs = vec![DockTabView {
        id: 92,
        title: "Terminal".into(),
        kind: SideTab::Terminal,
        active: true,
        closable: true,
        full_path: Some(state.workspace.clone()),
    }];
    let lines = ["$ cargo check", "Finished dev profile", "$ "];
    state.terminal_snapshot = Some(crate::terminal::TerminalSnapshot {
        lines: lines.iter().map(|line| (*line).into()).collect(),
        styled_lines: lines
            .iter()
            .map(|line| {
                vec![crate::terminal::TerminalSpan {
                    text: (*line).into(),
                    style: crate::terminal::TerminalStyle::default(),
                }]
            })
            .collect(),
        cursor: (2, 2),
        cols: 90,
        rows: 12,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "bash".into(),
        alternate_screen: false,
        selection: None,
    });
    state
}

#[test]
fn real_workspace_corners_remain_rounded_with_terminal_hidden_open_and_maximized() {
    use openide_ui::{DrawCommand, Point};
    for sidebar_collapsed in [false, true] {
        for (terminal, maximized) in [(false, false), (true, false), (true, true)] {
            let mut state = rounded_workspace_fixture();
            state.sidebar_collapsed = sidebar_collapsed;
            state.show_terminal_panel = terminal;
            state.terminal_maximized = maximized;
            let size = Size::new(1280.0, 720.0);
            let frame =
                UiTree::new(build_window_view(&state, size.width, size.height)).layout(size);
            let outer = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::StrokeRect { rect, radius, .. }
                        if *radius == WORKSPACE_RADIUS && rect.width > 600.0 =>
                    {
                        Some(*rect)
                    }
                    _ => None,
                })
                .unwrap();
            let pixels = rasterize_workspace_frame(&frame, size);
            for x in [outer.x + 1.0, outer.x + outer.width - 2.0] {
                for y in [outer.y + 1.0, outer.y + outer.height - 2.0] {
                    assert_pixel(&pixels, Point::new(x, y), theme::WINDOW);
                }
            }
            if terminal {
                let y = outer.y + outer.height - terminal_panel_height(&state, size.height);
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, radius } if *color == theme::BORDER && *radius == 0.0
                        && (rect.y - y).abs() < 0.01 && rect.height == 1.0 && rect.width == outer.width
                )), "the editor/terminal seam stays straight");
            }
        }
    }
}

#[test]
fn workspace_sidebars_finish_all_corners_with_their_existing_radius() {
    use openide_ui::{DrawCommand, Point};
    let size = Size::new(1280.0, 720.0);
    for settings in [false, true] {
        let mut state = rounded_workspace_fixture();
        state.show_settings = settings;
        state.sidebar_collapsed = false;
        let frame = UiTree::new(build_window_view(&state, size.width, size.height)).layout(size);
        let (mask_index, outer, radius) = frame
            .commands
            .iter()
            .enumerate()
            .find_map(|(index, command)| match command {
                DrawCommand::CornerMask { rect, color, radii }
                    if rect.x == GAP && rect.width < 400.0 && rect.height > 500.0 =>
                {
                    assert_eq!(*color, theme::WINDOW);
                    assert_eq!(
                        *radii,
                        openide_ui::CornerRadii::same(if settings { 12.0 } else { 6.0 })
                    );
                    Some((index, *rect, radii.top_left))
                }
                _ => None,
            })
            .expect("the visible sidebar has its own passive corner finish");
        assert!(
            frame.commands[mask_index + 1..]
                .iter()
                .any(|command| matches!(command,
                    DrawCommand::StrokeRect { rect, color, radius: border_radius, .. }
                        if *rect == outer && *color == theme::BORDER && *border_radius == radius
                )),
            "the outline finishes above opaque sidebar content"
        );
        let pixels = rasterize_workspace_frame(&frame, size);
        for x in [outer.x, outer.x + outer.width - 1.0] {
            for y in [outer.y, outer.y + outer.height - 1.0] {
                assert_pixel(&pixels, Point::new(x, y), theme::WINDOW);
            }
        }
    }
}

pub(super) fn scrolling_dock_fixture(kind: SideTab) -> ViewState {
    let mut state = rounded_workspace_fixture();
    state.side_tab = kind;
    state.dock_tabs[0].kind = kind;
    state.dock_tabs[0].title = match kind {
        SideTab::Files => "Archivos",
        SideTab::Activity => "Actividad",
        SideTab::CodeReview => "Repositorio",
        SideTab::SessionChanges => "Cambios del chat",
        SideTab::Browser => "Navegador",
        _ => "alerts.md",
    }
    .into();
    state.git_review = Some(Ok(Arc::new(GitReview {
        branch: "main".into(),
        untracked: (0..200)
            .map(|index| format!("src/file-{index:03}.rs"))
            .collect(),
        ..Default::default()
    })));
    state.file_rows = (0..200)
        .map(|index| FileRowView {
            name: format!("file-{index:03}.rs"),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        })
        .collect();
    state.transcript = (0..30)
        .map(|index| TranscriptItem::Tool {
            id: format!("tool-{index}"),
            // Commands: a run of reads would fold into one line of the log.
            name: "run_command".into(),
            arguments: serde_json::json!({"command": format!("rustfmt src/file-{index:03}.rs")}),
            output: Some("Formateado".into()),
            is_error: false,
            activity: crate::transcript::ActivityStatus::Completed,
        })
        .collect();
    state.code_editor.as_mut().unwrap().controller = Some(openide_ui::EditorController::new(
        openide_editor::TextBuffer::new(&"linea de ejemplo\n".repeat(200)).unwrap(),
    ));
    state
}

#[test]
fn dock_headers_and_actions_have_no_blanket_fade_in_any_surface() {
    for kind in [
        SideTab::Files,
        SideTab::Activity,
        SideTab::CodeReview,
        SideTab::SessionChanges,
        SideTab::Browser,
        SideTab::FilePreview,
    ] {
        let state = scrolling_dock_fixture(kind);
        let frame = UiTree::new(side_pane(&state, 380.0)).layout(Size::new(380.0, 600.0));
        assert!(
            !frame.commands.iter().any(|command| matches!(
                command,
                openide_ui::DrawCommand::FillVerticalGradient { .. }
            )),
            "unscrolled {kind:?} should be fully legible"
        );
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "dock_tabs_menu"));
    }
}

#[test]
fn dock_scroll_fades_keep_native_header_pixels_and_action_geometry_unchanged() {
    use openide_ui::{DrawCommand, Point, UiAction, UiEvent};
    for (kind, scroll_id, action) in [
        (SideTab::CodeReview, "git_change_files", "review_refresh"),
        (SideTab::Files, "file_tree", "files_search"),
        (SideTab::Activity, "dock_activity_tools", "dock_tabs_menu"),
        (SideTab::FilePreview, "code_editor:91", "editor_find"),
    ] {
        let state = scrolling_dock_fixture(kind);
        let mut tree = UiTree::new(side_pane(&state, 380.0));
        tree.set_reduced_motion(true);
        let size = Size::new(380.0, 600.0);
        let first = tree.layout(size);
        let body = first
            .regions
            .iter()
            .find(|region| region.id == scroll_id)
            .unwrap()
            .rect;
        let header_action = first
            .regions
            .iter()
            .find(|region| region.id == action)
            .unwrap()
            .rect;
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
            delta_y: -120.0,
        });
        let scrolled = tree.layout(size);
        let fades: Vec<_> = scrolled
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillVerticalGradient { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(fades.len(), 1, "{kind:?}");
        assert!(fades[0].y >= body.y && fades[0].y + fades[0].height <= body.y + body.height);
        assert!(fades[0].intersection(header_action).is_none());
        assert_eq!(
            scrolled
                .regions
                .iter()
                .find(|region| region.id == action)
                .unwrap()
                .rect,
            header_action
        );
        let before = rasterize_workspace_frame(&first, size);
        let after = rasterize_workspace_frame(&scrolled, size);
        for y in 0..body.y.floor() as u32 {
            for x in 0..size.width as u32 {
                assert_eq!(
                    before.pixel(x, y),
                    after.pixel(x, y),
                    "{kind:?} header pixel {x},{y}"
                );
            }
        }
        let point = Point::new(
            header_action.x + header_action.width / 2.0,
            header_action.y + header_action.height / 2.0,
        );
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id: action.into() }]
        );
    }
}

#[test]
#[ignore = "opt-in native unfaded dock headers and scrolling content review"]
fn export_dock_scroll_fade_visual_review() {
    for (kind, scroll_id, name) in [
        (SideTab::CodeReview, "git_change_files", "git"),
        (SideTab::Files, "file_tree", "files"),
        (SideTab::Activity, "dock_activity_tools", "activity"),
        (SideTab::FilePreview, "code_editor:91", "editor"),
    ] {
        let state = scrolling_dock_fixture(kind);
        export(
            side_pane(&state, 380.0),
            380.0,
            600.0,
            &format!("dock-fade-{name}-top"),
        );
        export_with_interaction(
            side_pane(&state, 380.0),
            380.0,
            600.0,
            &format!("dock-fade-{name}-scrolled"),
            Some(ExportInteraction::Scroll(scroll_id, -120.0)),
        );
    }
}

#[test]
#[ignore = "opt-in native four-corner workspace, split terminal and open-menu review"]
fn export_rounded_workspace_visual_review() {
    for (terminal, maximized, menu, name) in [
        (false, false, false, "terminal-hidden"),
        (true, false, false, "terminal-open"),
        (true, true, false, "terminal-maximized"),
        (true, false, true, "editor-menu"),
    ] {
        let mut state = rounded_workspace_fixture();
        state.sidebar_collapsed = true;
        state.show_terminal_panel = terminal;
        state.terminal_maximized = maximized;
        state.editor_workbench.menu_open = menu;
        export(
            build_window_view(&state, 1200.0, 720.0),
            1200.0,
            720.0,
            &format!("rounded-workspace-{name}"),
        );
    }
}

#[test]
#[ignore = "opt-in panel layout inspection with native fonts"]
fn export_dock_visual_review() {
    let mut state = super::tests::empty_state();
    state.workspace = "/home/nihuel/projects/personal/openADE".into();
    state.selected_session = None;
    state.prompt.clear();
    state.native_full_access = true;
    state.show_side_pane = true;
    state.side_tab = SideTab::CodeReview;
    state.dock_tabs = [
        ("Archivos", SideTab::Files),
        ("Terminal", SideTab::Terminal),
        ("Repositorio", SideTab::CodeReview),
        ("Actividad", SideTab::Activity),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (title, kind))| DockTabView {
        full_path: None,
        id: index as u64 + 1,
        title: title.into(),
        active: kind == SideTab::CodeReview,
        kind,
        closable: true,
    })
    .collect();
    let review = crate::git_review::GitReview {
        branch: "codex/ui-component-quality".into(),
        tracking: Some(crate::git_review::GitTracking {
            upstream: "origin/codex/ui-component-quality".into(),
            ahead: 2,
            behind: 1,
        }),
        files: 68,
        additions: 2014,
        deletions: 146,
        sections: vec![crate::git_review::ReviewSection {
            title: "CAMBIOS SIN PREPARAR".into(),
            files: (0..68)
                .map(|index| ReviewFile {
                    path: match index {
                        0 => "apps/agents-window/src/dock_chrome.rs".into(),
                        1 => "apps/agents-window/src/terminal.rs".into(),
                        2 => "crates/openide-ui/src/code_diff.rs".into(),
                        _ => format!("src/components/component_{index}.rs"),
                    },
                    status: "Modificado".into(),
                    additions: 32 + index,
                    deletions: 4,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }],
        untracked: vec!["docs/right-dock.md".into()],
        github: crate::github::GithubStatus::Ready(crate::github::GithubOverview {
            repository: crate::github::GithubRepository {
                owner: "OpenIDE".into(),
                name: "example-workspace".into(),
                remote_name: "origin".into(),
                head_owner: "OpenIDE".into(),
            },
            branch: "codex/ui-component-quality".into(),
            pulls: vec![],
            notice: None,
        }),
        ..Default::default()
    };
    state.git_review = Some(Ok(review.into()));
    for width in [320.0, 440.0, 620.0] {
        export(
            side_pane(&state, width),
            width,
            760.0,
            &format!("git-{width}"),
        );
    }
    export_with_interaction(
        side_pane(&state, 440.0),
        440.0,
        760.0,
        "git-stage-tooltip",
        Some(ExportInteraction::Hover("review_stage:0:0")),
    );
    export_with_interaction(
        side_pane(&state, 440.0),
        440.0,
        760.0,
        "git-github-footer-hover",
        Some(ExportInteraction::Hover("github_repo_open")),
    );
    export_with_interaction(
        side_pane(&state, 620.0),
        620.0,
        760.0,
        "dock-close-hover",
        Some(ExportInteraction::Hover("dock_close:3")),
    );
    let original_review = state.git_review.take();
    let mut prepared_review = original_review
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .clone();
    prepared_review.sections[0].title = "CAMBIOS PREPARADOS".into();
    state.git_review = Some(Ok(prepared_review.into()));
    export_with_interaction(
        side_pane(&state, 440.0),
        440.0,
        760.0,
        "git-unstage-tooltip",
        Some(ExportInteraction::Hover("review_unstage:0:0")),
    );
    state.git_review = original_review;
    state.right_dock_width = Some(440.0);
    export(
        build_window_view(&state, 1440.0, 900.0),
        1440.0,
        900.0,
        "workspace-git",
    );
    state.show_dock_tabs_menu = true;
    export(side_pane(&state, 440.0), 440.0, 760.0, "tabs-menu");
    state.show_dock_tabs_menu = false;
    state.side_tab = SideTab::Activity;
    for tab in &mut state.dock_tabs {
        tab.active = tab.kind == state.side_tab;
    }
    state.transcript = (0..100)
        .map(|index| TranscriptItem::Tool {
            id: index.to_string(),
            name: if index % 2 == 0 {
                "read_file".into()
            } else {
                "run_command".into()
            },
            arguments: serde_json::json!({"path": "src/main.rs"}),
            output: Some("Compiling openide\nFinished dev profile\nAll tests passed".into()),
            is_error: false,
            activity: crate::transcript::ActivityStatus::Completed,
        })
        .collect();
    export(side_pane(&state, 440.0), 440.0, 760.0, "activity");
    state.side_tab = SideTab::Terminal;
    for tab in &mut state.dock_tabs {
        tab.active = tab.kind == state.side_tab;
    }
    state.terminal_snapshot = Some(crate::terminal::TerminalSnapshot {
        lines: vec![
            "nihuel@nixos ~/projects/personal/openADE".into(),
            "$ cargo test -p agents-window".into(),
            "Finished test profile in 3.1s".into(),
            "test result: ok. 1200 passed".into(),
            "$ ".into(),
        ],
        styled_lines: [
            "nihuel@nixos ~/projects/personal/openADE",
            "$ cargo test -p agents-window",
            "Finished test profile in 3.1s",
            "test result: ok. 1200 passed",
            "$ ",
        ]
        .into_iter()
        .map(|text| {
            vec![crate::terminal::TerminalSpan {
                text: text.into(),
                style: crate::terminal::TerminalStyle::default(),
            }]
        })
        .collect(),
        cursor: (4, 2),
        cols: 56,
        rows: 35,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "bash".into(),
        alternate_screen: false,
        selection: None,
    });
    export(side_pane(&state, 440.0), 440.0, 760.0, "terminal");
    export_with_interaction(
        side_pane(&state, 440.0),
        440.0,
        760.0,
        "terminal-clear-hover",
        Some(ExportInteraction::Hover("terminal_clear")),
    );
    export_with_interaction(
        side_pane(&state, 440.0),
        440.0,
        760.0,
        "terminal-focused",
        Some(ExportInteraction::Focus("terminal_screen")),
    );
}

fn environment_visual_fixture() -> (tempfile::TempDir, ViewState) {
    let directory = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = crate::child_process::command_no_window("git")
            .args([
                "-c",
                "user.name=OpenIDE visual fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(
        directory.path().join("README.md"),
        "Environment visual fixture\n",
    )
    .unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "Initial fixture"]);
    git(&["switch", "-q", "-c", "codex/update-controls"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/openade.git",
    ]);
    let mut repository = crate::environment_git::load_snapshot(directory.path()).unwrap();
    repository.staged = 3;
    repository.unstaged = 4;
    repository.untracked = 2;
    repository.ahead = 2;
    repository.upstream = Some("origin/codex/update-controls".into());
    repository.worktrees = vec![
        crate::environment_git::WorktreeInfo {
            path: "/home/nihuel/projects/personal/openADE".into(),
            branch: Some("codex/update-controls".into()),
            current: true,
        },
        crate::environment_git::WorktreeInfo {
            path: "/home/nihuel/projects/worktrees/openADE-preview".into(),
            branch: Some("feature/environment-card".into()),
            current: false,
        },
    ];
    let mut state = super::tests::empty_state();
    state.workspace = "/home/nihuel/projects/personal/openADE".into();
    state.project_branch = Some(repository.branches.label.clone());
    state.composer_branches.snapshot = Some(repository.branches.clone());
    state.environment.open = true;
    state.environment.changes = crate::environment_ui::SessionChangesSummaryView {
        available: true,
        files: 5,
        additions: 312,
        deletions: 26,
        ..Default::default()
    };
    state.environment.workflow = crate::environment_git_ui::EnvironmentGitUiState {
        snapshot: Some(repository),
        commit_message: "Mejora la tarjeta de entorno y el panel derecho".into(),
        push_remote: "origin".into(),
        push_branch: "codex/update-controls".into(),
        compare_reference: "refs/heads/main".into(),
        ..Default::default()
    };
    state.git_review = Some(Ok(crate::git_review::GitReview {
        files: 68,
        additions: 25813,
        deletions: 8475,
        ..Default::default()
    }
    .into()));
    state.transcript = vec![
        TranscriptItem::Tool {
            id: "environment-read".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "apps/agents-window/src/environment_ui.rs"}),
            output: Some("//! Conversation environment".into()),
            is_error: false,
            activity: crate::transcript::ActivityStatus::Completed,
        },
        TranscriptItem::WebCitation {
            title: "Codex · Entorno local".into(),
            url: "https://learn.chatgpt.com/docs/environments/local-environment".into(),
        },
    ];
    (directory, state)
}

/// The panel as its host lays it out: at its own height, with room left over.
fn environment_card(state: &ViewState, width: f32, height: f32) -> Element {
    Element::column(vec![
        crate::environment_ui::panel(state, width, height),
        Element::spacer().flex(1.0),
    ])
}

#[test]
#[ignore = "opt-in environment card inspection with native fonts"]
fn export_environment_visual_review() {
    use crate::environment_git_ui::EnvironmentGitPanel;
    use ExportInteraction::Hover;
    let (_directory, mut state) = environment_visual_fixture();
    state.project_actions.workspace = Some(state.workspace.clone().into());
    state.project_actions.actions = vec![crate::project_actions::ProjectAction {
        id: "check".into(),
        label: "Verificar proyecto".into(),
        command: "cargo check --workspace".into(),
    }];
    state.project_actions.presets = vec![crate::project_actions::ProjectAction {
        id: "cargo-test".into(),
        label: "Probar".into(),
        command: "cargo test --workspace".into(),
    }];
    let figures = state.git_review.clone();
    for width in [280.0, 380.0] {
        let mut shot = |state: &ViewState, height: f32, name: &str, hover: Option<&'static str>| {
            export_with_interaction(
                environment_card(state, width, height),
                width,
                height,
                &format!("environment-{name}-{width}"),
                hover.map(Hover),
            );
        };
        shot(&state, 344.0, "overview", None);
        shot(&state, 344.0, "row-hover", Some("environment_branch_toggle"));
        shot(&state, 344.0, "commit-hover", Some("environment_git_open_commit"));
        for (name, id) in [
            ("row-focus", "environment_local_toggle"),
            ("commit-focus", "environment_git_open_commit"),
        ] {
            export_with_interaction(
                environment_card(&state, width, 344.0),
                width,
                344.0,
                &format!("environment-{name}-{width}"),
                Some(ExportInteraction::Focus(id)),
            );
        }
        // The changes of a working day, which fit beside the commit at any width.
        state.git_review = Some(Ok(crate::git_review::GitReview {
            files: 8,
            additions: 248,
            deletions: 31,
            ..Default::default()
        }
        .into()));
        shot(&state, 344.0, "overview-day", None);
        shot(&state, 344.0, "changes-hover", Some("dock_open:review"));
        state.environment.details_expanded = true;
        shot(&state, 760.0, "details", None);
        shot(&state, 760.0, "details-hover", Some("environment_source_file:0"));
        // Shorter than its rows: they scroll between the header and the foot.
        shot(&state, 344.0, "details-short", None);
        state.environment.workflow.panel = EnvironmentGitPanel::Actions;
        shot(&state, 900.0, "sections", None);
        state.environment.details_expanded = false;
        shot(&state, 620.0, "actions", None);
        shot(&state, 620.0, "actions-hover", Some("project_action_run:check"));
        state.project_actions.begin_new();
        shot(&state, 620.0, "action-editor", None);
        state.project_actions.cancel_edit();
        let saved = std::mem::take(&mut state.project_actions.actions);
        shot(&state, 620.0, "actions-empty", None);
        state.project_actions.actions = saved;
        state.environment.workflow.panel = EnvironmentGitPanel::Overview;
        state.environment.menu = crate::environment_ui::EnvironmentMenu::Local;
        shot(&state, 520.0, "local", None);
        state.environment.menu = crate::environment_ui::EnvironmentMenu::None;
        state.running = true;
        shot(&state, 344.0, "locked", None);
        state.running = false;
        state.git_review = None;
        state.environment.workflow.loading = true;
        shot(&state, 344.0, "loading", None);
        state.environment.workflow.loading = false;
        state.git_review = Some(Err(crate::git_review::NOT_A_REPOSITORY.into()));
        state.environment.workflow.error = Some(crate::git_review::NOT_A_REPOSITORY.into());
        shot(&state, 344.0, "no-repository", None);
        state.environment.workflow.error = None;
        state.git_review = figures.clone();
        state.environment.workflow.panel = EnvironmentGitPanel::Commit;
        shot(&state, 620.0, "commit", None);
        state.environment.workflow.panel = EnvironmentGitPanel::Compare;
        state.environment.workflow.comparison = Some(crate::environment_git::BranchComparison {
            reference: "refs/heads/main".into(),
            ahead: 2,
            behind: 0,
            files: 12,
            additions: 380,
            deletions: 44,
        });
        state.environment.workflow.compare_url =
            Some("https://github.com/example/openade/compare/main...codex/update-controls".into());
        shot(&state, 620.0, "compare", None);
        state.environment.workflow.panel = EnvironmentGitPanel::Overview;
    }
    state.show_side_pane = false;
    export(
        build_window_view(&state, 1440.0, 900.0),
        1440.0,
        900.0,
        "workspace-environment",
    );
    state.selected_session = Some("environment-layout".into());
    state.selected_session_title = Some("Revisar la interfaz del proyecto".into());
    state.transcript = vec![
        TranscriptItem::User { content: "Revisá los cambios y explicame cómo funciona el entorno.".into(), images: Vec::new() },
        TranscriptItem::Assistant { content: "El entorno ocupa su propia columna. El historial y el cuadro de mensaje se centran en el espacio disponible y siguen siendo accesibles mientras elegís una rama, revisás los cambios o preparás un commit.\n\nLa carpeta local contiene el repositorio, sus worktrees y las acciones del proyecto.".into(), streaming: false },
    ];
    state.transcript = (0..48).flat_map(|index| [
        TranscriptItem::User { content: format!("Paso {}: revisá los cambios del proyecto y explicá el resultado.", index + 1), images: Vec::new() },
        TranscriptItem::Assistant { content: format!("Revisión {} completada. El historial y el cuadro de mensaje conservan su propio espacio mientras el entorno muestra el repositorio y la rama actual.\n\nLa terminal y los archivos siguen disponibles en el panel de herramientas.", index + 1), streaming: false },
    ]).collect();
    for (width, height) in [(1600.0, 900.0), (900.0, 700.0), (520.0, 700.0)] {
        state.sidebar_collapsed = width < 1200.0;
        state.environment.open = true;
        export(
            build_window_view(&state, width, height),
            width,
            height,
            &format!("environment-layout-open-{width}"),
        );
        state.environment.open = false;
        export(
            build_window_view(&state, width, height),
            width,
            height,
            &format!("environment-layout-closed-{width}"),
        );
        state.environment.open = true;
        state.environment.menu = crate::environment_ui::EnvironmentMenu::Local;
        export(
            build_window_view(&state, width, height),
            width,
            height,
            &format!("environment-layout-local-{width}"),
        );
        state.environment.menu = crate::environment_ui::EnvironmentMenu::Branch;
        state.composer_branches.open = true;
        export(
            build_window_view(&state, width, height),
            width,
            height,
            &format!("environment-layout-branch-{width}"),
        );
        state.environment.menu = crate::environment_ui::EnvironmentMenu::None;
        state.composer_branches.open = false;
        state.environment_presentation_progress = Some(0.5);
        export(
            build_window_view(&state, width, height),
            width,
            height,
            &format!("environment-layout-half-{width}"),
        );
        state.environment_presentation_progress = None;
    }
    state.selected_session = None;
    state.transcript.clear();
    state.environment.open = true;
    state.sidebar_collapsed = false;
    export(
        build_window_view(&state, 1600.0, 900.0),
        1600.0,
        900.0,
        "environment-layout-home-open",
    );
    state.environment.open = false;
    export(
        build_window_view(&state, 1600.0, 900.0),
        1600.0,
        900.0,
        "environment-layout-home-closed",
    );
}

#[test]
#[ignore = "opt-in export of the sidebar's organization selector"]
fn export_session_organization_visual_review() {
    for (name, organization) in [
        ("group", SessionOrganization::Group),
        ("project", SessionOrganization::Project),
        ("recent", SessionOrganization::Recent),
    ] {
        let mut state = super::tests::empty_state();
        state.session_organization = organization;
        export_with_pointer(
            sidebar(&state, 264.0),
            264.0,
            200.0,
            &format!("sidebar-organization-{name}"),
            None,
        );
    }
}

#[test]
#[ignore = "opt-in session rows inspection with native fonts"]
fn export_session_rows_visual_review() {
    let mut state = super::tests::empty_state();
    state.selected_session = Some("running".into());
    state.running = true;
    let sessions = [
        (
            "idle",
            "hola de que trata mi proyecto? investigá la terminal y el entorno",
            false,
            false,
        ),
        (
            "running",
            "Revisando la interfaz y el comportamiento del proyecto",
            false,
            false,
        ),
        (
            "unread",
            "Análisis terminado · cambios pendientes de revisar",
            false,
            true,
        ),
        (
            "pinned",
            "Integración del panel derecho de OpenIDE",
            true,
            false,
        ),
        ("short", "Notas", false, false),
    ]
    .into_iter()
    .map(|(id, title, pinned, unread)| SessionView {
        id: id.into(),
        title: title.into(),
        project: "/project".into(),
        backend: "native".into(),
        pinned,
        unread,
        archived: false,
        native_automation: None,
    })
    .collect::<Vec<_>>();
    for width in [176.0, 264.0] {
        for (name, pointer) in [
            ("idle", None),
            ("hover", Some(openide_ui::Point::new(60.0, 25.0))),
        ] {
            let root = Element::column(
                sessions
                    .iter()
                    .map(|session| session_row(&state, session, width))
                    .collect(),
            )
            .gap(4.0)
            .padding(9.0)
            .background(theme::SIDEBAR)
            .size(Some(width), Some(194.0));
            export_with_pointer(
                root,
                width,
                194.0,
                &format!("sidebar-sessions-{width}-{name}"),
                pointer,
            );
        }
    }
}

/// The c04 pieces no other export reaches, each from its production builder:
/// the overview's closed tabs, the full-response button, the way from the
/// chat's changes to the repository's, and the side terminal's toolbar.
#[test]
#[ignore = "opt-in export of the chrome converged on limn profiles"]
fn export_converged_chrome_visual_review() {
    use ExportInteraction::Hover;
    let mut state = super::tests::empty_state();
    state.dock_recently_closed = vec![
        DockRecentlyClosedView {
            id: 9,
            title: "src/terminal_ui.rs".into(),
            kind: SideTab::FilePreview,
            restorable: true,
        },
        DockRecentlyClosedView {
            id: 10,
            title: "Sesión anterior".into(),
            kind: SideTab::Context,
            restorable: false,
        },
    ];
    for (name, hover) in [("idle", None), ("hover", Some(Hover("dock_reopen:9")))] {
        export_with_interaction(
            dock_overview::content(&state, 360.0).padding(12.0),
            360.0,
            360.0,
            &format!("converged-overview-reopen-{name}"),
            hover,
        );
    }

    for (name, hover) in [
        ("idle", None),
        ("hover", Some(Hover("conversation_source:draft:0:0:open"))),
    ] {
        export_with_interaction(
            crate::conversation_source_ui::open_button(&state, 0).padding(12.0),
            260.0,
            56.0,
            &format!("converged-source-open-{name}"),
            hover,
        );
    }

    state.session_changes.result = Some(Ok(std::sync::Arc::new(
        crate::session_changes::SessionChanges {
            session_id: "session-1".into(),
            workspace: "/project".into(),
            baseline: None,
            availability: crate::session_changes::ChangesAvailability::Unavailable {
                reason: "La conversación empezó antes de registrar una base.".into(),
            },
            totals: Default::default(),
            files: Vec::new(),
            coverage_notes: Vec::new(),
        },
    )));
    for (name, hover) in [("idle", None), ("hover", Some(Hover("dock_open:review")))] {
        export_with_interaction(
            crate::session_changes_ui::build(&state, 360.0),
            360.0,
            180.0,
            &format!("converged-session-changes-unavailable-{name}"),
            hover,
        );
    }

    let mut snapshot = crate::terminal::TerminalSnapshot {
        lines: vec!["$ cargo test".into(), "test result: ok".into(), "$ ".into()],
        styled_lines: Vec::new(),
        cursor: (2, 2),
        cols: 80,
        rows: 8,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "fish".into(),
        alternate_screen: false,
        selection: None,
    };
    for (name, running, scrollback, hover) in [
        ("live", true, 0, None),
        ("copy-hover", true, 0, Some(Hover("terminal_copy"))),
        ("history", true, 4, Some(Hover("terminal_bottom"))),
        ("exited", false, 0, Some(Hover("terminal_restart"))),
    ] {
        snapshot.running = running;
        snapshot.exit_code = (!running).then_some(0);
        snapshot.scrollback_position = scrollback;
        state.terminal_snapshot = Some(snapshot.clone());
        export_with_interaction(
            crate::terminal_ui::build(&state, 420.0),
            420.0,
            260.0,
            &format!("converged-side-terminal-{name}"),
            hover,
        );
    }
}

/// The c01 pieces no other export reaches: each is the production builder,
/// with the pointer over the control whose hint or hover the section moved.
#[test]
#[ignore = "opt-in export of the controls converged on limn profiles"]
fn export_converged_controls_visual_review() {
    use ExportInteraction::Hover;
    let mut state = super::tests::empty_state();
    state.dock_tabs = ["Entorno", "src/view/dock_overview.rs", "Terminal"]
        .into_iter()
        .zip([SideTab::Context, SideTab::FilePreview, SideTab::Terminal])
        .enumerate()
        .map(|(index, (title, kind))| DockTabView {
            id: index as u64 + 1,
            full_path: None,
            title: title.into(),
            active: index == 1,
            kind,
            closable: true,
        })
        .collect();
    for (name, hover) in [("idle", None), ("close-all-hover", Some(Hover("dock_close_all")))] {
        export_with_interaction(
            dock_overview::content(&state, 360.0).padding(12.0),
            360.0,
            520.0,
            &format!("converged-dock-overview-{name}"),
            hover,
        );
    }

    state.selected_session = Some("session-1".into());
    state.selected_backend = "native".into();
    state.session_approval_grants = ["run_command", "write_file"]
        .into_iter()
        .map(|tool| SessionApprovalGrantView {
            tool_name: tool.into(),
            key_digest: tool.repeat(8),
        })
        .collect();
    let revoke = format!("session_approval_revoke:{}", "run_command".repeat(8)).leak();
    // The session's permissions are listed by a disclosure: closed, then open.
    export(activity_content(&state), 360.0, 360.0, "session-grants-closed");
    state.activity.grants_open = true;
    for (name, hover) in [("idle", None), ("revoke-hover", Some(Hover(revoke)))] {
        export_with_interaction(
            activity_content(&state),
            360.0,
            360.0,
            &format!("converged-session-grants-{name}"),
            hover,
        );
    }
    state.activity.grants_open = false;

    let preset = crate::project_actions::ProjectAction {
        id: "dev".into(),
        label: "Ejecutar proyecto".into(),
        command: "npm run dev".into(),
    };
    let suggested = crate::project_actions::ProjectActionsState {
        presets: vec![preset],
        ..Default::default()
    };
    let saving = crate::project_actions::ProjectActionsState {
        saving: true,
        ..suggested.clone()
    };
    let mut editing = suggested.clone();
    editing.begin_new();
    export(
        Element::column(
            [&suggested, &saving, &editing]
                .into_iter()
                .map(|actions| crate::project_actions_ui::build(actions, 296.0))
                .collect(),
        )
        .gap(20.0)
        .padding(12.0)
        // The fill of the block that lists them in the environment card.
        .background(crate::environment_ui::block_palette().surface),
        320.0,
        560.0,
        "converged-project-actions",
    );

    for (name, loading, hover) in [
        ("idle", false, None),
        ("refresh-hover", false, Some(Hover("session_changes_refresh"))),
        ("loading", true, None),
    ] {
        state.session_changes.loading = loading;
        export_with_interaction(
            crate::session_changes_ui::build(&state, 360.0).padding(12.0),
            360.0,
            140.0,
            &format!("converged-session-changes-{name}"),
            hover,
        );
    }

    state.native_hooks.hooks = vec![openide_harness::CommandHookConfig {
        id: "formatear-al-escribir".into(),
        phase: openide_harness::HookPhase::PostToolUse,
        tool_name: "write_file".into(),
        executable: "/usr/bin/rustfmt".into(),
        args: Vec::new(),
        cwd: "/project".into(),
        timeout_ms: 3_000,
        max_output_bytes: 1_024,
    }];
    state.native_exec_rules.rules = vec![openide_harness::ExecRule::new(
        vec![
            openide_harness::ExecPatternToken::Literal("git".into()),
            openide_harness::ExecPatternToken::Literal("push".into()),
        ],
        openide_harness::ExecDecision::Ask,
        None,
    )
    .unwrap()];
    state.show_settings = true;
    state.native_provider_id = "openai".into();
    state.model = "gpt-4.1".into();
    for width in [360.0, 1300.0] {
        for page in [SettingsPage::Hooks, SettingsPage::Commands] {
            state.settings_page = page;
            export(
                crate::settings_ui::build_settings_page(&state, page, width, true),
                width,
                900.0,
                &format!("converged-settings-{page:?}-{width}"),
            );
        }
        // Chat Completions: the parameter group and the save group together.
        export(
            crate::settings_ui::native_output_settings_for_test(&state, width < 640.0)
                .padding(16.0),
            width,
            420.0,
            &format!("converged-settings-output-limit-{width}"),
        );
    }

    let mut state = super::tests::empty_state();
    state.sessions = [("uno", "Revisar el panel derecho"), ("dos", "Notas")]
        .into_iter()
        .map(|(id, title)| SessionView {
            id: id.into(),
            title: title.into(),
            project: "/project".into(),
            backend: "native".into(),
            pinned: false,
            unread: false,
            archived: false,
            native_automation: None,
        })
        .collect();
    for (name, hover) in [
        ("projects-expand-hover", "session_expand_all"),
        ("projects-session-menu-hover", "session_menu:uno"),
    ] {
        export_with_interaction(
            sidebar_for_height(&state, 264.0, 420.0),
            264.0,
            420.0,
            &format!("converged-sidebar-{name}"),
            Some(Hover(hover)),
        );
    }
    state.session_organization = SessionOrganization::Group;
    state.session_groups.groups.push(crate::session_groups::SessionGroup {
        id: "g1".into(),
        title: "Interfaz".into(),
        collapsed: false,
    });
    state.session_groups.members.insert("uno".into(), "g1".into());
    export_with_interaction(
        sidebar_for_height(&state, 264.0, 420.0),
        264.0,
        420.0,
        "converged-sidebar-groups-menu-hover",
        Some(Hover("session_group_menu:g1")),
    );
    state.session_sort_menu_open = true;
    // Tall enough for the whole menu: a 420 px frame would scroll it.
    export(
        sidebar_for_height(&state, 264.0, 480.0),
        264.0,
        480.0,
        "sidebar-sort-menu-groups",
    );
    state.show_archived = true;
    export(
        sidebar_for_height(&state, 264.0, 420.0),
        264.0,
        420.0,
        "sidebar-sort-menu-archive",
    );
    state.show_archived = false;
    state.session_sort_menu_open = false;
    state.group_create_open = true;
    export_with_interaction(
        sidebar_for_height(&state, 264.0, 420.0),
        264.0,
        420.0,
        "sidebar-group-create-blank",
        Some(ExportInteraction::Focus("group_create_draft")),
    );
    state.group_create_draft = "Interfaz nueva".into();
    export(
        sidebar_for_height(&state, 264.0, 420.0),
        264.0,
        420.0,
        "sidebar-group-create-named",
    );
    state.group_create_draft.clear();
    state.session_groups.groups.clear();
    state.sessions.clear();
    export(
        sidebar_for_height(&state, 264.0, 420.0),
        264.0,
        420.0,
        "sidebar-group-create-empty",
    );
}

#[test]
#[ignore = "opt-in native breadcrumbs and horizontal dock strip"]
fn export_breadcrumb_and_tab_strip_visual_review() {
    use openide_ui::{Breadcrumb, BreadcrumbItem};
    let mut state = super::tests::empty_state();
    state.dock_tabs = [
        ("Archivos", SideTab::Files),
        ("Repositorio", SideTab::CodeReview),
        ("Terminal", SideTab::Terminal),
        ("Actividad", SideTab::Activity),
        ("settings_ui.rs", SideTab::FilePreview),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (title, kind))| DockTabView {
        full_path: None,
        id: index as u64 + 1,
        title: title.into(),
        active: index == 2,
        kind,
        closable: true,
    })
    .collect();
    for width in [320.0, 440.0, 620.0] {
        let breadcrumb = Breadcrumb::new(
            "git-path",
            vec![
                BreadcrumbItem::new("Cambios").action("review_detail_back"),
                BreadcrumbItem::new("apps"),
                BreadcrumbItem::new("agents-window"),
                BreadcrumbItem::new("src"),
                BreadcrumbItem::new("environment_git_ui.rs")
                    .tooltip("apps/agents-window/src/environment_git_ui.rs"),
            ],
            theme::component_palette(),
        )
        .width(width - 24.0)
        .render();
        let terminal = Breadcrumb::new(
            "terminal-path",
            vec![
                BreadcrumbItem::new("bash").icon(Icon::Terminal),
                BreadcrumbItem::new("openADE").tooltip("/home/nihuel/projects/personal/openADE"),
            ],
            theme::component_palette(),
        )
        .width(width - 24.0)
        .render();
        let root = Element::column(vec![
            super::dock_chrome::header(&state, width),
            Element::panel(breadcrumb).padding(12.0),
            Element::panel(terminal).padding(12.0),
        ])
        .background(theme::BACKGROUND)
        .size(Some(width), Some(160.0));
        export(root, width, 160.0, &format!("breadcrumb-tabs-{width}"));
    }
}

#[test]
#[ignore = "opt-in running composer permission tooltip with native fonts"]
fn export_running_permission_tooltip_visual_review() {
    let mut state = super::tests::empty_state();
    state.workspace = "/home/nihuel/projects/personal/openADE".into();
    state.selected_session = Some("visual-running".into());
    state.selected_session_title = Some("hola de que trata mi proyecto?".into());
    state.running = true;
    state.native_full_access = true;
    state.show_side_pane = false;
    state.prompt.clear();
    for width in [900.0, 1440.0] {
        export_with_interaction(
            build_window_view(&state, width, 900.0),
            width,
            900.0,
            &format!("permissions-tooltip-running-{width}"),
            Some(ExportInteraction::Hover("composer:permissions")),
        );
    }
}

/// State-only fixture: this export never dispatches the destructive action or
/// opens a session store. Its project path belongs to an isolated TempDir.
#[test]
#[ignore = "opt-in conversation menu and delete confirmation with native fonts"]
fn export_session_delete_visual_review() {
    use openide_ui::{DrawCommand, Frame, HitKind, Rect};
    let directory = tempfile::tempdir().unwrap();
    let project = directory
        .path()
        .join("openADE")
        .to_string_lossy()
        .into_owned();
    let mut state = super::tests::empty_state();
    state.workspace = project.clone();
    state.show_side_pane = false;
    state.selected_session = Some("visual-delete".into());
    state.selected_session_title = Some("Revisar el panel del proyecto".into());
    state.sessions = vec![SessionView {
        id: "visual-delete".into(),
        title: "Revisar el panel del proyecto".into(),
        project,
        backend: "native".into(),
        pinned: false,
        unread: false,
        archived: false,
        native_automation: None,
    }];
    let rect = |frame: &Frame, id: &str| -> Rect {
        frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing visual target {id}"))
            .rect
    };
    let inside = |outer: Rect, inner: Rect, inset: f32| {
        assert!(inner.x >= outer.x + inset - 0.5);
        assert!(inner.y >= outer.y + inset - 0.5);
        assert!(inner.x + inner.width <= outer.x + outer.width - inset + 0.5);
        assert!(inner.y + inner.height <= outer.y + outer.height - inset + 0.5);
    };
    state.session_menu_id = Some("visual-delete".into());
    for width in [900.0, 1280.0] {
        let frame = export_with_interaction(
            build_window_view(&state, width, 700.0),
            width,
            700.0,
            &format!("session-delete-menu-{width}"),
            Some(ExportInteraction::Hover("session_delete:visual-delete")),
        );
        let menu = rect(&frame, "popover_surface:session_menu_dismiss");
        assert!(menu.width >= 246.0 && menu.width <= 400.0);
        inside(Rect::new(0.0, 0.0, width, 700.0), menu, 4.0);
        let actions = [
            ("session_rename:visual-delete", "Renombrar"),
            ("session_pin:visual-delete", "Fijar"),
            ("session_mark_unread:visual-delete", "Marcar no leído"),
            ("session_archive:visual-delete", "Archivar"),
            ("session_delete:visual-delete", "Eliminar…"),
        ];
        let first = rect(&frame, actions[0].0);
        let mut label_x: Option<f32> = None;
        for (id, label) in actions {
            let row = rect(&frame, id);
            inside(menu, row, 6.0);
            assert!((row.x - first.x).abs() < 0.1, "menu row alignment: {id}");
            assert!(
                (row.width - first.width).abs() < 0.1,
                "menu row width: {id}"
            );
            assert!((row.height - 36.0).abs() < 0.1, "menu row density: {id}");
            let (position, color) = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        position,
                        text,
                        color,
                        ..
                    } if text == label
                        && position.y >= row.y
                        && position.y <= row.y + row.height =>
                    {
                        Some((*position, *color))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing native label {label}"));
            if let Some(x) = label_x {
                assert!((position.x - x).abs() < 0.1);
            }
            label_x = Some(position.x);
            assert!(position.x >= row.x + 24.0, "label reserves icon/padding");
            if id.starts_with("session_delete:") {
                assert_eq!(color, theme::DESTRUCTIVE);
                let trash = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon {
                            rect,
                            icon: Icon::Trash,
                            ..
                        } if rect.y >= row.y && rect.y + rect.height <= row.y + row.height => {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .expect("destructive row trash icon");
                assert!((trash.y + trash.height / 2.0 - row.y - row.height / 2.0).abs() < 1.0);
            }
        }
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id.starts_with("delete_confirm:")));
    }
    state.session_menu_id = None;
    state.pending_delete_session_id = Some("visual-delete".into());
    for width in [360.0, 900.0] {
        state.sidebar_collapsed = width < 600.0;
        let frame = export_with_interaction(
            build_window_view(&state, width, 700.0),
            width,
            700.0,
            &format!("session-delete-confirm-{width}"),
            Some(ExportInteraction::Focus("delete_cancel")),
        );
        let card = rect(&frame, "popover_surface:delete_cancel");
        assert!((card.width - (width - 24.0).min(440.0)).abs() < 0.1);
        assert!((card.x + card.width / 2.0 - width / 2.0).abs() < 0.1);
        assert!((card.y + card.height / 2.0 - 350.0).abs() < 0.1);
        let cancel = frame
            .regions
            .iter()
            .find(|region| region.id == "delete_cancel" && region.kind == HitKind::Button)
            .unwrap()
            .rect;
        let confirm = rect(&frame, "delete_confirm:visual-delete");
        inside(card, cancel, 24.0);
        inside(card, confirm, 24.0);
        assert!((cancel.height - confirm.height).abs() < 0.1);
        if width < 600.0 {
            assert!((cancel.x - confirm.x).abs() < 0.1);
            assert!((cancel.width - confirm.width).abs() < 0.1);
            assert!((confirm.y - cancel.y - cancel.height - 8.0).abs() < 0.1);
        } else {
            assert!((cancel.y - confirm.y).abs() < 0.1);
            assert!((confirm.x - cancel.x - cancel.width - 8.0).abs() < 0.1);
        }
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Los archivos del proyecto se conservarán."));
        assert!(text.contains("Esta acción no se puede deshacer."));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "session_delete:visual-delete"));
    }
}
