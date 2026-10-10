use super::*;

pub(super) fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = root.path().canonicalize().unwrap().display().to_string();
    app.view.selected_session = Some("alice".into());
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.settings_preferences.terminal_shell =
        TerminalShell::Custom("fixture-shell-A-relative-invalid".into());
    (root, app)
}

pub(super) fn click(app: &mut AgentsWindow, id: impl Into<String>) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn present_tabs(app: &mut AgentsWindow, width: f32) {
    let terminal = if app.view.show_terminal_panel {
        crate::terminal_dock_ui::build(&app.view, width, 300.0)
    } else {
        Element::spacer()
    };
    app.tree.set_root(Element::column(vec![
        terminal,
        Element::text_input("prompt", &app.view.prompt, "Task").size(None, Some(36.0)),
    ]));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 336.0));
    app.focus_prompt_if_pending();
    if app.terminal_focus_pending && app.view.show_terminal_panel {
        let screen = app.active_terminal_screen_id();
        if app.tree.focus(&screen) {
            app.terminal_focus_pending = false;
        }
    }
}

fn dispatch(app: &mut AgentsWindow, event: UiEvent) -> Vec<UiAction> {
    let actions = app.tree.handle_event(event);
    for action in &actions {
        app.apply_action(action.clone(), &egui::Context::default());
    }
    actions
}

fn click_rendered(app: &mut AgentsWindow, action: &str) {
    click_rendered_as(app, action, action);
}

fn click_rendered_as(app: &mut AgentsWindow, target: &str, action: &str) {
    let region = app
        .tree
        .frame()
        .regions
        .iter()
        .rev()
        .find(|region| region.id == target)
        .expect("rendered terminal control");
    let point = openide_ui::Point::new(
        region.rect.x + region.rect.width / 2.0,
        region.rect.y + region.rect.height / 2.0,
    );
    dispatch(app, UiEvent::PointerDown(point));
    assert_eq!(
        dispatch(app, UiEvent::PointerUp(point)),
        vec![UiAction::Clicked { id: action.into() }]
    );
}

#[test]
fn terminal_tab_strip_keyboard_reveals_all_tabs_and_keeps_focus_for_repeated_navigation() {
    use openide_ui::Key;
    let (_root, mut app) = fixture();
    for _ in 0..7 {
        click(&mut app, "terminal_new");
    }
    // Environment is a neighboring dock surface, not a modal focus owner.
    app.view.environment.open = true;
    let ids: Vec<_> = app.view.terminal_tabs.iter().map(|tab| tab.id).collect();
    present_tabs(&mut app, 480.0);
    assert!(app.tree.focus(&format!("terminal_tab:{}", ids[6])));
    for (key, expected) in [
        (Key::Home, ids[0]),
        (Key::End, ids[6]),
        (Key::ArrowLeft, ids[5]),
        (Key::ArrowRight, ids[6]),
        (Key::Home, ids[0]),
    ] {
        let action = format!("terminal_tab:{expected}");
        assert_eq!(
            dispatch(
                &mut app,
                UiEvent::Key {
                    key,
                    shift: false,
                    ctrl: false
                }
            ),
            vec![UiAction::Clicked { id: action.clone() }]
        );
        present_tabs(&mut app, 480.0);
        assert_eq!(app.active_terminal_id(), Some(expected));
        assert_eq!(app.tree.focused_id(), Some(action.as_str()));
        assert!(
            !app.terminal_focus_pending,
            "keyboard tab navigation must not enter the PTY"
        );
        let tab = app
            .tree
            .frame()
            .regions
            .iter()
            .find(|region| region.id == action)
            .expect("keyboard-selected tab is revealed");
        assert!(tab.rect.width > 0.0 && tab.rect.x >= 0.0 && tab.rect.x + tab.rect.width <= 480.0);
    }
    assert_eq!(
        app.view
            .terminal_tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        ids
    );
    assert!(
        app.terminals.is_empty(),
        "unresolved fixture shells never launch"
    );
}

#[test]
fn terminal_tab_mouse_menu_and_keyboard_close_preserve_neighbor_and_final_composer_focus() {
    use openide_ui::Key;
    let (_root, mut app) = fixture();
    for _ in 0..3 {
        click(&mut app, "terminal_new");
    }
    let ids: Vec<_> = app.view.terminal_tabs.iter().map(|tab| tab.id).collect();
    present_tabs(&mut app, 1100.0);
    // UiTree retains an existing input's buffer across root rebuilds. Seed the
    // draft through the real input event, not an ignored initial-value prop.
    assert!(app.tree.focus("prompt"));
    assert_eq!(app.tree.input_value("prompt"), Some(""));
    dispatch(&mut app, UiEvent::Text("chat draft".into()));
    assert_eq!(app.view.prompt, "chat draft");
    click_rendered(&mut app, &format!("terminal_tab:{}", ids[0]));
    assert_eq!(app.active_terminal_id(), Some(ids[0]));
    assert!(
        app.terminal_focus_pending,
        "mouse tab selection enters its PTY"
    );
    present_tabs(&mut app, 1100.0);
    // A close hit belongs to its own tab; it must not select that inactive PTY.
    click_rendered(&mut app, &format!("terminal_close:{}", ids[1]));
    assert_eq!(app.active_terminal_id(), Some(ids[0]));
    assert!(!app.dock.contains_tab(ids[1]));
    assert!(app.dock.contains_tab(ids[0]) && app.dock.contains_tab(ids[2]));

    present_tabs(&mut app, 1100.0);
    click_rendered(&mut app, "terminal_sessions_toggle");
    present_tabs(&mut app, 1100.0);
    let menu_action = format!("terminal_tab_menu:{}", ids[2]);
    assert!(app.tree.focus(&menu_action));
    dispatch(
        &mut app,
        UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false,
        },
    );
    assert_eq!(app.active_terminal_id(), Some(ids[2]));
    assert!(
        app.terminal_focus_pending,
        "menu selection intentionally enters the PTY"
    );
    assert!(!app.view.terminal_controls.sessions_open);

    present_tabs(&mut app, 1100.0);
    assert!(app.tree.focus(&format!("terminal_close:{}", ids[2])));
    dispatch(
        &mut app,
        UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false,
        },
    );
    present_tabs(&mut app, 1100.0);
    assert_eq!(app.active_terminal_id(), Some(ids[0]));
    assert_eq!(
        app.tree.focused_id(),
        Some(format!("terminal_tab:{}", ids[0]).as_str())
    );
    assert!(!app.terminal_focus_pending);
    assert!(app.tree.focus(&format!("terminal_close:{}", ids[0])));
    dispatch(
        &mut app,
        UiEvent::Key {
            key: Key::Enter,
            shift: false,
            ctrl: false,
        },
    );
    present_tabs(&mut app, 1100.0);
    assert!(app.active_terminal_id().is_none() && !app.view.show_terminal_panel);
    assert_eq!(app.tree.focused_id(), Some("prompt"));
    assert_eq!(app.tree.input_value("prompt"), Some("chat draft"));
    assert_eq!(app.view.prompt, "chat draft");
    assert!(!app.terminal_focus_pending && !app.prompt_focus_pending);
}

#[test]
fn terminal_session_list_close_keeps_selection_independent_and_returns_focus_to_safe_rows() {
    let (_root, mut app) = fixture();
    for _ in 0..3 {
        click(&mut app, "terminal_new");
    }
    let ids: Vec<_> = app.view.terminal_tabs.iter().map(|tab| tab.id).collect();
    present_tabs(&mut app, 640.0);
    assert!(app.tree.focus("prompt"));
    dispatch(
        &mut app,
        UiEvent::Text("draft kept while managing shells".into()),
    );
    click_rendered(&mut app, "terminal_sessions_toggle");
    present_tabs(&mut app, 640.0);
    click_rendered(&mut app, &format!("terminal_tab_menu:{}", ids[0]));
    present_tabs(&mut app, 640.0);
    click_rendered(&mut app, "terminal_sessions_toggle");
    present_tabs(&mut app, 640.0);

    for (closed, expected_active) in [(ids[1], ids[0]), (ids[0], ids[2])] {
        click_rendered_as(
            &mut app,
            &format!("terminal_session_close:{closed}"),
            &format!("terminal_close:{closed}"),
        );
        present_tabs(&mut app, 640.0);
        assert_eq!(app.active_terminal_id(), Some(expected_active));
        assert!(!app.dock.contains_tab(closed));
        assert!(!app.terminal_controls_host.profiles.contains_key(&closed));
        assert!(!app.terminal_controls_host.errors.contains_key(&closed));
        assert!(app.view.terminal_controls.sessions_open);
        assert!(!app.terminal_focus_pending);
        assert_eq!(
            app.tree.focused_id(),
            Some(format!("terminal_tab_menu:{expected_active}").as_str()),
            "closing must focus a selection row, never another close action"
        );
    }

    // Selection retains its normal behavior: close the menu and enter the PTY.
    click_rendered(&mut app, &format!("terminal_tab_menu:{}", ids[2]));
    assert!(!app.view.terminal_controls.sessions_open);
    assert!(app.terminal_focus_pending);
    present_tabs(&mut app, 640.0);
    click_rendered(&mut app, "terminal_sessions_toggle");
    present_tabs(&mut app, 640.0);
    click_rendered_as(
        &mut app,
        &format!("terminal_session_close:{}", ids[2]),
        &format!("terminal_close:{}", ids[2]),
    );
    present_tabs(&mut app, 640.0);
    assert!(app.active_terminal_id().is_none());
    assert!(!app.view.show_terminal_panel && !app.view.terminal_controls.sessions_open);
    assert_eq!(app.tree.focused_id(), Some("prompt"));
    assert_eq!(app.view.prompt, "draft kept while managing shells");
    assert_eq!(
        app.tree.input_value("prompt"),
        Some(app.view.prompt.as_str())
    );
    assert!(!app.terminal_focus_pending && !app.prompt_focus_pending);
    assert!(
        app.terminals.is_empty(),
        "invalid fixture shells never launch"
    );
}

#[test]
fn last_session_closed_in_ide_focuses_the_visible_editor_or_file_search_without_revealing_chat() {
    for open_file in [false, true] {
        let (root, mut app) = fixture();
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.ide.agent_open = false;
        app.view.ide.compact_panel = None;
        app.view.prompt = "saved chat draft".into();
        app.prompt_focus_pending = false;
        app.viewport_width = 1440.0;
        app.viewport_height = 900.0;
        let editor_id = if open_file {
            std::fs::write(root.path().join("main.rs"), "fn main() {}\n").unwrap();
            let scope = app.dock_scope();
            let tab = app.dock.open(
                &scope,
                DockTabKind::FilePreview {
                    path: "main.rs".into(),
                },
            );
            app.sync_dock_view();
            let deadline = Instant::now() + Duration::from_secs(5);
            while app
                .view
                .code_editor
                .as_ref()
                .is_none_or(|editor| editor.loading)
            {
                app.poll_code_editor(&egui::Context::default());
                assert!(Instant::now() < deadline, "editor fixture did not load");
                std::thread::sleep(Duration::from_millis(2));
            }
            Some(crate::code_editor_ui::view_id(tab.get()))
        } else {
            None
        };
        click(&mut app, "terminal_new");
        let terminal = app.active_terminal_id().unwrap();
        let present = |app: &mut AgentsWindow| {
            app.tree
                .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
            app.tree.set_reduced_motion(true);
            app.tree.layout(Size::new(1440.0, 900.0));
        };
        present(&mut app);
        assert!(!app
            .tree
            .frame()
            .regions
            .iter()
            .any(|region| region.id == "prompt"));
        click_rendered(&mut app, "terminal_sessions_toggle");
        present(&mut app);
        click_rendered_as(
            &mut app,
            &format!("terminal_session_close:{terminal}"),
            &format!("terminal_close:{terminal}"),
        );
        present(&mut app);
        app.poll_editor_workbench(&egui::Context::default());
        assert_eq!(
            app.tree.focused_id(),
            Some(editor_id.as_deref().unwrap_or("ide_titlebar_quick_open"))
        );
        assert!(!app.view.show_terminal_panel && app.active_terminal_id().is_none());
        assert!(!app.prompt_focus_pending && !app.terminal_focus_pending);
        assert!(app.editor_services.focus_next.is_none());
        assert!(!app.view.ide.agent_open && app.view.ide.compact_panel.is_none());
        assert_eq!(app.view.prompt, "saved chat draft");
        assert!(app.terminals.is_empty());
    }
}

#[test]
fn terminal_tab_close_prefers_split_partner_and_rejects_other_owners_and_modal_background() {
    let (_root, mut app) = fixture();
    let scope = app.dock_scope();
    let editor = app.dock.open(&scope, DockTabKind::Files);
    click(&mut app, "terminal_new");
    let first = app.active_terminal_id().unwrap();
    click(&mut app, "terminal_split");
    let second = app.active_terminal_id().unwrap();
    click(&mut app, "terminal_new");
    let unrelated = app.active_terminal_id().unwrap();
    click(&mut app, format!("terminal_tab:{second}"));
    assert_eq!(app.visible_terminal_ids(), vec![first, second]);
    click(&mut app, format!("terminal_close:{second}"));
    assert_eq!(
        app.active_terminal_id(),
        Some(first),
        "closing a split pane keeps its partner visible"
    );
    assert_eq!(app.visible_terminal_ids(), vec![first]);
    assert!(app.view.terminal_controls.panes.is_empty());
    assert!(app.dock.contains_tab(unrelated));
    assert_eq!(app.dock.active_tab(&scope).map(|tab| tab.id), Some(editor));

    app.view.selected_session = Some("bob".into());
    app.sync_dock_view();
    click(&mut app, "terminal_new");
    let foreign = app.active_terminal_id().unwrap();
    for action in [
        format!("terminal_tab:{first}"),
        format!("terminal_close:{first}"),
        format!("terminal_tab_menu:{first}"),
    ] {
        click(&mut app, action);
    }
    assert_eq!(app.active_terminal_id(), Some(foreign));
    assert!(app.dock.contains_tab(first) && app.dock.contains_tab(unrelated));
    app.tree.set_root(
        Element::popover(
            Element::button("other", "Other dialog"),
            Element::text_input("other_input", "kept", "Name"),
            "other_dismiss",
            openide_ui::PopoverPlacement::BottomStart,
        )
        .popover_scrim(openide_ui::Color::rgba(0, 0, 0, 100)),
    );
    app.tree.layout(Size::new(600.0, 400.0));
    assert_eq!(app.tree.focused_id(), Some("other_input"));
    click(&mut app, format!("terminal_close:{foreign}"));
    assert_eq!(app.active_terminal_id(), Some(foreign));
    assert_eq!(app.tree.focused_id(), Some("other_input"));
    assert_eq!(app.tree.input_value("other_input"), Some("kept"));
}

#[test]
fn terminal_launch_profile_is_frozen_across_restart_new_default_and_close() {
    let (_root, mut app) = fixture();
    let first_profile = app.settings_preferences.terminal_shell.clone();
    click(&mut app, "terminal_new");
    let first = app.active_terminal_id().unwrap();
    assert!(
        app.terminals.is_empty(),
        "test profiles must fail before a child launches"
    );
    app.settings_preferences.terminal_shell =
        TerminalShell::Custom("fixture-shell-B-relative-invalid".into());
    click(&mut app, "terminal_restart");
    assert_eq!(app.terminal_profile_for(first), first_profile);
    click(&mut app, "terminal_new");
    let second = app.active_terminal_id().unwrap();
    assert_eq!(
        app.terminal_profile_for(second),
        app.settings_preferences.terminal_shell
    );
    click(&mut app, format!("terminal_tab:{first}"));
    click(&mut app, "terminal_panel_hide");
    click(&mut app, "terminal_toggle");
    assert_eq!(app.active_terminal_id(), Some(first));
    assert_eq!(app.terminal_profile_for(first), first_profile);
    click(&mut app, format!("terminal_close:{first}"));
    assert!(!app.terminal_controls_host.profiles.contains_key(&first));
    assert!(app.terminal_controls_host.profiles.contains_key(&second));
}

#[test]
fn terminal_rename_preserves_identity_and_rejects_cross_owner_or_invalid_names() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    let id = app.active_terminal_id().unwrap();
    click(&mut app, format!("terminal_rename:{id}"));
    let ctx = egui::Context::default();
    let input = format!("terminal_rename_input:{id}");
    app.apply_action(
        UiAction::InputChanged {
            id: input.clone(),
            value: " Build 🦀 ".into(),
        },
        &ctx,
    );
    click(&mut app, format!("terminal_rename_confirm:{id}"));
    assert_eq!(app.active_terminal_id(), Some(id));
    assert_eq!(app.view.terminal_tabs[0].title, "Build 🦀");
    click(&mut app, "terminal_panel_hide");
    click(&mut app, "terminal_toggle");
    assert_eq!(app.view.terminal_tabs[0].title, "Build 🦀");
    click(&mut app, format!("terminal_rename:{id}"));
    app.apply_action(
        UiAction::InputChanged {
            id: input.clone(),
            value: "bad\nname".into(),
        },
        &ctx,
    );
    click(&mut app, format!("terminal_rename_confirm:{id}"));
    assert!(app
        .view
        .terminal_controls
        .rename
        .as_ref()
        .unwrap()
        .error
        .is_some());
    assert_eq!(app.view.terminal_tabs[0].title, "Build 🦀");
    app.view.selected_session = Some("bob".into());
    app.sync_dock_view();
    app.apply_action(
        UiAction::InputChanged {
            id: input,
            value: "foreign".into(),
        },
        &ctx,
    );
    click(&mut app, format!("terminal_rename_confirm:{id}"));
    assert_eq!(
        app.terminal_controls_host
            .names
            .get(&id)
            .map(String::as_str),
        Some("Build 🦀")
    );
    assert!(app.view.terminal_controls.rename.is_none());
}

#[test]
fn terminal_profile_picker_excludes_unresolved_shells_and_is_scope_bound_even_after_launch_error() {
    let (_root, mut app) = fixture();
    click(&mut app, "terminal_new");
    assert!(app.view.terminal_error.is_some());
    click(&mut app, "terminal_profiles_toggle");
    assert!(
        app.view.terminal_controls.profiles_open,
        "a failed shell must still allow choosing another profile"
    );
    let profiles = &app.view.terminal_controls.profiles;
    assert!(profiles.iter().any(|profile| profile.id == "system"));
    assert!(!profiles.iter().any(|profile| profile.id == "default"));
    // Agent CLI entries resolve through their own program lookup; every
    // advertised entry of either kind must still be launchable.
    assert!(profiles.iter().all(|profile| {
        match crate::agent_cli_definitions::profile(&profile.id) {
            Some((agent, _)) => app.resolve_agent_cli(agent).is_some(),
            None => named_profile(&profile.id, &app.settings_preferences.terminal_shell)
                .unwrap()
                .command()
                .is_ok(),
        }
    }));
    let count = app.view.terminal_tabs.len();
    click(&mut app, "terminal_new_profile:untrusted-executable");
    assert_eq!(app.view.terminal_tabs.len(), count);
    click(&mut app, "terminal_sessions_toggle");
    assert!(app.view.terminal_controls.sessions_open && !app.view.terminal_controls.profiles_open);
    click(&mut app, "terminal_sessions_dismiss");
    assert!(!app.view.terminal_controls.sessions_open);
    click(&mut app, "terminal_profiles_toggle");
    app.view.selected_session = Some("bob".into());
    app.sync_dock_view();
    click(&mut app, "terminal_new_profile:system");
    assert!(
        app.view.terminal_tabs.is_empty(),
        "a stale menu cannot launch in a different task"
    );
    assert!(app.terminals.is_empty());
}
