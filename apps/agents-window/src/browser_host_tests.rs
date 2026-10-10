use super::*;
use crate::browser_runtime::{
    tests::BrowserCommands, BrowserCommand, BrowserScope, BrowserSnapshot,
};
use openide_ui::{Element, Key, Point, RasterImage, RasterImageFormat, Size};

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
    // The native renderer is not invoked here. This is only the immutable
    // image identity used to verify focus and event dispatch at the host.
    let image = RasterImage::new(
        "a".repeat(64),
        RasterImageFormat::Png,
        3,
        Arc::from("AAAA"),
        "Fixture viewport",
    )
    .unwrap();
    app.view.browser.image = Some(image.clone());
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
            Element::raster_image(image).size(Some(320.0), Some(240.0)),
        )
        .size(Some(320.0), Some(240.0)),
    ]));
    app.tree.layout(Size::new(320.0, 272.0));
}

#[test]
fn browser_dimensions_fallback_matches_the_rendered_viewport_in_both_window_modes() {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    // A valid in-memory image makes this the real Page surface without a
    // browser process, server, or terminal actor behind the UI fixture.
    let bytes = resvg::tiny_skia::Pixmap::new(2, 2)
        .unwrap()
        .encode_png()
        .unwrap();
    let image = RasterImage::new(
        format!("{:x}", Sha256::digest(&bytes)),
        RasterImageFormat::Png,
        bytes.len() as u32,
        Arc::from(base64::engine::general_purpose::STANDARD.encode(bytes)),
        "Browser viewport geometry fixture",
    )
    .unwrap();
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

                app.view.browser.image = Some(image.clone());
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
fn wheel_trackpad_shift_and_scaled_viewport_preserve_both_axes_and_reject_nonfinite_input() {
    let point = Point::new(80.0, 60.0);
    assert_eq!(
        browser_scroll_delta(&UiEvent::Scroll {
            position: point,
            delta_y: 10.0
        }),
        Some((point, 0.0, 10.0))
    );
    assert_eq!(
        browser_scroll_delta(&UiEvent::ScrollDirect {
            position: point,
            delta_y: -2.5
        }),
        Some((point, 0.0, -2.5))
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
        Some((point, 3.5, -6.0))
    );
    assert_eq!(
        browser_scroll_delta(&scroll(0.0, 9.0, true)),
        Some((point, 9.0, 0.0))
    );
    assert_eq!(
        browser_scroll_delta(&scroll(3.0, 9.0, true)),
        Some((point, 3.0, 9.0))
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
        matches!(commands.finish_next(), Some(BrowserCommand::Scroll { dx, dy }) if dx == -7.0 && dy == 12.0)
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
                BrowserCommand::Scroll { dx, dy } => (x + dx, y + dy),
                _ => panic!("scroll barrier changed"),
            });
    assert_eq!((x, y), (-40.0, 80.0));
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
        screenshot: Some(Arc::new(crate::browser_runtime::BrowserScreenshot {
            base64: Arc::from("AAAA"),
            sha256: "a".repeat(64),
            size_bytes: 3,
            width: 640,
            height: 480,
        })),
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
    assert_eq!(delivered.len(), 7);
    for (index, command) in delivered[..5].iter().enumerate() {
        assert!(matches!(command, BrowserCommand::Type { text } if text == &index.to_string()));
    }
    assert!(matches!(delivered[5], BrowserCommand::Click { x, y } if x == 80.0 && y == 60.0));
    assert!(matches!(&delivered[6], BrowserCommand::Type { text } if text == "después"));
}

#[test]
fn native_copy_and_cut_events_use_scoped_clipboard_and_cut_confirmation_before_following_input() {
    use crate::browser_runtime::BrowserClipboard;
    let (_root, mut app, id) = fixture();
    // This fixture's native raster is 320×240. Matching the actor avoids a
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
