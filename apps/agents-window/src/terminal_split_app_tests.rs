//! Split integration exercises the actual input router and independent PTYs.
use super::*;
use openide_ui::{Key, Point, Size};

fn present(app: &mut AgentsWindow, width: f32) {
    app.tree
        .set_root(crate::terminal_dock_ui::build(&app.view, width, 300.0));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 300.0));
}

fn point(app: &AgentsWindow, id: u64) -> Point {
    let screen = app.terminal_screen_id(id);
    let rect = app
        .tree
        .frame()
        .regions
        .iter()
        .find(|region| region.id == screen)
        .unwrap()
        .rect;
    Point::new(rect.x + 24.0, rect.y + 30.0)
}

fn select_pane(app: &mut AgentsWindow, id: u64, ctx: &egui::Context) {
    let point = point(app, id);
    assert!(app.handle_terminal_event(&UiEvent::PointerDown(point), ctx));
    app.handle_terminal_event(&UiEvent::PointerUp(point), ctx);
    assert_eq!(app.active_terminal_id(), Some(id));
    assert_eq!(
        app.tree.focused_id(),
        Some(app.terminal_screen_id(id).as_str())
    );
}

#[test]
fn terminal_split_errors_are_independent_and_group_follows_verified_draft_promotion() {
    let (_root, mut app) = super::tests::fixture();
    app.view.selected_session = None;
    super::tests::click(&mut app, "terminal_new");
    let first = app.active_terminal_id().unwrap();
    super::tests::click(&mut app, "terminal_split");
    let second = app.active_terminal_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(app.visible_terminal_ids(), vec![first, second]);
    assert_eq!(app.view.terminal_controls.panes.len(), 2);
    assert!(app
        .view
        .terminal_controls
        .panes
        .iter()
        .all(|pane| pane.error.is_some()));
    assert!(app.terminals.is_empty());
    let first_error = app.terminal_controls_host.errors.get(&first).cloned();
    super::tests::click(&mut app, format!("terminal_restart:{second}"));
    assert_eq!(
        app.terminal_controls_host.errors.get(&first),
        first_error.as_ref()
    );
    assert!(
        !app.ensure_terminal_session(first),
        "failed launch does not repeat each frame"
    );

    app.dock.promote_draft(&app.view.workspace, "promoted");
    app.view.selected_session = Some("promoted".into());
    app.sync_dock_view();
    assert_eq!(app.visible_terminal_ids(), vec![first, second]);
    app.view.selected_session = Some("foreign".into());
    app.sync_dock_view();
    super::tests::click(&mut app, format!("terminal_restart:{first}"));
    assert!(app.visible_terminal_ids().is_empty());
    assert!(app.view.terminal_controls.panes.is_empty());
    app.view.selected_session = Some("promoted".into());
    app.sync_dock_view();
    assert_eq!(app.visible_terminal_ids(), vec![first, second]);
    super::tests::click(&mut app, format!("terminal_close:{second}"));
    assert_eq!(app.visible_terminal_ids(), vec![first]);
    assert!(app.view.terminal_controls.panes.is_empty());
    assert!(!app.terminal_controls_host.errors.contains_key(&second));
}

#[cfg(unix)]
#[test]
fn terminal_split_routes_input_and_clipboard_by_pane_and_preserves_processes_on_hide_resize_unsplit(
) {
    let (root, mut app) = super::tests::fixture();
    super::tests::click(&mut app, "terminal_new");
    let first = app.active_terminal_id().unwrap();
    super::tests::click(&mut app, "terminal_split");
    let second = app.active_terminal_id().unwrap();
    // A controlled shell loop, with no interactive/login rc files. Only these
    // synthetic commands touch the two temporary child environments.
    for id in [first, second] {
        let session = TerminalSession::spawn_with_command(
            root.path(),
            60,
            12,
            std::ffi::OsStr::new("sh"),
            &[
                "-c".into(),
                r#"printf '\033[2J\033[HREADY\n'; while IFS= read -r cmd; do eval "$cmd"; done"#
                    .into(),
            ],
        )
        .unwrap();
        app.terminals.insert(id, session);
        app.terminal_controls_host.errors.remove(&id);
    }
    app.sync_dock_view();
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(Default::default());
    present(&mut app, 1000.0);
    app.update_terminal(&ctx);
    select_pane(&mut app, first, &ctx);
    assert!(app.handle_terminal_event(&UiEvent::Text("SPLIT_TAG=one".into()), &ctx));
    assert!(app.handle_terminal_event(
        &UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false
        },
        &ctx
    ));
    select_pane(&mut app, second, &ctx);
    assert!(app.handle_terminal_event(&UiEvent::Text("SPLIT_TAG=two".into()), &ctx));
    assert!(app.handle_terminal_event(
        &UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false
        },
        &ctx
    ));
    for id in [first, second] {
        select_pane(&mut app, id, &ctx);
        app.handle_terminal_event(
            &UiEvent::Text("printf 'TAG:%s PID:%s\\n' \"$SPLIT_TAG\" \"$$\"".into()),
            &ctx,
        );
        app.handle_terminal_event(
            &UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            },
            &ctx,
        );
    }
    let output =
        |app: &AgentsWindow, id| app.terminals.get(&id).unwrap().snapshot().lines.join("\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        app.update_terminal(&ctx);
        if output(&app, first).contains("TAG:one PID:")
            && output(&app, second).contains("TAG:two PID:")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let first_output = output(&app, first);
    let second_output = output(&app, second);
    assert!(first_output.contains("TAG:one PID:"), "{first_output}");
    assert!(second_output.contains("TAG:two PID:"), "{second_output}");
    assert!(!first_output.contains("TAG:two PID:"));
    assert!(!second_output.contains("TAG:one PID:"));
    let pid = |text: &str| {
        text.lines()
            .find_map(|line| {
                line.split_once(" PID:")
                    .and_then(|(_, value)| value.trim().parse::<u32>().ok())
            })
            .unwrap()
    };
    let first_pid = pid(&first_output);
    let second_pid = pid(&second_output);
    assert_ne!(first_pid, second_pid);

    // An asynchronous clipboard response belongs to the original focused PTY.
    select_pane(&mut app, first, &ctx);
    app.request_text_paste(app.active_terminal_screen_id(), &ctx);
    assert!(app.pending_text_paste.is_some());
    select_pane(&mut app, second, &ctx);
    assert!(app
        .route_text_paste(UiEvent::Paste("must-not-enter-other-shell".into()), false)
        .is_none());

    let columns = app.terminals.get(&first).unwrap().snapshot().cols;
    present(&mut app, 620.0);
    app.update_terminal(&ctx);
    assert!(app.terminals.get(&first).unwrap().snapshot().cols < columns);
    assert_eq!(
        app.terminals.get(&first).unwrap().snapshot().cols,
        app.terminals.get(&second).unwrap().snapshot().cols
    );
    super::tests::click(&mut app, "terminal_panel_hide");
    app.update_terminal(&ctx);
    assert!(app
        .terminals
        .values()
        .all(|session| session.snapshot().running));
    super::tests::click(&mut app, "terminal_toggle");
    assert_eq!(app.visible_terminal_ids(), vec![first, second]);
    super::tests::click(&mut app, "terminal_unsplit");
    assert!(app.view.terminal_controls.panes.is_empty());
    assert_eq!(app.view.terminal_tabs.len(), 2);
    assert_eq!(app.terminals.len(), 2);
    present(&mut app, 1000.0);
    for (id, expected) in [(first, first_pid), (second, second_pid)] {
        super::tests::click(&mut app, format!("terminal_tab:{id}"));
        assert_eq!(pid(&output(&app, id)), expected);
    }
    super::tests::click(&mut app, format!("terminal_close:{second}"));
    assert!(app.terminals.get(&first).unwrap().snapshot().running);
    assert!(!app.terminals.contains_key(&second));
    ctx.end_pass().textures_delta.clear();
}
