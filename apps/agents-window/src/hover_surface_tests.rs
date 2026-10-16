//! Pointer-driven coverage of app controls on canvas and elevated surfaces.
use super::super::*;
use openide_ui::{DrawCommand, Frame, Point, Rect, Size, UiEvent, UiTree};
use std::time::{Duration, Instant};

fn region(frame: &Frame, id: &str) -> Rect {
    frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
        .rect
}

fn center(rect: Rect) -> Point {
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn fill(frame: &Frame, bounds: Rect, color: Color) -> bool {
    frame.commands.iter().any(|command| {
        matches!(command,
            DrawCommand::FillRect { rect, color: actual, .. }
                if *rect == bounds && *actual == color
        )
    })
}

/// Advance the shared animation clock, assert paint and geometry, then leave.
fn hover(tree: &mut UiTree, size: Size, id: &str, expected: Color) -> Frame {
    let now = Instant::now();
    let idle = tree.layout_at(size, now);
    let bounds = region(&idle, id);
    let point = center(bounds);
    assert_eq!(idle.hit_test(point), Some(id));
    tree.handle_event_at(UiEvent::PointerMove(point), now);
    tree.layout_at(size, now);
    let hovered = tree.layout_at(size, now + Duration::from_millis(150));
    assert_eq!(region(&hovered, id), bounds, "{id} moved during hover");
    assert!(
        fill(&hovered, bounds, expected),
        "{id} needs visible contextual feedback"
    );
    tree.handle_event_at(
        UiEvent::PointerMove(Point::new(-20.0, -20.0)),
        now + Duration::from_millis(160),
    );
    tree.layout_at(size, now + Duration::from_millis(160));
    let left = tree.layout_at(size, now + Duration::from_millis(310));
    assert_eq!(region(&left, id), bounds);
    assert!(
        !fill(&left, bounds, expected),
        "{id} remained highlighted after leaving"
    );
    hovered
}

fn surface(root: Element, size: Size) -> UiTree {
    UiTree::new(
        Element::panel(root)
            .background(theme::BACKGROUND)
            .size(Some(size.width), Some(size.height)),
    )
}

fn icon_centered(frame: &Frame, id: &str, icon: Icon) {
    icon_centered_at_size(frame, id, icon, 32.0);
}

fn icon_centered_at_size(frame: &Frame, id: &str, icon: Icon, target_size: f32) {
    let bounds = region(frame, id);
    assert_eq!((bounds.width, bounds.height), (target_size, target_size));
    let icon = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Icon {
                rect, icon: actual, ..
            } if *actual == icon && bounds.contains(center(*rect)) => Some(*rect),
            _ => None,
        })
        .expect("icon must be painted inside its own control");
    assert_eq!((icon.width, icon.height), (16.0, 16.0));
    assert!((center(icon).x - center(bounds).x).abs() < 0.01);
    assert!((center(icon).y - center(bounds).y).abs() < 0.01);
}

#[test]
fn canvas_terminal_source_and_environment_actions_keep_hover_contrast() {
    let mut state = tests::empty_state();
    state.terminal_snapshot = None;
    state.terminal_error = Some("No se pudo iniciar el shell".into());
    let size = Size::new(360.0, 280.0);
    let mut terminal = surface(crate::terminal_ui::build(&state, size.width), size);
    let frame = terminal.layout(size);
    assert!(fill(
        &frame,
        region(&frame, "terminal_restart"),
        theme::BACKGROUND
    ));
    hover(&mut terminal, size, "terminal_restart", theme::INPUT);

    let mut source = surface(crate::conversation_source_ui::open_button(&state, 0), size);
    hover(
        &mut source,
        size,
        "conversation_source:draft:0:0:open",
        theme::INPUT,
    );

    state.environment.open = true;
    // Use the actual conversation header: a standalone Panel deliberately
    // fills its child, which is not the host geometry of this icon control.
    let mut environment = surface(
        Element::column(vec![
            crate::session_surface::header(&state, size.width, true, true),
            Element::spacer().flex(1.0),
        ]),
        size,
    );
    let frame = hover(&mut environment, size, "environment_open", theme::INPUT);
    icon_centered_at_size(&frame, "environment_open", Icon::Laptop, 24.0);
}

#[test]
fn terminal_canvas_tools_and_passive_breadcrumb_do_not_cover_neighbor_controls() {
    let mut state = tests::empty_state();
    state.workspace = "/workspace/openADE".into();
    state.terminal_snapshot = Some(crate::terminal::TerminalSnapshot {
        lines: vec!["$".into()],
        styled_lines: vec![Vec::new()],
        cursor: (0, 1),
        cols: 80,
        rows: 24,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "bash".into(),
        alternate_screen: false,
        selection: None,
    });
    for width in [320.0, 720.0] {
        let size = Size::new(width, 360.0);
        let mut tree = surface(crate::terminal_ui::build(&state, width), size);
        let frame = hover(&mut tree, size, "terminal_clear", theme::INPUT);
        icon_centered(&frame, "terminal_clear", Icon::Trash);
        let clear = region(&frame, "terminal_clear");
        let interrupt = region(&frame, "terminal_interrupt");
        assert!(clear.x + clear.width <= interrupt.x);
        assert!(!fill(&frame, interrupt, theme::INPUT));
        let resource = region(&frame, "terminal_path:item:1");
        assert!(
            resource.width < 100.0,
            "passive path should hug its filename: {resource:?}"
        );
        assert_ne!(frame.hit_test(center(resource)), Some("terminal_clear"));
    }
}

#[test]
fn tab_close_hover_stays_inside_tab_and_keeps_neighbor_and_tab_selection_distinct() {
    let mut state = tests::empty_state();
    state.dock_tabs = vec![
        DockTabView {
            id: 41,
            title: "commands.rs".into(),
            full_path: None,
            active: true,
            kind: SideTab::FilePreview,
            closable: true,
        },
        DockTabView {
            id: 42,
            title: "Terminal".into(),
            full_path: None,
            active: false,
            kind: SideTab::Terminal,
            closable: true,
        },
    ];
    let active_surface = |frame: &Frame, bounds: Rect| {
        frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillCornerRect { rect, color, radii }
                if *rect == bounds
                    && *color == theme::BACKGROUND
                    && *radii == openide_ui::CornerRadii::top(8.0)
        ))
    };
    for width in [360.0, 720.0] {
        let size = Size::new(width, RIGHT_HEADER_HEIGHT);
        let mut tree = surface(dock_chrome::header(&state, width), size);
        // Integrated tabs join the editor canvas; their active surface is
        // BACKGROUND, while SELECTED belongs to the older detached tab style.
        let idle = tree.layout(size);
        let active_tab = region(&idle, "dock_tab:41");
        assert!(active_surface(&idle, active_tab));
        let frame = hover(&mut tree, size, "dock_close:41", theme::INPUT);
        icon_centered_at_size(&frame, "dock_close:41", Icon::Close, 20.0);
        let close = region(&frame, "dock_close:41");
        let tab = region(&frame, "dock_tab:41");
        assert!(tab.contains(center(close)));
        assert_eq!(frame.hit_test(center(close)), Some("dock_close:41"));
        assert_eq!(tab, active_tab);
        assert!(
            active_surface(&frame, tab),
            "close hover must not replace tab state"
        );
        assert!(!fill(
            &frame,
            region(&frame, "dock_tabs_menu"),
            theme::INPUT
        ));
    }
}

#[test]
fn file_tree_row_hover_is_local_and_uses_canvas_contrast_at_multiple_widths() {
    let mut state = tests::empty_state();
    state.file_rows = vec![
        FileRowView {
            name: "src".into(),
            depth: 0,
            is_dir: true,
            expanded: true,
            can_load_more: false,
        },
        FileRowView {
            name: "commands.rs".into(),
            depth: 1,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        },
        FileRowView {
            name: "Cargo.toml".into(),
            depth: 0,
            is_dir: false,
            expanded: false,
            can_load_more: false,
        },
    ];
    for width in [320.0, 720.0] {
        let size = Size::new(width, 360.0);
        let mut tree = surface(file_tree_content(&state, width), size);
        let frame = hover(&mut tree, size, "file_row:1", theme::INPUT);
        for id in ["file_row:0", "file_row:2"] {
            assert!(
                !fill(&frame, region(&frame, id), theme::INPUT),
                "neighbor {id} must stay idle"
            );
        }
        let file = region(&frame, "file_row:1");
        assert!(file.x >= 0.0 && file.x + file.width <= width);
        assert_eq!(frame.hit_test(center(file)), Some("file_row:1"));
    }
}

#[test]
fn raised_notice_and_account_card_actions_contrast_without_brightening_the_whole_card() {
    let pending = crate::composer_pending::PendingComposerSelection::new(
        crate::composer_pending::ComposerScope {
            workspace: "/workspace".into(),
            session: None,
        },
    );
    let size = Size::new(720.0, 900.0);
    let mut props = crate::composer_ui::tests::props("Revisa el proyecto", size.width);
    props.pending = Some(&pending);
    let mut composer = surface(crate::composer_ui::build_composer(&props), size);
    let undo = format!("composer_pending_undo:{}", pending.token);
    hover(&mut composer, size, &undo, theme::hover_for(theme::CARD));

    let mut state = tests::empty_state();
    state.github_account = crate::github_account::GithubAccountStatus::Connected(
        crate::github_account::GithubIdentity {
            login: "example".into(),
            avatar: None,
        },
    );
    state.github_contributions = crate::github_contributions::GithubContributionsStatus::Ready(
        crate::github_contributions::GithubContributionCalendar {
            login: "example".into(),
            total: 0,
            weeks: vec![],
        },
    );
    let mut settings = surface(
        crate::settings_ui::build_settings_page(
            &state,
            crate::settings_ui::SettingsPage::Github,
            size.width,
            true,
        ),
        size,
    );
    let frame = hover(
        &mut settings,
        size,
        "github_contributions_refresh",
        theme::hover_for(theme::CARD),
    );
    icon_centered_at_size(&frame, "github_contributions_refresh", Icon::Refresh, 24.0);
    let refresh = region(&frame, "github_contributions_refresh");
    assert!(frame.commands.iter().any(|command| matches!(command,
        DrawCommand::FillRect { rect, color, .. }
            if *color == theme::CARD && rect.contains(center(refresh)) && rect.width > refresh.width
    )), "the card stays at its resting surface while its icon is hovered");
}

#[test]
fn agent_sidebar_toolbar_uses_shared_corners_without_overlapping_actions() {
    let shared_radius = UiIconButton::new(
        "reference_tool",
        Icon::Filter,
        theme::action_palette(theme::SIDEBAR, theme::FOREGROUND_SUBTLE),
    )
    .variant(ButtonVariant::Subtle)
    .size(ControlSize::Dense)
    .render()
    .style
    .radius;
    for width in [176.0, 200.0, 264.0, 320.0] {
        for organization in [SessionOrganization::Project, SessionOrganization::Group] {
            let mut state = tests::empty_state();
            state.session_organization = organization;
            state.sessions.push(SessionView {
                id: "toolbar-session".into(),
                title: "Tarea".into(),
                project: "/workspace".into(),
                backend: "native".into(),
                pinned: false,
                archived: false,
                unread: false,
                native_automation: None,
            });
            let context = if organization == SessionOrganization::Group {
                ("session_group_new", Icon::Add)
            } else {
                ("session_expand_all", Icon::Collapse)
            };
            let targets = [
                context,
                ("session_sort_toggle", Icon::Sort),
                ("toggle_archived_view", Icon::Archive),
            ];
            let size = Size::new(width, 640.0);
            let mut tree = surface(sidebar_for_height(&state, width, size.height), size);
            for (id, icon) in targets {
                let frame = hover(&mut tree, size, id, theme::hover_for(theme::SIDEBAR));
                icon_centered_at_size(&frame, id, icon, SIDEBAR_TOOL_SIZE);
                let bounds = region(&frame, id);
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::FillRect { rect, color, radius }
                        if *rect == bounds && *color == theme::hover_for(theme::SIDEBAR)
                            && *radius == shared_radius && *radius < bounds.height / 4.0
                )), "{id} must use the compact shared hover shape");
                assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width);
                for (neighbor, _) in targets {
                    if neighbor != id {
                        let neighbor_bounds = region(&frame, neighbor);
                        assert!(!bounds.contains(center(neighbor_bounds)));
                        assert!(!fill(&frame, neighbor_bounds, theme::hover_for(theme::SIDEBAR)));
                    }
                }
                tree.handle_event(UiEvent::PointerDown(center(bounds)));
                assert!(tree.handle_event(UiEvent::PointerUp(center(bounds))).contains(
                    &openide_ui::UiAction::Clicked { id: id.into() }
                ));
            }
        }
    }
}

#[test]
fn ide_sidebar_controls_contrast_on_gray_and_keep_their_host_actions() {
    for width in [180.0, 264.0, 420.0] {
        let mut state = tests::empty_state();
        state.ide.mode = crate::ide_workspace::WindowMode::Ide;
        state.ide_search.query = "workspace".into();
        state.debug.scope = "test-workspace".into();
        let size = Size::new(width, 600.0);
        for (root, targets) in [
            (
                ide_search_ui::panel(&state, width, size.height),
                vec![
                    ("ide_search_case".to_owned(), "ide_search_case".to_owned()),
                    (
                        "ide_search_submit".to_owned(),
                        "ide_search_submit".to_owned(),
                    ),
                ],
            ),
            (
                ide_activity_ui::extensions_sidebar(&state, width, size.height),
                vec![
                    (
                        "ide_extensions_refresh".to_owned(),
                        "marketplace_refresh".to_owned(),
                    ),
                    (
                        "marketplace_sources_open".to_owned(),
                        "ide_extensions_catalog".to_owned(),
                    ),
                ],
            ),
            (
                crate::debug_ui::build_sidebar(&state.debug, width, size.height),
                vec![(
                    crate::debug_ui::action(&state.debug.scope, "reload"),
                    crate::debug_ui::action(&state.debug.scope, "reload"),
                )],
            ),
        ] {
            let mut tree = surface(root, size);
            for (id, action) in targets {
                let frame = hover(&mut tree, size, &id, theme::hover_for(theme::SIDEBAR));
                let bounds = region(&frame, &id);
                assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width);
                tree.handle_event(UiEvent::PointerDown(center(bounds)));
                assert!(tree
                    .handle_event(UiEvent::PointerUp(center(bounds)))
                    .contains(&openide_ui::UiAction::Clicked { id: action }));
            }
        }
    }
}
