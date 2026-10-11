//! Host action integration. The fixture rejects shell configuration before spawn;
//! browser tests use only the bounded in-memory actor, never a personal profile.
use super::*;
use openide_ui::{Element, Key, Size};

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.canonicalize().unwrap().display().to_string();
    app.view.selected_session = Some("alice".into());
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.environment.open = false;
    app.viewport_width = 1280.0;
    app.viewport_height = 820.0;
    app.dock = DockState::default();
    // No environment-selected shell or native child is launched by these tests.
    app.settings_preferences.terminal_shell = crate::settings_preferences::TerminalShell::Custom(
        PathBuf::from("invalid-relative-shell-fixture"),
    );
    let scope = app.dock_scope();
    app.dock.open(&scope, DockTabKind::Files);
    app.sync_dock_view();
    (root, app)
}

fn click(app: &mut AgentsWindow, id: impl Into<String>) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn right_id(app: &AgentsWindow) -> u64 {
    app.dock.active_tab(&app.dock_scope()).unwrap().id.get()
}

#[test]
fn terminal_toggle_hide_and_reopen_preserve_both_selections_and_registry() {
    let (_root, mut app) = fixture();
    let right = right_id(&app);
    click(&mut app, "terminal_toggle");
    let first = app.active_terminal_id().unwrap();
    assert!(app.view.show_terminal_panel);
    assert_eq!(right_id(&app), right);
    assert!(app.view.show_side_pane);
    assert!(
        app.terminals.is_empty(),
        "fixture shell fails validation before spawn"
    );
    click(&mut app, "terminal_new");
    let second = app.active_terminal_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(app.view.terminal_tabs.len(), 2);
    assert!(
        app.view
            .dock_tabs
            .iter()
            .all(|tab| tab.kind != SideTab::Terminal)
    );
    click(&mut app, "terminal_panel_maximize");
    assert!(app.view.terminal_maximized);
    click(&mut app, "terminal_toggle");
    assert!(!app.view.show_terminal_panel);
    assert_eq!(app.active_terminal_id(), Some(second));
    assert!(app.dock.contains_tab(first) && app.dock.contains_tab(second));
    click(&mut app, "terminal_toggle");
    assert!(app.view.show_terminal_panel && app.view.terminal_maximized);
    assert_eq!(app.active_terminal_id(), Some(second));
    assert_eq!(right_id(&app), right);
    click(&mut app, "terminal_panel_hide");
    assert!(!app.view.show_terminal_panel);
    assert!(app.dock.contains_tab(second));
}

#[test]
fn terminal_scope_preferences_and_close_actions_never_cross_conversations() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_toggle");
    let alice = app.active_terminal_id().unwrap();
    click(&mut app, "terminal_panel_maximize");
    click(&mut app, "terminal_panel_hide");
    app.view.selected_session = Some("bob".into());
    app.sync_dock_view();
    assert!(!app.view.show_terminal_panel);
    assert!(!app.view.terminal_maximized);
    assert!(app.active_terminal_id().is_none());
    click(&mut app, format!("terminal_close:{alice}"));
    assert!(app.dock.contains_tab(alice));
    click(&mut app, "terminal_toggle");
    let bob = app.active_terminal_id().unwrap();
    assert_ne!(alice, bob);
    assert!(app.view.show_terminal_panel);
    app.view.selected_session = Some("alice".into());
    app.sync_dock_view();
    assert_eq!(app.active_terminal_id(), Some(alice));
    assert!(!app.view.show_terminal_panel);
    assert!(app.view.terminal_maximized);
    click(&mut app, format!("terminal_close:{alice}"));
    assert!(!app.dock.contains_tab(alice));
    assert!(app.dock.contains_tab(bob));
}

#[test]
fn right_close_all_and_keyboard_cycle_leave_terminal_selection_untouched() {
    let (_root, mut app) = fixture();
    let scope = app.dock_scope();
    let first = app.dock.active_tab(&scope).unwrap().id;
    click(&mut app, "terminal_new");
    let terminal = app.active_terminal_id().unwrap();
    let second = app.dock.open(&scope, DockTabKind::Activity);
    app.dock.activate(&scope, first);
    app.sync_dock_view();
    assert!(app.handle_dock_tab_event(&UiEvent::Key {
        key: Key::Tab,
        ctrl: true,
        shift: false
    }));
    assert_eq!(app.dock.active_tab(&scope).unwrap().id, second);
    assert_eq!(app.active_terminal_id(), Some(terminal));
    assert!(app.handle_dock_tab_event(&UiEvent::Key {
        key: Key::Tab,
        ctrl: true,
        shift: true
    }));
    assert_eq!(app.dock.active_tab(&scope).unwrap().id, first);
    click(&mut app, format!("dock_move_down:{}", first.get()));
    assert_eq!(
        app.dock
            .right_tabs(&scope)
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [second, first]
    );
    click(&mut app, "dock_close_all");
    assert!(app.view.dock_tabs.is_empty());
    assert!(app.view.show_terminal_panel);
    assert_eq!(app.active_terminal_id(), Some(terminal));
    assert!(app.dock.contains_tab(terminal));
}

#[test]
fn changing_right_tab_does_not_retry_an_unchanged_failed_terminal() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    let terminal = app.active_terminal_id().unwrap();
    assert!(app.view.terminal_error.is_some());
    let original_error = "Fallo original: requiere una acción explícita de Reintentar".to_owned();
    // Errors now belong to each PTY identity (including both split panes),
    // while ViewState is only the active-session projection.
    app.terminal_controls_host.errors.insert(terminal, original_error.clone());
    app.view.terminal_error = Some(original_error.clone());
    let scope = app.dock_scope();
    let next = app.dock.open(&scope, DockTabKind::Activity);
    click(&mut app, format!("dock_tab:{}", next.get()));
    assert_eq!(
        app.view.terminal_error.as_deref(),
        Some(original_error.as_str())
    );
    assert_eq!(app.active_terminal_id(), Some(terminal));
    assert!(app.terminals.is_empty());
    assert!(!app.ensure_terminal_session(terminal), "an unchanged failed launch must not retry during polling");
    assert_eq!(app.terminal_controls_host.errors.get(&terminal), Some(&original_error));
    click(&mut app, "terminal_restart");
    assert_ne!(app.view.terminal_error.as_ref(), Some(&original_error), "explicit Retry must attempt the frozen profile again");
    assert_eq!(app.view.terminal_error.as_ref(), app.terminal_controls_host.errors.get(&terminal));
    assert!(app.terminals.is_empty(), "the synthetic invalid profile must never spawn");
}

#[test]
fn file_search_opens_the_existing_file_scope_without_attaching_or_changing_dock() {
    let (_root, mut app) = fixture();
    let active = right_id(&app);
    app.view.command_query = "> comando anterior".into();
    app.command_center_attaches_file = true;
    click(&mut app, "files_search");
    assert!(app.view.show_command_center);
    assert_eq!(app.view.command_scope, Scope::Files);
    assert!(app.view.command_query.is_empty());
    assert!(!app.command_center_attaches_file);
    assert!(app.command_focus_pending);
    assert_eq!(right_id(&app), active);
    assert!(app.active_terminal_id().is_none());
}

#[test]
fn browser_more_copy_and_edit_use_the_visible_address_even_on_chrome_error() {
    let (_root, mut app) = fixture();
    let address = "http://localhost:3000/project";
    app.view.browser.url = "chrome-error://chromewebdata/".into();
    app.view.browser.url_draft = address.into();
    app.tree
        .set_root(Element::text_input("browser_url", address, "URL").size(Some(300.0), Some(32.0)));
    app.tree.layout(Size::new(300.0, 32.0));
    click(&mut app, "browser_more_toggle");
    assert!(app.view.browser.more_open);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        app.apply_action(
            UiAction::Clicked {
                id: "browser_copy_url".into(),
            },
            ui.ctx(),
        );
    });
    output.textures_delta.clear();
    assert!(
        output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if text == address)
        )
    );
    assert!(!app.view.browser.more_open);
    click(&mut app, "browser_more_toggle");
    click(&mut app, "browser_focus_url");
    assert!(!app.view.browser.more_open);
    assert_eq!(app.tree.focused_id(), Some("browser_url"));
    click(&mut app, "browser_more_toggle");
    click(&mut app, "browser_more_dismiss");
    assert!(!app.view.browser.more_open);
    assert!(app.browsers.is_empty() && app.browser_starts.is_empty());
}

#[test]
fn browser_menu_and_runtime_stay_with_their_conversation() {
    let (_root, mut app) = fixture();
    let scope = app.dock_scope();
    let browser = app.dock.open(&scope, DockTabKind::Browser);
    let (runtime, _commands) = crate::browser_runtime::tests::fixture(
        crate::browser_runtime::BrowserScope {
            workspace: PathBuf::from(&app.view.workspace),
            session_id: "alice".into(),
        },
        crate::browser_runtime::BrowserSnapshot {
            started: true,
            generation: 1,
            ..Default::default()
        },
    );
    app.browsers.insert(browser.get(), runtime);
    app.sync_dock_view();
    click(&mut app, "browser_more_toggle");
    assert!(app.view.browser.more_open);
    assert!(app.browser_client_for_current_scope().is_some());
    app.view.selected_session = Some("bob".into());
    app.sync_dock_view();
    assert!(!app.view.browser.more_open);
    assert!(app.browser_client_for_current_scope().is_none());
    assert!(app.browsers.contains_key(&browser.get()));
    assert!(app.browser_starts.is_empty());
    app.view.selected_session = Some("alice".into());
    app.sync_dock_view();
    assert_eq!(app.view.browser.tab_id, Some(browser.get()));
    assert!(app.browser_client_for_current_scope().is_some());
    assert!(!app.view.browser.more_open);
}

fn present_terminal_screen(app: &mut AgentsWindow) {
    use crate::terminal::{TerminalSnapshot, TerminalSpan, TerminalStyle};
    app.view.terminal_error = None;
    app.view.terminal_snapshot = Some(TerminalSnapshot {
        lines: vec!["resultado café".into(), "$ ".into()],
        styled_lines: vec![vec![TerminalSpan {
            text: "resultado café".into(),
            style: TerminalStyle::default(),
        }]],
        cursor: (1, 2),
        cols: 80,
        rows: 8,
        scrollback_position: 0,
        running: true,
        exit_code: None,
        error: None,
        revision: 1,
        shell: "fixture".into(),
        alternate_screen: false,
        selection: Some("café".into()),
    });
    app.tree
        .set_root(crate::terminal_ui::build_body(&app.view, 700.0, 180.0));
    app.tree.layout(Size::new(700.0, 180.0));
    assert!(app.tree.focus("terminal_screen"));
}

#[test]
fn bottom_terminal_context_copy_is_independent_of_right_tab_and_collapse() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    let terminal = app.active_terminal_id().unwrap();
    let scope = app.dock_scope();
    let files = app.dock.active_tab(&scope).unwrap().id;
    let browser = app.dock.open(&scope, DockTabKind::Browser);
    let (runtime, _commands) = crate::browser_runtime::tests::fixture(
        crate::browser_runtime::BrowserScope {
            workspace: PathBuf::from(&app.view.workspace),
            session_id: "alice".into(),
        },
        crate::browser_runtime::BrowserSnapshot {
            started: true,
            generation: 1,
            ..Default::default()
        },
    );
    app.browsers.insert(browser.get(), runtime);
    let ctx = egui::Context::default();
    for (right, side_tab) in [(files, SideTab::Files), (browser, SideTab::Browser)] {
        app.dock.activate(&scope, right);
        app.sync_dock_view();
        assert_eq!(app.view.side_tab, side_tab);
        for collapsed in [false, true] {
            app.dock.set_collapsed(&scope, collapsed);
            app.sync_dock_view();
            present_terminal_screen(&mut app);
            assert_eq!(app.active_terminal_id(), Some(terminal));
            assert!(app.view.show_terminal_panel);
            assert_eq!(app.view.show_side_pane, !collapsed);
            for (operation, expected) in [
                ("copy_selection", "café"),
                ("copy_output", "resultado café\n$"),
            ] {
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    app.apply_action(
                        UiAction::Clicked {
                            id: format!("terminal_context:{terminal}:{operation}"),
                        },
                        ui.ctx(),
                    );
                });
                output.textures_delta.clear();
                assert!(
                    output.platform_output.commands.iter().any(|command| {
                        matches!(command, egui::OutputCommand::CopyText(text) if text == expected)
                    }),
                    "{operation} was rejected with {side_tab:?}, collapsed={collapsed}"
                );
            }
        }
    }
}

#[test]
fn hidden_terminal_never_consumes_text_keys_or_native_clipboard_with_retained_focus() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    let ctx = egui::Context::default();
    for hidden_by in [
        "collapsed",
        "settings",
        "page",
        "command_center",
        "missing_region",
    ] {
        app.view.show_terminal_panel = true;
        app.view.show_settings = false;
        app.view.main_page = MainPage::Workspace;
        app.view.show_command_center = false;
        present_terminal_screen(&mut app);
        assert_eq!(app.tree.focused_id(), Some("terminal_screen"));
        match hidden_by {
            "collapsed" => app.view.show_terminal_panel = false,
            "settings" => app.view.show_settings = true,
            "page" => app.view.main_page = MainPage::Marketplace,
            "command_center" => app.view.show_command_center = true,
            "missing_region" => {
                app.tree.set_root(Element::text("Otra superficie"));
                app.tree.layout(Size::new(700.0, 180.0));
            }
            _ => unreachable!(),
        }
        if hidden_by != "missing_region" {
            assert_eq!(app.tree.focused_id(), Some("terminal_screen"));
        }
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Copy],
                ..Default::default()
            },
            |ui| {
                assert!(!app.handle_terminal_clipboard(ui.ctx()), "{hidden_by}");
                assert!(ui.ctx().input(|input| {
                    input
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::Copy))
                }));
                for event in [
                    UiEvent::Text("never send to hidden PTY".into()),
                    UiEvent::Paste("private clipboard".into()),
                    UiEvent::Key {
                        key: Key::Enter,
                        ctrl: false,
                        shift: false,
                    },
                ] {
                    assert!(
                        !app.handle_terminal_event(&event, ui.ctx()),
                        "{hidden_by}: {event:?}"
                    );
                }
            },
        );
        output.textures_delta.clear();
        assert!(
            !output
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command, egui::OutputCommand::CopyText(_)))
        );
    }
}

#[test]
fn diagonal_trackpad_scroll_routes_to_bottom_terminal_only_inside_its_screen() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    present_terminal_screen(&mut app);
    let screen = app
        .tree
        .frame()
        .regions
        .iter()
        .find(|region| region.id == "terminal_screen")
        .unwrap()
        .rect;
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        for direct in [false, true] {
            let event = UiEvent::Scroll2D {
                position: openide_ui::Point::new(screen.x + 40.0, screen.y + 40.0),
                delta_x: 3.0,
                delta_y: 36.0,
                direct,
                shift: false,
            };
            assert!(
                app.handle_terminal_event(&event, ui.ctx()),
                "diagonal vertical scrolling must not fall through to chat"
            );
        }
        assert!(!app.handle_terminal_event(
            &UiEvent::Scroll2D {
                position: openide_ui::Point::new(screen.x + screen.width + 10.0, screen.y + 40.0),
                delta_x: 3.0,
                delta_y: 36.0,
                direct: true,
                shift: false,
            },
            ui.ctx()
        ));
    });
    output.textures_delta.clear();
}

#[test]
fn browser_pointer_and_scroll_follow_the_page_at_its_corner_during_transient_resize() {
    use crate::browser_runtime::{BrowserCommand, BrowserScope, BrowserSnapshot};
    use openide_ui::{Point, Rect};

    let (_root, mut app) = fixture();
    let browser = app.dock.open(&app.dock_scope(), DockTabKind::Browser);
    let (runtime, commands) = crate::browser_runtime::tests::fixture(
        BrowserScope {
            workspace: PathBuf::from(&app.view.workspace),
            session_id: "alice".into(),
        },
        BrowserSnapshot {
            started: true,
            generation: 1,
            url: "http://localhost:3000/geometry-fixture".into(),
            viewport_width: 800,
            viewport_height: 600,
            ..Default::default()
        },
    );
    app.browsers.insert(browser.get(), runtime);
    // Dispatch geometry needs a surface that holds a frame; this host test
    // does not decode or claim rendered pixels.
    app.browser_live.insert(browser.get(), true);
    app.sync_dock_view();
    // The 800×600 page stays at the surface's corner, one point per CSS pixel,
    // while Chromium catches up. The first case represents an expanded bottom
    // terminal that clips it; the second a wider dock, whose margin beyond
    // the page is not the page.
    let page_top = crate::browser_ui::TOOLBAR_HEIGHT + 1.0;
    for (width, page_height) in [(400.0, 60.0), (1000.0, 800.0)] {
        // Reserve the same page area at any chrome density. The old fixed
        // outer heights accidentally encoded a 48 px navigation toolbar.
        let height = page_top + page_height + crate::browser_ui::STATUS_HEIGHT;
        app.tree
            .set_root(crate::browser_ui::build(&app.view, width).size(Some(width), Some(height)));
        app.tree.layout(Size::new(width, height));
        let viewport = app
            .tree
            .frame()
            .regions
            .iter()
            .find(|region| region.id == "browser_viewport")
            .unwrap()
            .rect;
        assert_eq!(viewport, Rect::new(0.0, page_top, width, page_height));
        assert_eq!(
            app.browser_viewport_rect().unwrap(),
            Rect::new(0.0, page_top, 800.0, 600.0)
        );

        if width > 800.0 {
            let margin = Point::new(900.0, page_top + 700.0);
            assert_eq!(app.tree.frame().hit_test(margin), Some("browser_viewport"));
            assert!(app.handle_browser_event(&UiEvent::PointerDown(margin)));
            assert!(app.handle_browser_event(&UiEvent::PointerUp(margin)));
            assert!(app.handle_browser_event(&UiEvent::Scroll2D {
                position: margin,
                delta_x: 3.0,
                delta_y: -6.0,
                direct: true,
                shift: false,
            }));
            assert!(
                commands.finish_next().is_none(),
                "the margin beyond the page must not click or scroll it"
            );
        }

        let content = Point::new(200.0, page_top + 30.0);
        assert!(app.handle_browser_event(&UiEvent::PointerDown(content)));
        assert!(
            matches!(commands.finish_next(), Some(BrowserCommand::PointerDown { x, y, .. })
            if (x - 200.0).abs() < 0.01 && (y - 30.0).abs() < 0.01)
        );
        assert!(app.handle_browser_event(&UiEvent::PointerUp(content)));
        assert!(
            matches!(commands.finish_next(), Some(BrowserCommand::PointerUp { x, y, .. })
            if (x - 200.0).abs() < 0.01 && (y - 30.0).abs() < 0.01)
        );
        assert!(app.handle_browser_event(&UiEvent::Scroll2D {
            position: content,
            delta_x: 3.0,
            delta_y: -6.0,
            direct: true,
            shift: false,
        }));
        assert!(
            matches!(commands.finish_next(), Some(BrowserCommand::Scroll { dx, dy })
            if (dx + 3.0).abs() < 0.01 && (dy - 6.0).abs() < 0.01)
        );
        assert!(commands.finish_next().is_none());
        // Every request above was answered; nothing stays in flight for the
        // next pane size.
        app.browser_requests.clear();
    }
}

fn present_workspace(app: &mut AgentsWindow) {
    app.tree.set_root(crate::view::build_window_view(
        &app.view,
        app.viewport_width,
        app.viewport_height,
    ));
    app.tree
        .layout(Size::new(app.viewport_width, app.viewport_height));
}

fn region_rect(app: &AgentsWindow, id: &str) -> openide_ui::Rect {
    app.tree
        .frame()
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap_or_else(|| panic!("missing visible region {id}"))
        .rect
}

#[test]
fn floating_terminal_menu_owns_pointer_over_every_workspace_divider() {
    use openide_ui::Point;

    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    present_terminal_screen(&mut app);
    app.view.terminal_menu_open = true;
    present_workspace(&mut app);
    assert!(app.tree.has_open_popover());
    assert!(app.tree.frame().regions.iter().any(|r| r.id == "terminal_copy"));

    let layout = crate::view::dock_layout(&app.view, app.viewport_width);
    let split_y = app.viewport_height
        - 4.0
        - crate::view::terminal_panel_height(&app.view, app.viewport_height);
    let sizes = (
        app.view.left_dock_width,
        app.view.right_dock_width,
        app.view.terminal_panel_height,
    );
    for point in [
        Point::new(layout.left_split_x.unwrap(), 160.0),
        Point::new(layout.right_split_x.unwrap(), 160.0),
        Point::new(layout.left + 120.0, split_y),
    ] {
        let width = app.viewport_width;
        assert!(
            !app.handle_dock_drag(&UiEvent::PointerDown(point), width),
            "a menu or its dismissal backdrop owns this point: {point:?}"
        );
        assert!(!app.handle_dock_drag(
            &UiEvent::PointerMove(Point::new(point.x + 60.0, point.y - 60.0)),
            width,
        ));
        assert!(app.dock_drag.is_none());
    }
    assert_eq!(
        (
            app.view.left_dock_width,
            app.view.right_dock_width,
            app.view.terminal_panel_height,
        ),
        sizes
    );
    // The host did not swallow the menu's actual action: the same frame still
    // dispatches it through UiTree when it is clicked normally.
    let copy = region_rect(&app, "terminal_copy");
    let point = Point::new(copy.x + copy.width / 2.0, copy.y + copy.height / 2.0);
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert!(actions.iter().any(|action| {
        matches!(action, UiAction::Clicked { id } if id == "terminal_copy")
    }));
}

#[test]
fn leaving_window_cancels_divider_capture_before_a_later_pointer_move() {
    use openide_ui::Point;

    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    let width = app.viewport_width;
    let layout = crate::view::dock_layout(&app.view, width);
    let split_y = app.viewport_height
        - 4.0
        - crate::view::terminal_panel_height(&app.view, app.viewport_height);
    for point in [
        Point::new(layout.left_split_x.unwrap(), 160.0),
        Point::new(layout.right_split_x.unwrap(), 160.0),
        Point::new(layout.left + 120.0, split_y),
    ] {
        assert!(app.handle_dock_drag(&UiEvent::PointerDown(point), width));
        assert!(app.dock_drag.is_some());
        app.handle_dock_drag(&UiEvent::PointerLeave, width);
        assert!(app.dock_drag.is_none());
        let sizes = (
            app.view.left_dock_width,
            app.view.right_dock_width,
            app.view.terminal_panel_height,
        );
        assert!(!app.handle_dock_drag(
            &UiEvent::PointerMove(Point::new(point.x + 90.0, point.y - 90.0)),
            width,
        ));
        assert_eq!(
            (
                app.view.left_dock_width,
                app.view.right_dock_width,
                app.view.terminal_panel_height,
            ),
            sizes
        );
    }
}

#[test]
fn bottom_terminal_geometry_is_independent_of_right_width_and_top_content_is_clipped() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    present_terminal_screen(&mut app);
    app.view.left_dock_width = Some(240.0);
    for (width, height) in [(1080.0, 480.0), (1440.0, 820.0)] {
        app.viewport_width = width;
        app.viewport_height = height;
        for maximized in [false, true] {
            app.view.terminal_maximized = maximized;
            let mut bottom_rect = None;
            for right_width in [280.0, 420.0] {
                app.view.right_dock_width = Some(right_width);
                present_workspace(&mut app);
                let terminal_height = crate::view::terminal_panel_height(&app.view, height);
                let split_y = height - 4.0 - terminal_height;
                let screen = region_rect(&app, "terminal_screen");
                assert!(screen.height > 0.0, "{width}×{height}, maximized={maximized}");
                assert!(
                    (screen.y - split_y - crate::terminal_dock_ui::HEADER_HEIGHT).abs() <= 0.1,
                    "PTY must begin directly below its compact tab strip: {screen:?}, split={split_y}"
                );
                assert!(screen.y + screen.height <= height - 4.0 + 0.1);
                assert!(screen.x + screen.width <= width - 4.0 + 0.1);
                if let Some(previous) = bottom_rect {
                    assert_eq!(screen, previous, "right width must change only the top row");
                }
                bottom_rect = Some(screen);
                for id in ["prompt", "run_toggle", "files_search"] {
                    let rect = region_rect(&app, id);
                    if id == "run_toggle" {
                        let size = openide_ui::ControlSize::Compact.height();
                        assert!((rect.height - size).abs() <= 0.1 && (rect.width - size).abs() <= 0.1,
                            "compact send control must remain intact: {rect:?}");
                    } else {
                        assert!(rect.height >= 23.9, "{id} must remain usable: {rect:?}");
                    }
                    assert!(
                        rect.y + rect.height <= split_y + 0.1,
                        "{id} hit region escaped into terminal: {rect:?}, split={split_y}"
                    );
                }
                for region in app.tree.frame().regions.iter().filter(|region| {
                    region.id.starts_with("dock_tab:")
                        || region.id.starts_with("terminal_tab:")
                }) {
                    if region.id.starts_with("dock_tab:") {
                        assert!(region.rect.y + region.rect.height <= split_y + 0.1);
                    } else {
                        assert!(region.rect.y >= split_y - 0.1);
                    }
                }
                // A right divider has no interactive continuation through the
                // bottom panel; its x-coordinate there belongs to the PTY.
                let layout = crate::view::dock_layout(&app.view, width);
                let point = openide_ui::Point::new(
                    layout.right_split_x.unwrap(),
                    screen.y + screen.height / 2.0,
                );
                assert!(!app.handle_dock_drag(&UiEvent::PointerDown(point), width));
            }
        }
    }
}

#[test]
fn temporary_window_height_does_not_replace_the_workspace_terminal_preference() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    app.dock.set_terminal_height_px(&app.view.workspace, 400.0);
    app.sync_dock_view();
    assert_eq!(crate::view::terminal_panel_height(&app.view, 820.0), 400.0);

    app.viewport_height = 400.0;
    // Any unrelated dock action can synchronize while the window is small.
    app.sync_dock_view();
    assert!(crate::view::terminal_panel_height(&app.view, 400.0) < 400.0);
    app.viewport_height = 820.0;
    assert_eq!(
        crate::view::terminal_panel_height(&app.view, 820.0),
        400.0,
        "growing the window restores the desired height without needing another dock action"
    );
}

#[test]
fn maximized_terminal_keeps_home_draft_editor_and_submit_inside_the_top_workspace() {
    let (_root, mut app) = fixture();
    app.view.selected_session = None;
    app.sync_dock_view();
    click(&mut app, "terminal_new");
    present_terminal_screen(&mut app);
    app.view.terminal_maximized = true;
    app.view.prompt = (0..20)
        .map(|index| format!("Conservar la instrucción número {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    // with_data_root already created the retained prompt editor. Programmatic
    // draft changes use the same explicit synchronization as the host.
    app.tree.set_input_value("prompt", app.view.prompt.clone());
    {
        present_workspace(&mut app);
        let split_y = app.viewport_height
            - 4.0
            - crate::view::terminal_panel_height(&app.view, app.viewport_height);
        for id in ["prompt", "run_toggle"] {
            let rect = region_rect(&app, id);
            assert!(rect.height >= 24.0, "{id} is clipped: {rect:?}");
            assert!(rect.y >= 4.0);
            assert!(
                rect.y + rect.height <= split_y + 0.1,
                "{id} escaped below the top workspace: {rect:?}"
            );
        }
        assert_eq!(app.tree.input_value("prompt"), Some(app.view.prompt.as_str()));
    }
}

#[test]
fn stacked_environment_and_maximized_terminal_preserve_home_and_session_composer_bounds() {
    for is_home in [true, false] {
        for height in [480.0, 820.0] {
            let (_root, mut app) = fixture();
            app.viewport_width = 1080.0;
            app.viewport_height = height;
            app.view.left_dock_width = Some(240.0);
            app.view.sidebar_collapsed = false;
            app.view.left_presentation_width = None;
            app.view.right_presentation_width = None;
            if is_home {
                app.view.selected_session = None;
            }
            // The fixture's original Files tab belongs to Alice. Home has a
            // separate draft scope, so establish its own actual right pane.
            let scope = app.dock_scope();
            let files = app.dock.open(&scope, DockTabKind::Files);
            app.dock.activate(&scope, files);
            app.dock.set_collapsed(&scope, false);
            app.dock.set_width_px(&scope.workspace_id, 360.0);
            app.sync_dock_view();
            click(&mut app, "terminal_new");
            present_terminal_screen(&mut app);
            app.view.terminal_maximized = true;
            app.view.prompt = "Conservar este borrador\nY mantener accesible el envío".into();
            app.tree.set_input_value("prompt", app.view.prompt.clone());
            let layout = crate::view::dock_layout(&app.view, app.viewport_width);
            assert_eq!(app.view.side_tab, SideTab::Files);
            assert!(layout.left > 0.0 && layout.right.is_some(), "{layout:?}, home={is_home}");
            assert!(layout.center < 616.0, "fixture must exercise stacked Environment");
            let closed_height = crate::view::terminal_panel_height(&app.view, height);

            for progress in [1.0, 0.5] {
                app.view.environment.open = progress == 1.0;
                app.view.environment_presentation_progress = Some(progress);
                present_workspace(&mut app);
                let terminal_height = crate::view::terminal_panel_height(&app.view, height);
                assert!(terminal_height < closed_height);
                let split_y = height - 4.0 - terminal_height;
                let environment = region_rect(&app, "chat_environment");
                let prompt = region_rect(&app, "prompt");
                let submit = region_rect(&app, "run_toggle");
                assert!(prompt.height >= 24.0, "home={is_home}, height={height}, progress={progress}: {prompt:?}");
                assert!(submit.height >= openide_ui::ControlSize::Compact.height() - 0.1, "home={is_home}, height={height}, progress={progress}: {submit:?}");
                assert!(environment.y + environment.height <= prompt.y + 0.1,
                    "Environment overlaps the draft: {environment:?}, {prompt:?}");
                for (id, rect) in [("prompt", prompt), ("run_toggle", submit)] {
                    assert!(rect.y >= 4.0);
                    assert!(rect.y + rect.height <= split_y + 0.1,
                        "{id} crosses the bottom separator: {rect:?}, split={split_y}");
                }
                let terminal = region_rect(&app, "terminal_screen");
                assert!(terminal.height > 0.0);
                assert!(
                    (terminal.y - split_y - crate::terminal_dock_ui::HEADER_HEIGHT).abs() <= 0.1,
                    "PTY must remain below the tab strip: {terminal:?}, split={split_y}"
                );
                assert_eq!(app.tree.input_value("prompt"), Some(app.view.prompt.as_str()));
            }

            app.view.environment.open = false;
            app.view.environment_presentation_progress = None;
            present_workspace(&mut app);
            assert_eq!(crate::view::terminal_panel_height(&app.view, height), closed_height);
            assert!(!app.tree.frame().regions.iter().any(|region| region.id == "chat_environment"));
            assert_eq!(app.tree.input_value("prompt"), Some(app.view.prompt.as_str()));
        }
    }
}

#[test]
fn environment_motion_moves_the_bottom_separator_continuously_without_hiding_composer_actions() {
    for is_home in [true, false] {
        for height in [480.0, 820.0] {
            let (_root, mut app) = fixture();
            app.viewport_width = 1080.0;
            app.viewport_height = height;
            app.view.left_dock_width = Some(240.0);
            app.view.sidebar_collapsed = false;
            app.view.left_presentation_width = None;
            app.view.right_presentation_width = None;
            app.view.selected_session = (!is_home).then(|| "alice".into());
            let scope = app.dock_scope();
            let files = app.dock.open(&scope, DockTabKind::Files);
            app.dock.activate(&scope, files);
            app.dock.set_collapsed(&scope, false);
            app.dock.set_width_px(&scope.workspace_id, 360.0);
            app.sync_dock_view();
            click(&mut app, "terminal_new");
            present_terminal_screen(&mut app);
            app.view.terminal_maximized = true;
            app.view.prompt = "Mantener el borrador\nY el envío durante la transición".into();
            app.tree.set_input_value("prompt", app.view.prompt.clone());
            let closed_height = crate::view::terminal_panel_height(&app.view, height);

            for opening in [true, false] {
                app.view.environment.open = opening;
                let mut previous: Option<f32> = None;
                for step in 0..=8 {
                    let progress = if opening { step as f32 / 8.0 } else { 1.0 - step as f32 / 8.0 };
                    app.view.environment_presentation_progress = Some(progress);
                    present_workspace(&mut app);
                    let terminal_height = crate::view::terminal_panel_height(&app.view, height);
                    if let Some(previous) = previous {
                        let movement = terminal_height - previous;
                        assert!(movement.abs() <= 30.0, "separator jumped {movement}px at progress={progress}");
                        assert!(if opening { movement <= 0.01 } else { movement >= -0.01 });
                    }
                    previous = Some(terminal_height);
                    let split_y = height - 4.0 - terminal_height;
                    let prompt = region_rect(&app, "prompt");
                    let submit = region_rect(&app, "run_toggle");
                    assert!(prompt.height >= 24.0, "home={is_home}, progress={progress}: {prompt:?}");
                    assert!(submit.height >= openide_ui::ControlSize::Compact.height() - 0.1, "home={is_home}, progress={progress}: {submit:?}");
                    assert!(prompt.y + prompt.height <= split_y + 0.1);
                    assert!(submit.y + submit.height <= split_y + 0.1);
                    if let Some(environment) = app.tree.frame().regions.iter().find(|r| r.id == "chat_environment") {
                        assert!(environment.rect.y + environment.rect.height <= prompt.y + 0.1);
                    }
                    let terminal = region_rect(&app, "terminal_screen");
                    assert!(
                        (terminal.y - split_y - crate::terminal_dock_ui::HEADER_HEIGHT).abs() <= 0.1,
                        "PTY must track the tab strip during motion: {terminal:?}, split={split_y}"
                    );
                }
            }
            assert_eq!(crate::view::terminal_panel_height(&app.view, height), closed_height);

            // The real host clears presentation progress for reduced motion.
            // Both the settled fallback and snap must reach the exact bounds.
            app.reduced_motion = true;
            app.view.environment.open = true;
            assert!(!app.update_environment_motion(Instant::now()));
            assert_eq!(app.view.environment_presentation_progress, None);
            assert_eq!(crate::view::terminal_panel_height(&app.view, height), closed_height - 180.0);
            app.view.environment.open = false;
            assert!(!app.update_environment_motion(Instant::now()));
            assert_eq!(app.view.environment_presentation_progress, None);
            assert_eq!(crate::view::terminal_panel_height(&app.view, height), closed_height);
            assert_eq!(app.tree.input_value("prompt"), Some(app.view.prompt.as_str()));
        }
    }
}
