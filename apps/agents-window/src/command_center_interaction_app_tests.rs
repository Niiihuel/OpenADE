use super::*;
use openide_ui::{Frame, Key, Point};

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
    app.view.workspace = directory.path().display().to_string();
    app.view.show_settings = false;
    app.view.main_page = MainPage::Workspace;
    app.view.show_command_center = true;
    app.view.command_scope = Scope::All;
    app.view.command_query.clear();
    app.command_tree.set_reduced_motion(true);
    app.set_command_results(
        (0..36)
            .map(|index| command_center::ResultItem {
                target: if index == 0 {
                    Target::OpenSettings
                } else {
                    Target::OpenSession(format!("session-{index}"))
                },
                section: match index / 12 {
                    0 => Scope::Commands,
                    1 => Scope::Sessions,
                    _ => Scope::Files,
                },
                title: format!("Resultado {index}"),
                subtitle: format!("proyecto/ruta-{index}"),
            })
            .collect(),
    );
    app.command_focus_pending = true;
    (directory, app)
}

fn layout(app: &mut AgentsWindow, size: Size) -> Frame {
    app.command_tree
        .set_root(build_command_center(&app.view, size.width, size.height));
    let mut frame = app.command_tree.layout(size);
    if app.command_focus_pending {
        assert!(app.command_tree.focus("command_query"));
        app.command_focus_pending = false;
        frame = app.command_tree.layout(size);
    }
    frame
}

fn result_center(frame: &Frame, index: usize) -> Point {
    let id = format!("command_result:{index}");
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap()
        .rect;
    Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

#[test]
fn keyboard_navigation_reveals_results_with_section_headers_without_stealing_query_focus() {
    for size in [Size::new(300.0, 260.0), Size::new(560.0, 500.0)] {
        let (_directory, mut app) = fixture();
        layout(&mut app, size);
        for _ in 0..35 {
            app.move_command_selection(true);
        }
        let frame = layout(&mut app, size);
        assert_eq!(app.view.command_selected_index, 35);
        assert_eq!(app.command_tree.focused_id(), Some("command_query"));
        assert!(app.command_tree.scroll_offset("command_results") > 0.0);
        let row = frame
            .regions
            .iter()
            .find(|region| region.id == "command_result:35")
            .unwrap()
            .rect;
        let list = frame
            .regions
            .iter()
            .find(|region| region.id == "command_results")
            .unwrap()
            .rect;
        assert!(row.y >= list.y && row.y + row.height <= list.y + list.height + 0.1);
        assert_eq!(
            frame.hit_test(result_center(&frame, 35)),
            Some("command_result:35")
        );
        for _ in 0..40 {
            app.move_command_selection(false);
        }
        let frame = layout(&mut app, size);
        assert_eq!(app.view.command_selected_index, 0);
        assert_eq!(
            frame.hit_test(result_center(&frame, 0)),
            Some("command_result:0")
        );
        assert_eq!(app.command_tree.focused_id(), Some("command_query"));
    }
}

#[test]
fn arrow_navigation_after_tab_focus_submits_the_highlighted_result() {
    let (_directory, mut app) = fixture();
    let size = Size::new(560.0, 320.0);
    let mut results = app.view.command_results[..2].to_vec();
    results[0].target = Target::OpenSessions;
    results[1].target = Target::OpenSettings;
    app.set_command_results(results);
    layout(&mut app, size);

    // Reach the first row through native keyboard traversal, as a user would.
    for _ in 0..16 {
        if app.command_tree.focused_id() == Some("command_result:0") {
            break;
        }
        assert!(app
            .command_tree
            .handle_event(UiEvent::Key {
                key: Key::Tab,
                ctrl: false,
                shift: false,
            })
            .is_empty());
    }
    assert_eq!(app.command_tree.focused_id(), Some("command_result:0"));
    assert_eq!(app.view.command_selected_index, 0);

    // The host handles arrows before layout, then routes Enter to the tree.
    app.move_command_selection(true);
    assert_eq!(app.view.command_selected_index, 1);
    assert!(app.command_focus_pending);
    layout(&mut app, size);
    assert_eq!(app.command_tree.focused_id(), Some("command_query"));
    let actions = app.command_tree.handle_event(UiEvent::Key {
        key: Key::Enter,
        ctrl: false,
        shift: false,
    });
    assert_eq!(
        actions,
        vec![UiAction::Submitted {
            id: "command_query".into(),
            value: String::new(),
        }]
    );
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(
        app.view.show_settings,
        "Enter must activate row 1, not row 0"
    );
    assert!(!app.view.show_command_center);
}

#[test]
fn refreshing_unchanged_results_does_not_override_manual_scroll() {
    let (_directory, mut app) = fixture();
    let size = Size::new(560.0, 320.0);
    layout(&mut app, size);
    app.command_tree.scroll_to_end("command_results");
    layout(&mut app, size);
    let offset = app.command_tree.scroll_offset("command_results");
    assert!(offset > 0.0);
    app.set_command_results(app.view.command_results.clone());
    layout(&mut app, size);
    assert_eq!(app.command_tree.scroll_offset("command_results"), offset);
    assert_eq!(app.command_tree.focused_id(), Some("command_query"));
}

#[test]
fn query_edit_and_scope_selection_reset_scroll_but_preserve_mid_query_caret() {
    let (_directory, mut app) = fixture();
    let size = Size::new(560.0, 320.0);
    app.view.command_query = "uno dos".into();
    layout(&mut app, size);
    app.command_tree.set_input_value("command_query", "uno dos");
    assert!(app.command_tree.focus("command_query"));
    for _ in 0..3 {
        app.command_tree.handle_event(UiEvent::Key {
            key: Key::ArrowLeft,
            ctrl: false,
            shift: false,
        });
    }
    let selection = app.command_tree.input_selection("command_query").unwrap();
    assert_eq!(selection.0, 4);
    app.command_tree.scroll_to_end("command_results");
    layout(&mut app, size);
    let actions = app.command_tree.handle_event(UiEvent::Text("X".into()));
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert_eq!(app.view.command_query, "uno Xdos");
    assert!(
        !app.command_focus_pending,
        "typing must not schedule a focus that moves the caret"
    );
    layout(&mut app, size);
    assert_eq!(
        app.command_tree.input_selection("command_query").unwrap().0,
        5
    );
    assert_eq!(app.command_tree.scroll_offset("command_results"), 0.0);

    assert!(app.command_tree.focus("command_scope:commands"));
    app.apply_action(
        UiAction::Clicked {
            id: "command_scope:commands".into(),
        },
        &egui::Context::default(),
    );
    assert!(app.command_focus_pending);
    assert_eq!(app.view.command_selected_index, 0);
    layout(&mut app, size);
    assert_eq!(app.command_tree.focused_id(), Some("command_query"));
    assert_eq!(app.command_tree.scroll_offset("command_results"), 0.0);
}

#[test]
fn clicking_a_scope_replaces_the_typed_prefix_and_syncs_the_retained_query() {
    let (_directory, mut app) = fixture();
    let size = Size::new(560.0, 320.0);
    app.view.command_query = "#abc".into();
    let frame = layout(&mut app, size);
    assert_eq!(app.command_tree.input_value("command_query"), Some("#abc"));
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == "command_scope:files")
        .unwrap()
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    app.command_tree.handle_event(UiEvent::PointerDown(point));
    assert_eq!(app.command_tree.focused_id(), Some("command_scope:files"));
    let actions = app.command_tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(
        actions,
        vec![UiAction::Clicked {
            id: "command_scope:files".into()
        }]
    );
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert_eq!(app.view.command_query, "abc");
    assert_eq!(app.view.command_scope, Scope::Files);
    assert_eq!(
        command_center::parse_query(&app.view.command_query, app.view.command_scope).scope,
        Scope::Files
    );
    assert_eq!(
        app.command_tree.input_value("command_query"),
        Some("abc"),
        "retained input must be synchronized before the next layout"
    );
    assert!(app.command_focus_pending);
    layout(&mut app, size);
    assert_eq!(app.command_tree.focused_id(), Some("command_query"));
    assert_eq!(app.command_tree.input_value("command_query"), Some("abc"));
    assert_eq!(app.command_tree.scroll_offset("command_results"), 0.0);
}

#[test]
fn result_actions_reject_stale_query_scope_workspace_target_and_closed_picker() {
    for change in ["query", "scope", "workspace", "target", "closed"] {
        let (_directory, mut app) = fixture();
        let action = crate::view::command_result_action(&app.view, 0);
        match change {
            "query" => app.view.command_query.push('x'),
            "scope" => app.view.command_scope = Scope::Files,
            "workspace" => app.view.workspace.push_str("/other"),
            "target" => app.view.command_results[0].target = Target::OpenSessions,
            "closed" => app.view.show_command_center = false,
            _ => unreachable!(),
        }
        app.apply_action(UiAction::Clicked { id: action }, &egui::Context::default());
        assert!(
            !app.view.show_settings,
            "stale {change} must not open settings"
        );
        assert_eq!(app.view.show_command_center, change != "closed");
    }
    let (_directory, mut app) = fixture();
    app.apply_action(
        UiAction::Clicked {
            id: "command_result:0".into(),
        },
        &egui::Context::default(),
    );
    assert!(
        !app.view.show_settings,
        "unscoped legacy action must not activate a result"
    );
    let action = crate::view::command_result_action(&app.view, 0);
    app.apply_action(UiAction::Clicked { id: action }, &egui::Context::default());
    assert!(app.view.show_settings);
    assert!(!app.view.show_command_center);
}

#[test]
fn async_results_cancel_held_click_but_unchanged_rebuilds_keep_scoped_alias_clickable() {
    let (_directory, mut app) = fixture();
    let size = Size::new(560.0, 320.0);
    let frame = layout(&mut app, size);
    let point = result_center(&frame, 0);
    app.command_tree.handle_event(UiEvent::PointerDown(point));
    let mut changed = app.view.command_results.clone();
    changed[0].target = Target::OpenSessions;
    app.set_command_results(changed);
    layout(&mut app, size);
    assert!(app
        .command_tree
        .handle_event(UiEvent::PointerUp(point))
        .is_empty());
    assert!(app.view.show_command_center);

    let mut current = app.view.command_results.clone();
    current[0].target = Target::OpenSettings;
    app.set_command_results(current);
    let frame = layout(&mut app, size);
    let point = result_center(&frame, 0);
    app.command_tree.handle_event(UiEvent::PointerDown(point));
    app.set_command_results(app.view.command_results.clone());
    layout(&mut app, size);
    let expected = crate::view::command_result_action(&app.view, 0);
    let actions = app.command_tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: expected }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
    assert!(app.view.show_settings);
    assert!(!app.view.show_command_center);
}
