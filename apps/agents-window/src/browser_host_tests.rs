use super::*;
use crate::browser_runtime::{
    tests::BrowserCommands, BrowserCommand, BrowserScope, BrowserSnapshot,
};
use openide_ui::{Element, Key, Point, Size};

fn fixture() -> (tempfile::TempDir, AgentsWindow, DockTabId) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.canonicalize().unwrap().display().to_string();
    app.view.selected_session = Some("browser-a".into());
    app.view.main_page = MainPage::Workspace;
    app.view.right_dock_width = Some(664.0);
    app.viewport_height = 660.0;
    // Host unit tests must not spawn real browsers when exercising Retry.
    app.view.show_side_pane = false;
    let id = app.dock.open(&app.dock_scope(), DockTabKind::Browser);
    app.sync_browser_tab();
    (root, app, id)
}

fn install_runtime(
    app: &mut AgentsWindow,
    id: DockTabId,
    snapshot: BrowserSnapshot,
) -> BrowserCommands {
    let (runtime, commands) = crate::browser_runtime::tests::fixture(
        BrowserScope {
            workspace: PathBuf::from(&app.view.workspace),
            session_id: "browser-a".into(),
        },
        snapshot,
    );
    app.browsers.insert(id.get(), runtime);
    commands
}

fn blank(generation: u64) -> BrowserSnapshot {
    BrowserSnapshot {
        started: true,
        generation,
        viewport_width: 640,
        viewport_height: 480,
        ..Default::default()
    }
}

fn viewport(app: &mut AgentsWindow) {
    // The native renderer is not invoked here. This is only the surface the
    // host resolves focus and event dispatch against, as if it held a frame.
    let tab = app.view.browser.tab_id.unwrap();
    app.browser_live.insert(tab, true);
    app.view.browser.live = true;
    app.view.browser.started = true;
    app.view.show_side_pane = true;
    app.view.show_settings = false;
    app.view.show_command_center = false;
    app.view.browser.viewport_width = 640;
    app.view.browser.viewport_height = 480;
    app.tree.set_root(Element::column(vec![
        Element::text_input("browser_url", &app.view.browser.url_draft, "Dirección")
            .size(Some(320.0), Some(32.0)),
        Element::clickable(
            "browser_viewport",
            Element::surface(crate::browser_ui::surface_id(tab), "Fixture viewport")
                .size(Some(320.0), Some(240.0)),
        )
        .size(Some(320.0), Some(240.0)),
    ]));
    app.tree.layout(Size::new(320.0, 272.0));
}

#[test]
fn browser_dimensions_fallback_matches_the_rendered_viewport_in_both_window_modes() {
    // A surface that holds a frame makes this the real Page surface without
    // a browser process, server, or terminal actor behind the UI fixture.
    let (_root, mut app, _) = fixture();

    for mode in [
        crate::ide_workspace::WindowMode::Ide,
        crate::ide_workspace::WindowMode::Agents,
    ] {
        for size in [Size::new(1440.0, 900.0), Size::new(1040.0, 720.0)] {
            for terminal_visible in [false, true] {
                app.view = crate::view::ide_visual_tests::fixture();
                app.view.ide.mode = mode;
                app.view.show_terminal_panel = terminal_visible;
                app.view.ide.bottom_open = terminal_visible;
                app.view.show_side_pane = true;
                app.view.side_tab = SideTab::Browser;
                app.view.right_dock_width = Some(if size.width < 1200.0 { 320.0 } else { 420.0 });
                app.view.dock_tabs = vec![crate::view::DockTabView {
                    id: 71,
                    title: "Navegador".into(),
                    full_path: None,
                    active: true,
                    kind: SideTab::Browser,
                    closable: true,
                }];
                app.view.browser = crate::browser_ui::BrowserUiState {
                    started: true,
                    url: "http://localhost:3000/".into(),
                    url_draft: "http://localhost:3000/".into(),
                    ..Default::default()
                };
                app.viewport_width = size.width;
                app.viewport_height = size.height;
                app.tree.set_root(crate::view::build_window_view(
                    &app.view,
                    size.width,
                    size.height,
                ));
                app.tree.layout(size);
                assert!(!app
                    .tree
                    .frame()
                    .regions
                    .iter()
                    .any(|region| region.id == "browser_viewport"));
                let fallback = app.browser_dimensions();

                app.view.browser.live = true;
                app.tree.set_root(crate::view::build_window_view(
                    &app.view,
                    size.width,
                    size.height,
                ));
                app.tree.layout(size);
                let viewport = app
                    .tree
                    .frame()
                    .regions
                    .iter()
                    .find(|region| region.id == "browser_viewport")
                    .expect("visible browser page")
                    .rect;
                let rendered = (
                    viewport.width.round().clamp(320.0, 1600.0) as u32,
                    viewport.height.round().clamp(240.0, 1200.0) as u32,
                );
                assert_eq!(
                    fallback, rendered,
                    "mode={mode:?}, size={size:?}, terminal={terminal_visible}"
                );
                assert_eq!(app.browser_dimensions(), rendered);
            }
        }
    }
    assert!(app.browsers.is_empty());
    assert!(app.browser_starts.is_empty());
}

#[test]
fn retry_address_survives_startup_and_switching_tabs_and_is_dispatched_once_to_its_owner() {
    let (_root, mut app, id) = fixture();
    let scope_a = app.dock_scope();
    let address = "http://localhost:3000/retry?value=á";
    app.view.browser.url = "http://localhost:3000/old".into();
    app.view.browser.url_draft = address.into();
    app.restart_browser();
    assert!(
        app.browser_starts.is_empty(),
        "hidden retry does not start a process"
    );
    assert_eq!(
        app.browser_restart_urls.get(&id.get()).map(String::as_str),
        Some(address)
    );
    assert!(app.apply_browser_snapshot(id.get(), blank(1)));
    assert_eq!(app.view.browser.url_draft, address);

    app.view.selected_session = Some("browser-b".into());
    let other = app.dock.open(&app.dock_scope(), DockTabKind::Browser);
    app.sync_browser_tab();
    assert_eq!(app.view.browser.tab_id, Some(other.get()));
    assert_ne!(app.view.browser.url_draft, address);
    app.view.selected_session = Some("browser-a".into());
    app.sync_browser_tab();
    assert_eq!(app.view.browser.url_draft, address);
    assert_eq!(app.tree.input_value("browser_url"), Some(address));

    // Finish startup while another conversation is visible. Only the original
    // runtime gets Navigate, regardless of the active dock tab.
    app.view.selected_session = Some("browser-b".into());
    app.sync_browser_tab();
    let commands = install_runtime(&mut app, id, blank(2));
    app.update_browser(&egui::Context::default());
    assert!(!app.browser_restart_urls.contains_key(&id.get()));
    assert_eq!(app.view.browser.tab_id, Some(other.get()));

    // Switching back during the queued navigation also preserves the URL.
    app.view.selected_session = Some("browser-a".into());
    app.sync_browser_tab();
    assert_eq!(app.view.browser.url_draft, address);
    assert_eq!(app.tree.input_value("browser_url"), Some(address));
    assert!(app.view.browser.navigating);
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::Navigate { url }) if url == address)
    );
    commands.set_snapshot(BrowserSnapshot {
        generation: 3,
        started: true,
        url: address.into(),
        ..Default::default()
    });
    app.update_browser(&egui::Context::default());
    assert!(
        commands.finish_next().is_none(),
        "retry must not replay navigation"
    );
    assert_eq!(app.view.browser.url, address);

    app.browser_restart_urls.insert(id.get(), address.into());
    app.dock.close(&scope_a, id).unwrap();
    app.update_browser(&egui::Context::default());
    assert!(!app.browser_restart_urls.contains_key(&id.get()));
}

#[test]
fn snapshot_preserves_focused_address_and_failed_destination_but_updates_confirmed_redirect() {
    let (_root, mut app, id) = fixture();
    let address = "http://localhost:3000/project";
    app.view.browser.url_draft = address.into();
    viewport(&mut app);
    assert!(app.tree.focus("browser_url"));
    app.apply_browser_snapshot(
        id.get(),
        BrowserSnapshot {
            generation: 1,
            url: "https://example.test/redirect".into(),
            started: true,
            ..Default::default()
        },
    );
    assert_eq!(app.view.browser.url_draft, address);

    viewport(&mut app);
    assert!(app.tree.focus("browser_viewport"));
    app.apply_browser_snapshot(
        id.get(),
        BrowserSnapshot {
            generation: 2,
            url: "chrome-error://chromewebdata/".into(),
            error: Some("net::ERR_CONNECTION_REFUSED".into()),
            started: true,
            ..Default::default()
        },
    );
    assert_eq!(
        app.view.browser.url_draft, address,
        "internal error pages must not replace the destination"
    );
    app.apply_browser_snapshot(
        id.get(),
        BrowserSnapshot {
            generation: 3,
            url: "https://example.test/confirmed".into(),
            started: true,
            ..Default::default()
        },
    );
    assert_eq!(app.view.browser.url_draft, "https://example.test/confirmed");
}

#[test]
fn restart_intents_share_protocol_validation_and_exclude_blank_credentials_and_control_characters()
{
    assert_eq!(
        browser_restart_url(" https://example.test/path ").as_deref(),
        Some("https://example.test/path")
    );
    for invalid in [
        "about:blank",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:secret@example.test",
        "https://example.test/\npath",
        "",
    ] {
        assert!(browser_restart_url(invalid).is_none(), "{invalid}");
    }
    assert!(browser_restart_url(&format!("https://example.test/{}", "x".repeat(4096))).is_none());
}

#[test]
fn wheel_trackpad_and_shift_preserve_both_axes_in_css_pixels_and_reject_nonfinite_input() {
    let point = Point::new(80.0, 60.0);
    assert_eq!(
        browser_scroll_delta(&UiEvent::Scroll {
            position: point,
            delta_y: 10.0
        }),
        Some((point, 0.0, 10.0, false))
    );
    assert_eq!(
        browser_scroll_delta(&UiEvent::ScrollDirect {
            position: point,
            delta_y: -2.5
        }),
        Some((point, 0.0, -2.5, true))
    );
    let scroll = |x, y, shift| UiEvent::Scroll2D {
        position: point,
        delta_x: x,
        delta_y: y,
        shift,
        direct: true,
    };
    assert_eq!(
        browser_scroll_delta(&scroll(3.5, -6.0, false)),
        Some((point, 3.5, -6.0, true))
    );
    assert_eq!(
        browser_scroll_delta(&scroll(0.0, 9.0, true)),
        Some((point, 9.0, 0.0, true))
    );
    assert_eq!(
        browser_scroll_delta(&scroll(3.0, 9.0, true)),
        Some((point, 3.0, 9.0, true))
    );
    assert!(browser_scroll_delta(&scroll(f32::NAN, 1.0, false)).is_none());
    assert!(browser_scroll_delta(&scroll(0.0, f32::INFINITY, false)).is_none());

    let (_root, mut app, id) = fixture();
    let snapshot = BrowserSnapshot {
        viewport_width: 640,
        viewport_height: 480,
        ..blank(1)
    };
    let commands = install_runtime(&mut app, id, snapshot.clone());
    app.apply_browser_snapshot(id.get(), snapshot);
    viewport(&mut app);
    assert!(app.handle_browser_event(&scroll(3.5, -6.0, false)));
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::Scroll { dx, dy, .. }) if dx == -3.5 && dy == 6.0)
    );
    assert!(!app.handle_browser_event(&scroll(f32::NAN, 1.0, false)));
    assert!(commands.finish_next().is_none());
}

#[test]
fn paste_and_ime_commit_require_focused_visible_viewport_and_are_consumed_without_dispatch_while_navigating(
) {
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    assert!(app.tree.focus("browser_url"));
    assert!(!app.handle_browser_event(&UiEvent::Paste("URL text".into())));
    assert!(commands.finish_next().is_none());
    assert!(app.tree.focus("browser_viewport"));
    for event in [
        UiEvent::Paste("Niño 🧠".into()),
        UiEvent::ImeCommit("日本語".into()),
    ] {
        assert!(app.handle_browser_event(&event));
        assert!(
            matches!(commands.finish_next(), Some(BrowserCommand::Type { text }) if !text.is_empty())
        );
    }
    for key in [Key::PageUp, Key::PageDown] {
        assert!(app.handle_browser_event(&UiEvent::Key {
            key,
            ctrl: false,
            shift: false
        }));
        assert!(
            matches!(commands.finish_next(), Some(BrowserCommand::Press { key: value }) if value == format!("{key:?}"))
        );
    }
    app.view.browser.navigating = true;
    assert!(app.handle_browser_event(&UiEvent::Paste("Must not enter a new page".into())));
    assert!(app.handle_browser_event(&UiEvent::ImeCommit("未確定".into())));
    assert!(commands.finish_next().is_none());
    app.view.browser.navigating = false;
    app.view.show_side_pane = false;
    assert!(!app.handle_browser_event(&UiEvent::Paste("hidden".into())));
    app.view.show_side_pane = true;
    app.view.show_settings = true;
    assert!(!app.handle_browser_event(&UiEvent::Paste("settings".into())));
    assert!(commands.finish_next().is_none());
}

#[test]
fn twenty_piece_typing_burst_and_scroll_burst_preserve_every_input_and_barrier() {
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    assert!(app.tree.focus("browser_viewport"));
    for index in 0..20 {
        assert!(app.handle_browser_event(&UiEvent::Text(format!("{index}🧠"))));
    }
    assert!(app.handle_browser_event(&UiEvent::Key {
        key: Key::ArrowLeft,
        ctrl: false,
        shift: false
    }));
    for _ in 0..20 {
        assert!(app.handle_browser_event(&UiEvent::Scroll2D {
            position: Point::new(80.0, 60.0),
            delta_x: 1.0,
            delta_y: -2.0,
            direct: true,
            shift: false
        }));
    }
    assert!(app.view.browser.queued_inputs > 0);
    assert!(app.browser_requests.get(&id.get()).unwrap().len() <= 4);
    app.view.show_side_pane = false; // No automatic viewport resize while polling.
    let mut delivered = Vec::new();
    for _ in 0..20 {
        while let Some(command) = commands.finish_next() {
            delivered.push(command);
        }
        app.update_browser(&egui::Context::default());
        if app.browser_requests.is_empty() && app.browser_inputs.is_empty() {
            break;
        }
    }
    let barrier = delivered
        .iter()
        .position(|command| matches!(command, BrowserCommand::Press { key } if key == "ArrowLeft"))
        .unwrap();
    let actual: String = delivered[..barrier]
        .iter()
        .map(|command| match command {
            BrowserCommand::Type { text } => text.as_str(),
            _ => panic!("input order changed"),
        })
        .collect();
    let expected: String = (0..20).map(|index| format!("{index}🧠")).collect();
    assert_eq!(actual, expected);
    let (x, y) =
        delivered[barrier + 1..]
            .iter()
            .fold((0.0, 0.0), |(x, y), command| match command {
                BrowserCommand::Scroll { dx, dy, .. } => (x + dx, y + dy),
                _ => panic!("scroll barrier changed"),
            });
    assert_eq!((x, y), (-20.0, 40.0));
    assert!(
        delivered.len() < 10,
        "adjacent input is batched, not one screenshot per character"
    );
    assert_eq!(app.view.browser.queued_inputs, 0);
}

#[test]
fn pointer_during_pending_typing_is_an_ordered_barrier_but_navigation_blocks_it() {
    let (_root, mut app, id) = fixture();
    let snapshot = BrowserSnapshot {
        loading: true,
        ..blank(1)
    };
    let commands = install_runtime(&mut app, id, snapshot.clone());
    app.apply_browser_snapshot(id.get(), snapshot);
    viewport(&mut app);
    assert!(app.tree.focus("browser_viewport"));
    for index in 0..5 {
        assert!(app.handle_browser_event(&UiEvent::Text(index.to_string())));
    }
    assert!(app.view.browser.loading);
    assert_eq!(app.browser_requests.get(&id.get()).unwrap().len(), 4);
    let rect = app.browser_viewport_rect().unwrap();
    let click = UiEvent::PointerDown(Point::new(rect.x + 40.0, rect.y + 30.0));
    assert!(app.handle_browser_event(&click));
    assert!(app.handle_browser_event(&UiEvent::Text("después".into())));
    assert_eq!(app.browser_inputs.get(&id.get()).unwrap().len(), 3);

    app.view.browser.navigating = true;
    assert!(app.handle_browser_event(&click));
    assert_eq!(
        app.browser_inputs.get(&id.get()).unwrap().len(),
        3,
        "a navigation blocks clicks against the outgoing document"
    );
    app.view.show_side_pane = false;
    let mut delivered = Vec::new();
    for _ in 0..8 {
        while let Some(command) = commands.finish_next() {
            delivered.push(command);
        }
        app.update_browser(&egui::Context::default());
        if app.browser_requests.is_empty() && app.browser_inputs.is_empty() {
            break;
        }
    }
    // The button is no longer down: the page is also told of the release.
    assert_eq!(delivered.len(), 8);
    assert!(matches!(delivered[7], BrowserCommand::PointerUp { x, y, .. } if x == 40.0 && y == 30.0));
    for (index, command) in delivered[..5].iter().enumerate() {
        assert!(matches!(command, BrowserCommand::Press { key } if key == &index.to_string()));
    }
    assert!(matches!(delivered[5], BrowserCommand::PointerDown { x, y, .. } if x == 40.0 && y == 30.0));
    assert!(matches!(&delivered[6], BrowserCommand::Type { text } if text == "después"));
}

#[test]
fn native_copy_and_cut_events_use_scoped_clipboard_and_cut_confirmation_before_following_input() {
    use crate::browser_runtime::BrowserClipboard;
    let (_root, mut app, id) = fixture();
    // This fixture's surface is 320×240. Matching the actor avoids a
    // resize command interleaving with the clipboard transaction under test.
    let snapshot = BrowserSnapshot {
        viewport_width: 320,
        viewport_height: 240,
        ..blank(1)
    };
    let commands = install_runtime(&mut app, id, snapshot.clone());
    app.apply_browser_snapshot(id.get(), snapshot);
    viewport(&mut app);
    assert!(app.tree.focus("browser_viewport"));
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Copy],
            ..Default::default()
        },
        |ui| {
            let events = crate::render::events(ui.ctx(), egui::Pos2::ZERO);
            assert_eq!(events.len(), 1);
            assert!(app.handle_browser_event(&events[0]));
        },
    );
    output.textures_delta.clear();
    assert!(matches!(
        commands.finish_next_with_clipboard(Some(BrowserClipboard {
            text: "Copiado á".into(),
            cut_token: None
        })),
        Some(BrowserCommand::CopySelection)
    ));
    let mut output = ctx.run_ui(Default::default(), |ui| {
        app.update_browser(ui.ctx());
    });
    output.textures_delta.clear();
    assert!(output.platform_output.commands.iter().any(
        |command| matches!(command, egui::OutputCommand::CopyText(text) if text == "Copiado á")
    ));

    viewport(&mut app);
    app.tree.focus("browser_viewport");
    let mut output = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Cut],
            ..Default::default()
        },
        |ui| {
            let events = crate::render::events(ui.ctx(), egui::Pos2::ZERO);
            assert_eq!(events.len(), 1);
            assert!(app.handle_browser_event(&events[0]));
        },
    );
    output.textures_delta.clear();
    assert!(app.handle_browser_event(&UiEvent::Text("después".into())));
    assert!(matches!(
        commands.finish_next_with_clipboard(Some(BrowserClipboard {
            text: "Cortado 🧠".into(),
            cut_token: Some("fixture-token".into())
        })),
        Some(BrowserCommand::PrepareCut)
    ));
    assert!(
        commands.finish_next().is_none(),
        "typing waits for cut confirmation"
    );
    let mut output = ctx.run_ui(Default::default(), |ui| {
        app.update_browser(ui.ctx());
    });
    output.textures_delta.clear();
    assert!(output.platform_output.commands.iter().any(
        |command| matches!(command, egui::OutputCommand::CopyText(text) if text == "Cortado 🧠")
    ));
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::ConfirmCut { token }) if token == "fixture-token")
    );
    assert!(commands.finish_next().is_none());
    app.update_browser(&ctx);
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::Type { text }) if text == "después")
    );
}

#[test]
fn changing_conversation_before_cut_reply_neither_copies_nor_confirms_deletion() {
    use crate::browser_runtime::BrowserClipboard;
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    app.tree.focus("browser_viewport");
    assert!(app.handle_browser_event(&UiEvent::Key {
        key: Key::X,
        ctrl: true,
        shift: false
    }));
    assert!(matches!(
        commands.finish_next_with_clipboard(Some(BrowserClipboard {
            text: "original".into(),
            cut_token: Some("cancelled-token".into())
        })),
        Some(BrowserCommand::PrepareCut)
    ));
    app.view.selected_session = Some("other-conversation".into());
    app.view.show_side_pane = false;
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        app.update_browser(ui.ctx());
    });
    output.textures_delta.clear();
    assert!(!output
        .platform_output
        .commands
        .iter()
        .any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
    assert!(
        commands.finish_next().is_none(),
        "no destructive confirmation crosses conversation scope"
    );
}

#[test]
fn hover_reaches_the_page_one_move_at_a_time_and_a_drag_ends_wherever_it_is_released() {
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    let rect = app.browser_viewport_rect().unwrap();
    let at = |x: f32, y: f32| Point::new(rect.x + x, rect.y + y);
    for step in 0..10 {
        // The tree keeps the move: it owns hover around the page.
        assert!(!app.handle_browser_event(&UiEvent::PointerMove(at(10.0 + step as f32, 20.0))));
    }
    // One move is in flight and only the newest waits. Neither is work the
    // footer reports, and their replies do not rebuild the window.
    assert_eq!(app.browser_requests.get(&id.get()).unwrap().len(), 1);
    assert_eq!(app.browser_inputs.get(&id.get()).unwrap().len(), 1);
    assert_eq!(app.view.browser.queued_inputs, 0);
    assert!(!app.view.browser.loading);
    app.view.show_side_pane = false; // No automatic viewport resize while polling.
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::PointerMove { x, y, .. }) if x == 10.0 && y == 20.0)
    );
    assert!(!app.update_browser(&egui::Context::default()));
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::PointerMove { x, y, .. }) if x == 19.0 && y == 20.0)
    );
    assert!(!app.update_browser(&egui::Context::default()));
    assert!(commands.finish_next().is_none());

    // A press, a drag that leaves the page and a release outside it: the page
    // gets the drag on its nearest edge and is always told of the release.
    app.view.show_side_pane = true;
    assert!(app.handle_browser_event(&UiEvent::PointerDown(at(5.0, 6.0))));
    assert!(!app.handle_browser_event(&UiEvent::PointerMove(at(-50.0, 900.0))));
    assert!(app.handle_browser_event(&UiEvent::PointerUp(at(-50.0, 900.0))));
    assert!(!app.handle_browser_event(&UiEvent::PointerMove(at(-50.0, 900.0))));
    app.view.show_side_pane = false;
    let mut delivered = Vec::new();
    for _ in 0..8 {
        while let Some(command) = commands.finish_next() {
            delivered.push(command);
        }
        app.update_browser(&egui::Context::default());
        if app.browser_requests.is_empty() && app.browser_inputs.is_empty() {
            break;
        }
    }
    assert!(
        matches!(delivered.as_slice(), [
            BrowserCommand::PointerDown { x, y, .. },
            BrowserCommand::PointerMove { x: move_x, y: move_y, .. },
            BrowserCommand::PointerUp { x: up_x, y: up_y, .. },
        ] if (*x, *y, *move_x, *move_y, *up_x, *up_y) == (5.0, 6.0, 0.0, 479.0, 0.0, 479.0)),
        "{delivered:?}"
    );
}

#[test]
fn a_tab_is_a_page_only_while_its_surface_holds_a_frame_of_a_started_context() {
    let (_root, mut app, id) = fixture();
    let _commands = install_runtime(&mut app, id, BrowserSnapshot {
        url: "http://localhost:3000/".into(),
        ..blank(1)
    });
    // Started, but nothing was presented yet: there is no page to click.
    assert!(app.update_browser(&egui::Context::default()));
    assert!(!app.view.browser.live);
    app.browser_live.insert(id.get(), true);
    assert!(app.update_browser(&egui::Context::default()));
    assert!(app.view.browser.live);
    // A context that is gone takes its last frame with it.
    app.browsers.remove(&id.get());
    app.update_browser(&egui::Context::default());
    assert!(!app.browser_live.contains_key(&id.get()));
}

#[test]
fn a_context_started_for_another_scale_starts_again_at_its_address() {
    let (_root, mut app, id) = fixture();
    let _commands = install_runtime(&mut app, id, BrowserSnapshot {
        url: "http://localhost:3000/docs".into(),
        ..blank(1)
    });
    // The fixture context was started for scale 1, and the window has it.
    let ctx = egui::Context::default();
    app.update_browser(&ctx);
    assert!(app.browsers.contains_key(&id.get()));
    assert!(!app.browser_restart_urls.contains_key(&id.get()));
    // The window turns out to be on a denser display.
    ctx.set_pixels_per_point(2.0);
    ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    assert_eq!(ctx.pixels_per_point(), 2.0);
    assert!(app.update_browser(&ctx));
    assert!(!app.browsers.contains_key(&id.get()), "a page at the old scale would stay blurred");
    assert_eq!(app.browser_restart_urls.get(&id.get()).map(String::as_str), Some("http://localhost:3000/docs"));
}

#[test]
fn a_page_shown_again_takes_no_input_until_its_stream_is_back_and_a_press_is_always_released() {
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    app.sync_browser_tab();
    let rect = app.browser_viewport_rect().unwrap();
    let inside = Point::new(rect.x + 30.0, rect.y + 40.0);
    assert!(app.handle_browser_event(&UiEvent::PointerDown(inside)));
    assert!(matches!(commands.finish_next(), Some(BrowserCommand::PointerDown { .. })));

    // The window shows something else by the time the button comes up: the
    // page still hears of it, where the pointer last was on it.
    app.view.show_settings = true;
    app.tree.set_root(Element::spacer());
    app.tree.layout(Size::new(320.0, 272.0));
    assert!(app.handle_browser_event(&UiEvent::PointerUp(Point::new(-400.0, -400.0))));
    assert!(
        matches!(commands.finish_next(), Some(BrowserCommand::PointerUp { x, y, .. }) if x == 30.0 && y == 40.0)
    );
    assert!(app.browser_pointer_down.is_none());

    // Hidden, the stream stops. Back, the frame from before still shows but
    // is not a page to click until a frame of the new stream replaces it.
    viewport(&mut app);
    app.sync_browser_tab();
    app.view.show_side_pane = false;
    app.sync_browser_tab();
    app.view.show_side_pane = true;
    app.sync_browser_tab();
    assert_eq!(app.browser_live.get(&id.get()), Some(&false));
    assert!(app.view.browser.live);
    for event in [UiEvent::PointerDown(inside), UiEvent::PointerMove(inside)] {
        assert!(!app.handle_browser_event(&event));
    }
    assert!(commands.finish_next().is_none());
    app.browser_live.insert(id.get(), true);
    assert!(app.handle_browser_event(&UiEvent::PointerDown(inside)));
    assert!(matches!(commands.finish_next(), Some(BrowserCommand::PointerDown { .. })));
}

#[test]
fn presses_close_in_time_and_place_are_one_run_of_clicks_and_other_buttons_click_whole() {
    use openide_browser::MouseButton;
    let (_root, mut app, id) = fixture();
    // The page already has the size of its pane, so no resize joins the queue.
    let page = BrowserSnapshot {
        viewport_width: 320,
        viewport_height: 240,
        ..blank(1)
    };
    let commands = install_runtime(&mut app, id, page.clone());
    app.apply_browser_snapshot(id.get(), page);
    viewport(&mut app);
    app.sync_browser_tab();
    let rect = app.browser_viewport_rect().unwrap();
    let at = |x: f32, y: f32| Point::new(rect.x + x, rect.y + y);
    let ctx = egui::Context::default();
    // The clicks the page is told a press and its release are.
    let click = |app: &mut AgentsWindow, press: UiEvent, release: Point| -> (u8, u8) {
        assert!(app.handle_browser_event(&press));
        let Some(BrowserCommand::PointerDown { button: MouseButton::Left, clicks: down, .. }) = commands.finish_next() else {
            panic!("a left press reaches the page");
        };
        assert!(app.handle_browser_event(&UiEvent::PointerUp(release)));
        let Some(BrowserCommand::PointerUp { button: MouseButton::Left, clicks: up, .. }) = commands.finish_next() else {
            panic!("and so does its release");
        };
        app.update_browser(&ctx);
        (down, up)
    };
    let spot = at(30.0, 40.0);
    assert_eq!(click(&mut app, UiEvent::PointerDown(spot), spot), (1, 1));
    assert_eq!(click(&mut app, UiEvent::PointerDown(spot), spot), (2, 2));
    // The window may name a press otherwise once it has seen a double
    // click: it counts for the page all the same, a pixel or two away.
    let editor = UiEvent::PointerDownEditor {
        position: at(32.0, 41.0),
        shift: false,
        alt: false,
        ctrl: false,
        click_count: 1,
    };
    assert_eq!(click(&mut app, editor, at(32.0, 41.0)), (3, 3));
    assert_eq!(click(&mut app, UiEvent::PointerDown(spot), spot), (3, 3), "a run stays at three");
    // A release far from its press, after a drag, is still that press's.
    let far = at(80.0, 40.0);
    assert_eq!(click(&mut app, UiEvent::PointerDown(far), at(200.0, 90.0)), (1, 1), "elsewhere a run starts over");
    assert_eq!(click(&mut app, UiEvent::PointerDown(far), far), (2, 2));
    let (tab, _, x, y, count) = app.browser_click.unwrap();
    let long_ago = Instant::now() - BROWSER_MULTI_CLICK - Duration::from_millis(50);
    app.browser_click = Some((tab, long_ago, x, y, count));
    assert_eq!(click(&mut app, UiEvent::PointerDown(far), far), (1, 1), "and so it does later");

    // The secondary button: the page gets the whole click at once, and no
    // release is owed.
    assert!(app.handle_browser_event(&UiEvent::ContextMenu { position: spot }));
    assert!(matches!(
        commands.finish_next(),
        Some(BrowserCommand::PointerDown { x, y, button: MouseButton::Right, clicks: 1, .. }) if (x, y) == (30.0, 40.0)
    ));
    assert!(matches!(
        commands.finish_next(),
        Some(BrowserCommand::PointerUp { x, y, button: MouseButton::Right, clicks: 1, .. }) if (x, y) == (30.0, 40.0)
    ));
    assert!(app.browser_pointer_down.is_none());
    app.update_browser(&ctx);

    // The middle button, which the window's own events do not carry.
    let origin = egui::pos2(100.0, 50.0);
    let middle = |position: Point| egui::RawInput {
        events: vec![egui::Event::PointerButton {
            pos: egui::pos2(origin.x + position.x, origin.y + position.y),
            button: egui::PointerButton::Middle,
            pressed: false,
            modifiers: Default::default(),
        }],
        ..Default::default()
    };
    ctx.begin_pass(middle(spot));
    assert!(app.handle_browser_middle_click(&ctx, origin));
    ctx.end_pass().textures_delta.clear();
    assert!(matches!(
        commands.finish_next(),
        Some(BrowserCommand::PointerDown { x, y, button: MouseButton::Middle, clicks: 1, .. }) if (x, y) == (30.0, 40.0)
    ));
    assert!(matches!(commands.finish_next(), Some(BrowserCommand::PointerUp { button: MouseButton::Middle, .. })));
    app.update_browser(&ctx);

    // Outside the page nothing is the page's, and while it is between two
    // documents a click over it is swallowed.
    let toolbar = Point::new(rect.x + 30.0, rect.y - 10.0);
    assert!(!app.handle_browser_event(&UiEvent::ContextMenu { position: toolbar }));
    ctx.begin_pass(middle(toolbar));
    assert!(!app.handle_browser_middle_click(&ctx, origin));
    ctx.end_pass().textures_delta.clear();
    app.view.browser.navigating = true;
    assert!(app.handle_browser_event(&UiEvent::ContextMenu { position: spot }));
    ctx.begin_pass(middle(spot));
    assert!(app.handle_browser_middle_click(&ctx, origin));
    ctx.end_pass().textures_delta.clear();
    assert!(commands.finish_next().is_none());
}

#[test]
fn held_modifier_keys_travel_with_the_pointer_and_the_wheel_and_control_with_the_wheel_moves_nothing() {
    use openide_browser::modifiers::{ALT, CONTROL, SHIFT};
    use openide_browser::MouseButton;
    let held = |shift, ctrl, alt| browser_modifiers(egui::Modifiers { shift, ctrl, alt, ..Default::default() });
    assert_eq!(held(false, false, false), 0);
    assert_eq!((held(true, false, false), held(false, true, false), held(false, false, true)), (SHIFT, CONTROL, ALT));
    assert_eq!(held(true, true, true), SHIFT | CONTROL | ALT);

    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    app.view.show_side_pane = true;
    let ctx = egui::Context::default();
    let rect = app.browser_viewport_rect().unwrap();
    let at = |x: f32, y: f32| Point::new(rect.x + x, rect.y + y);
    let delivered = |app: &mut AgentsWindow| {
        app.view.show_side_pane = false; // No automatic viewport resize while polling.
        let mut delivered = Vec::new();
        for _ in 0..8 {
            while let Some(command) = commands.finish_next() {
                delivered.push(command);
            }
            app.update_browser(&ctx);
        }
        app.view.show_side_pane = true;
        delivered
    };

    // Shift + click extends a selection: the press, the drag and the release
    // all say which keys were held.
    app.browser_modifiers = SHIFT;
    assert!(app.handle_browser_event(&UiEvent::PointerDownModified { position: at(30.0, 40.0), shift: true }));
    assert!(!app.handle_browser_event(&UiEvent::PointerMove(at(50.0, 40.0))));
    assert!(app.handle_browser_event(&UiEvent::PointerUp(at(50.0, 40.0))));
    assert!(
        matches!(delivered(&mut app).as_slice(), [
            BrowserCommand::PointerDown { modifiers: SHIFT, .. },
            BrowserCommand::PointerMove { modifiers: SHIFT, .. },
            BrowserCommand::PointerUp { modifiers: SHIFT, .. },
        ])
    );
    // Control + click, which the window reports as an editor gesture, and
    // the right button.
    app.browser_modifiers = CONTROL | ALT;
    let editor = UiEvent::PointerDownEditor { position: at(30.0, 40.0), shift: false, alt: true, ctrl: true, click_count: 1 };
    assert!(app.handle_browser_event(&editor));
    assert!(app.handle_browser_event(&UiEvent::PointerUp(at(30.0, 40.0))));
    assert!(app.handle_browser_event(&UiEvent::ContextMenu { position: at(30.0, 40.0) }));
    let both = CONTROL | ALT;
    assert!(
        matches!(delivered(&mut app).as_slice(), [
            BrowserCommand::PointerDown { button: MouseButton::Left, modifiers: left_down, .. },
            BrowserCommand::PointerUp { button: MouseButton::Left, modifiers: left_up, .. },
            BrowserCommand::PointerDown { button: MouseButton::Right, modifiers: right_down, .. },
            BrowserCommand::PointerUp { button: MouseButton::Right, modifiers: right_up, .. },
        ] if [*left_down, *left_up, *right_down, *right_up] == [both; 4])
    );

    // Shift turns the wheel sideways and the page is told Shift was held.
    app.browser_modifiers = SHIFT;
    let wheel = |delta_y: f32, direct: bool| UiEvent::Scroll2D { position: at(30.0, 40.0), delta_x: 0.0, delta_y, direct, shift: true };
    assert!(app.handle_browser_event(&wheel(-9.0, true)));
    assert!(
        matches!(delivered(&mut app).as_slice(), [BrowserCommand::Scroll { dx, dy, modifiers: SHIFT }] if (*dx, *dy) == (9.0, 0.0))
    );

    // Control with the wheel zooms a browser. This page cannot be zoomed, and
    // is not scrolled instead: neither a touchpad's deltas nor a wheel notch
    // reach it, and no notch is left gliding.
    app.browser_modifiers = CONTROL;
    for event in [
        UiEvent::ScrollDirect { position: at(30.0, 40.0), delta_y: -12.0 },
        UiEvent::Scroll { position: at(30.0, 40.0), delta_y: -30.0 },
    ] {
        assert!(app.handle_browser_event(&event), "taken, so nothing behind the page scrolls either");
    }
    assert!(delivered(&mut app).is_empty());
    // Control pressed while a notch still glides: what is left of it scrolls.
    app.browser_modifiers = 0;
    assert!(app.handle_browser_event(&UiEvent::Scroll { position: at(30.0, 40.0), delta_y: -30.0 }));
    app.browser_modifiers = CONTROL;
    std::thread::sleep(Duration::from_millis(30));
    let glided = delivered(&mut app);
    assert!(!glided.is_empty() && glided.iter().all(|command| matches!(command, BrowserCommand::Scroll { modifiers: 0, .. })), "{glided:?}");
}

#[test]
fn a_typed_character_is_a_key_the_page_hears_and_other_text_is_inserted() {
    let (_root, mut app, id) = fixture();
    let commands = install_runtime(&mut app, id, blank(1));
    app.apply_browser_snapshot(id.get(), blank(1));
    viewport(&mut app);
    assert!(app.tree.focus("browser_viewport"));
    let ctx = egui::Context::default();
    let sent = |app: &mut AgentsWindow, event: UiEvent| {
        assert!(app.handle_browser_event(&event), "{event:?}");
        let command = commands.finish_next().unwrap_or_else(|| panic!("{event:?} sent nothing"));
        app.view.show_side_pane = false; // No automatic viewport resize while polling.
        app.update_browser(&ctx);
        app.view.show_side_pane = true;
        command
    };
    let key = |key, shift, ctrl| UiEvent::Key { key, shift, ctrl };
    // What the US layout types goes down and up as its key: a page's own
    // shortcuts, a list's type-ahead and Space on a button all need that.
    for typed in ["a", "Z", "7", " ", "/", "?", "+", "~"] {
        let command = sent(&mut app, UiEvent::Text(typed.into()));
        assert!(matches!(&command, BrowserCommand::Press { key } if key == typed), "{typed:?}: {command:?}");
        assert!(command.validate().is_ok());
    }
    // What it does not is inserted, as an accent's dead key, an input method
    // and the clipboard deliver theirs.
    for event in [
        UiEvent::Text("ñ".into()),
        UiEvent::Text("€".into()),
        UiEvent::Text("🧠".into()),
        UiEvent::Text("ab".into()),
        UiEvent::ImeCommit("á".into()),
        UiEvent::ImeCommit("a".into()),
        UiEvent::Paste("a".into()),
    ] {
        let command = sent(&mut app, event.clone());
        assert!(matches!(command, BrowserCommand::Type { .. }), "{event:?}: {command:?}");
    }
    // A shortcut names its letter as the keyboard types it, so a page that
    // listens for Control + z hears `z`.
    for (event, expected) in [
        (key(Key::Z, false, true), "Control+z"),
        (key(Key::Z, true, true), "Control+Shift+z"),
        (key(Key::A, false, true), "Control+a"),
        (key(Key::F5, false, true), "Control+F5"),
        (key(Key::ArrowLeft, true, false), "Shift+ArrowLeft"),
        (key(Key::Home, true, true), "Control+Shift+Home"),
        (key(Key::Delete, false, false), "Delete"),
        (key(Key::Tab, true, false), "Shift+Tab"),
    ] {
        let command = sent(&mut app, event);
        assert!(matches!(&command, BrowserCommand::Press { key } if key == expected), "{expected}: {command:?}");
    }
    // The key of a character arrives with its text and is not sent twice.
    assert!(app.handle_browser_event(&key(Key::A, false, false)));
    assert!(commands.finish_next().is_none());
}
