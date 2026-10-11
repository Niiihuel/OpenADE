//! Host regressions use recorded DAP events and isolated workspaces, never an adapter.
use super::*;
use openide_debug::{Breakpoint, Scope, Source, Variable};
use openide_editor::Selection;

fn fixture() -> (tempfile::TempDir, AgentsWindow) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = workspace
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.view.workspace_draft = app.view.workspace.clone();
    app.view.main_page = MainPage::Workspace;
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    app.view.show_settings = false;
    app.reset_debug_workspace();
    app.view.debug_open = true;
    (root, app)
}

fn stopped(app: &mut AgentsWindow, stop: u64) {
    app.handle_debug_event(DebugEvent::Stopped {
        stop_id: stop,
        reason: "breakpoint".into(),
        description: None,
        thread_id: Some(7),
        all_threads_stopped: true,
    });
}

fn frame(id: i64, path: Option<PathBuf>) -> StackFrame {
    StackFrame {
        id,
        name: format!("frame-{id}"),
        source: path.map(|path| Source {
            path: Some(path),
            ..Default::default()
        }),
        line: 2,
        column: 1,
        end_line: None,
        end_column: None,
    }
}

fn node(key: &str, reference: i64) -> VariableNode {
    VariableNode {
        reference,
        children: Vec::new(),
        path: key.into(),
        more: false,
        view: DebugVariableView {
            key: key.into(),
            name: key.into(),
            has_children: true,
            expanded: true,
            loading: true,
            ..Default::default()
        },
    }
}

fn open_fixture_document(app: &mut AgentsWindow) {
    std::fs::write(
        PathBuf::from(&app.view.workspace).join("main.rs"),
        "fn main() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    let scope = app.dock_scope();
    app.dock.open(
        &scope,
        DockTabKind::FilePreview {
            path: "main.rs".into(),
        },
    );
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .view
        .code_editor
        .as_ref()
        .is_none_or(|view| view.loading)
    {
        app.poll_code_editor(&egui::Context::default());
        assert!(Instant::now() < deadline, "isolated editor failed to load");
        std::thread::sleep(Duration::from_millis(2));
    }
    app.sync_editor_structure();
}

#[test]
fn debug_navigation_cannot_move_a_homonymous_file_after_a_workspace_switch() {
    let (root, mut app) = fixture();
    open_fixture_document(&mut app);
    let original_workspace = app.view.workspace.clone();
    app.queue_debug_navigation(
        "main.rs".into(),
        openide_language::Position {
            line: 1,
            character: 0,
        },
    );
    let other = root.path().join("other-project");
    std::fs::create_dir(&other).unwrap();
    app.view.workspace = other.to_string_lossy().into_owned();
    open_fixture_document(&mut app);
    assert!(app.guard_debug_navigation());
    assert!(app.editor_services.pending_navigation.is_none());
    app.poll_editor_workbench(&egui::Context::default());
    let snapshot = app.active_editor().unwrap().3;
    assert_eq!(
        snapshot.position_of(snapshot.selection().head).unwrap().row,
        0
    );

    // A different destination issued by another tool owns the shared slot.
    app.queue_debug_navigation(
        "main.rs".into(),
        openide_language::Position {
            line: 1,
            character: 0,
        },
    );
    let replacement = (
        PathBuf::from("other.rs"),
        openide_language::Position {
            line: 2,
            character: 0,
        },
    );
    app.editor_services.pending_navigation = Some(replacement.clone());
    app.view.workspace = original_workspace;
    assert!(!app.guard_debug_navigation());
    assert_eq!(app.editor_services.pending_navigation, Some(replacement));
}

#[test]
fn agents_mode_ignores_debug_shortcuts_and_stale_debug_controls() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;
    app.view.debug_open = false;
    for (key, shift) in [
        (openide_ui::Key::F5, false),
        (openide_ui::Key::F5, true),
        (openide_ui::Key::F10, false),
        (openide_ui::Key::F11, false),
        (openide_ui::Key::F11, true),
    ] {
        assert!(!app.handle_debug_key(
            &UiEvent::Key {
                key,
                ctrl: false,
                shift
            },
            &ctx
        ));
    }
    for id in ["dock_open:debugger", "ide_debug_toggle"] {
        assert!(app.apply_debug_action(&UiAction::Clicked { id: id.into() }, &ctx));
    }
    assert!(!app.view.debug_open);
    assert!(app.debugger.config_job.is_none());
    assert!(app.debugger.session.is_none());
    app.view.debug_open = true; // Preserved state from a hidden IDE session.
    let id = crate::debug_ui::action(&app.view.debug.scope, "configure");
    assert!(app.apply_debug_action(&UiAction::Clicked { id }, &ctx));
    assert!(app.debugger.config_job.is_none());
    assert!(!PathBuf::from(&app.view.workspace)
        .join(debug_config::CONFIG_PATH)
        .exists());
    assert_eq!(app.view.ide.mode, crate::ide_workspace::WindowMode::Agents);
}

#[test]
fn ide_menu_blocks_debug_shortcuts_and_f9_without_changing_editor_or_menu_focus() {
    let (_root, mut app) = fixture();
    open_fixture_document(&mut app);
    app.viewport_width = 1440.0;
    app.viewport_height = 900.0;
    app.tree
        .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
    app.tree.layout(Size::new(1440.0, 900.0));
    let editor = app.view.code_editor.as_ref().unwrap();
    let editor_id = crate::code_editor_ui::view_id(editor.tab_id);
    assert!(app.tree.focus(&editor_id));
    let before = editor.controller.as_ref().unwrap().snapshot().unwrap();
    app.view.debug_open = false;
    app.view.ide.menu_open = Some("run".into());
    let ctx = egui::Context::default();
    for (key, shift) in [
        (openide_ui::Key::F5, false),
        (openide_ui::Key::F5, true),
        (openide_ui::Key::F10, false),
        (openide_ui::Key::F11, false),
        (openide_ui::Key::F11, true),
    ] {
        assert!(!app.handle_debug_key(
            &UiEvent::Key {
                key,
                ctrl: false,
                shift
            },
            &ctx
        ));
    }
    assert!(!app.handle_editor_workbench_key(
        &UiEvent::Key {
            key: openide_ui::Key::F9,
            ctrl: false,
            shift: false
        },
        &ctx,
    ));
    assert!(!app.view.debug_open);
    assert!(app.debugger.config_job.is_none());
    assert!(app.debugger.session.is_none());
    assert!(app.debug_source_breakpoints().is_empty());
    assert_eq!(app.view.ide.menu_open.as_deref(), Some("run"));
    assert_eq!(app.tree.focused_id(), Some(editor_id.as_str()));
    let after = app.active_editor().unwrap().3;
    assert_eq!(after.revision(), before.revision());
    assert_eq!(after.selection(), before.selection());
    // Menu navigation still reaches the dedicated handler.
    assert!(app.handle_ide_menu_event(&UiEvent::Key {
        key: openide_ui::Key::ArrowRight,
        ctrl: false,
        shift: false,
    }));
    assert_eq!(app.view.ide.menu_open.as_deref(), Some("terminal"));
}

#[test]
fn run_menu_reopens_hidden_debugger_only_for_current_enabled_commands() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    // Recorded stopped state suffices to exercise routing, without an adapter.
    stopped(&mut app, 4);
    app.debugger.configurations = vec![serde_json::from_value(serde_json::json!({
        "name": "Never launched", "adapter": {"command": "missing-test-adapter"},
        "request": "launch", "arguments": {}
    }))
    .unwrap()];
    app.refresh_debug_controls();
    let current = crate::debug_ui::action(&app.view.debug.scope, "resume");
    app.view.debug_open = false;
    // Hidden inspector buttons remain invalid even though the menu is allowed.
    assert!(app.apply_debug_action(
        &UiAction::Clicked {
            id: current.clone()
        },
        &ctx
    ));
    assert!(!app.view.debug_open);
    for target in [
        crate::debug_ui::action("stale workspace/stop", "resume"),
        crate::debug_ui::action(&app.view.debug.scope, "pause"),
        crate::debug_ui::action(&app.view.debug.scope, "configure"),
    ] {
        app.view.ide.menu_open = Some("run".into());
        let id = format!(
            "ide_menu_action:{}",
            serde_json::to_string(&(None::<(u64, u64)>, target)).unwrap()
        );
        assert!(app.apply_ide_menu_action(&UiAction::Clicked { id }, &ctx));
        assert!(!app.view.debug_open);
        assert!(app.view.ide.menu_open.is_none());
        assert!(app.debugger.config_job.is_none());
    }
    app.view.ide.menu_open = Some("run".into());
    let id = format!(
        "ide_menu_action:{}",
        serde_json::to_string(&(None::<(u64, u64)>, current)).unwrap()
    );
    assert!(app.apply_ide_menu_action(&UiAction::Clicked { id }, &ctx));
    assert!(app.view.debug_open);
    assert!(app.view.ide.menu_open.is_none());
    assert_eq!(app.view.debug.status, DebugStatus::Stopped);
    assert!(app.debugger.config_job.is_none());
    assert!(app.debugger.session.is_none());
}

#[test]
fn accepted_editor_close_waits_for_debug_disconnect_after_keep_or_discard() {
    for (decision, durable, cancel_wait) in [
        ("code_editor_quit_discard", false, false),
        ("code_editor_quit_discard", true, false),
        ("code_editor_quit_keep", true, false),
        ("code_editor_quit_keep", true, true),
    ] {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        if durable {
            // Recovery initialization precedes loading on the same worker.
            app.init_code_editor_recovery();
        }
        open_fixture_document(&mut app);
        let controller = app.code_editor_controller(Path::new("main.rs")).unwrap();
        controller
            .shared_buffer()
            .lock()
            .unwrap()
            .apply_transaction(
                &[openide_editor::TextEdit {
                    range: 0..0,
                    replacement: "// unsaved\n".into(),
                }],
                Selection::caret(0),
                openide_editor::EditKind::Paste,
                1,
            )
            .unwrap();
        assert!(app.code_editor.has_pending_changes());
        // A missing executable exercises real worker disconnect/reap without
        // launching a process. Events remain queued until poll_debugger below.
        app.debugger.session = Some(
            DebugSession::start(openide_debug::DebugConfig::new(
                PathBuf::from(&app.view.workspace).join("never-created-adapter"),
                &app.view.workspace,
                serde_json::json!({}),
            ))
            .unwrap(),
        );
        let mut close_input = egui::RawInput::default();
        close_input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let commands = |output: &egui::FullOutput| {
            output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .unwrap()
                .commands
                .clone()
        };
        let output = ctx.run_ui(close_input, |ui| {
            app.guard_editor_close(ui.ctx());
            app.guard_debugger_close(ui.ctx());
        });
        assert!(app.view.code_editor_quit_confirm);
        assert!(
            !app.debugger.close_after_stop,
            "the editor decision comes first"
        );
        assert!(commands(&output).contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands(&output).contains(&egui::ViewportCommand::Close));
        output.drop_without_applying_deltas();

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.apply_code_editor_action(
                &UiAction::Clicked {
                    id: decision.into(),
                },
                ui.ctx(),
            );
        });
        assert!(!commands(&output).contains(&egui::ViewportCommand::Close));
        output.drop_without_applying_deltas();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.code_editor.close_authorized() {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.poll_code_editor(ui.ctx());
            });
            assert!(
                !commands(&output).contains(&egui::ViewportCommand::Close),
                "programmatic editor close bypassed the live DAP owner: {decision}"
            );
            output.drop_without_applying_deltas();
            assert!(
                Instant::now() < deadline,
                "editor did not authorize {decision}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.debugger.close_after_stop);
        assert!(app.debugger.session.is_some());
        if cancel_wait {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.apply_code_editor_action(
                    &UiAction::Clicked {
                        id: "code_editor_quit_cancel".into(),
                    },
                    ui.ctx(),
                );
            });
            assert!(!commands(&output).contains(&egui::ViewportCommand::Close));
            output.drop_without_applying_deltas();
            assert!(!app.debugger.close_after_stop && !app.code_editor.close_authorized());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                // Match host order: editor completion runs before DAP polling.
                app.poll_code_editor(ui.ctx());
                app.poll_debugger(ui.ctx());
            });
            let closed = commands(&output).contains(&egui::ViewportCommand::Close);
            output.drop_without_applying_deltas();
            if cancel_wait {
                assert!(!closed, "Cancel must revoke the pending viewport close");
                if app.debugger.session.is_none() {
                    break;
                }
            } else if closed {
                assert!(app.debugger.session.is_none());
                assert!(!app.debugger.close_after_stop);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker disconnect did not release close"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[test]
fn debugger_activity_sidebar_and_breakpoint_gutter_are_exclusive_to_ide() {
    let (_root, mut app) = fixture();
    open_fixture_document(&mut app);
    app.viewport_width = 1440.0;
    app.viewport_height = 900.0;
    let size = Size::new(1440.0, 900.0);
    let ctx = egui::Context::default();
    let (_, _, controller, before) = app.active_editor().unwrap();
    for ide in [false, true] {
        app.view.ide.mode = if ide {
            crate::ide_workspace::WindowMode::Ide
        } else {
            crate::ide_workspace::WindowMode::Agents
        };
        app.view.ide.activity = crate::ide_workspace::IdeActivity::Files;
        app.view.debug_open = false;
        let mut tree = openide_ui::UiTree::new(crate::view::build_window_view(
            &app.view,
            size.width,
            size.height,
        ));
        tree.set_reduced_motion(true);
        let frame = tree.layout(size);
        let rail = frame.regions.iter().find(|region| {
            region.id == "ide_activity:debug" && region.kind == openide_ui::HitKind::Button
        });
        assert_eq!(rail.is_some(), ide);
        assert_eq!(
            frame.regions.iter().any(|region| {
                region.id.starts_with("editor_breakpoint:")
                    && region.kind == openide_ui::HitKind::Button
            }),
            ide
        );
        let configure = crate::debug_ui::action(&app.view.debug.scope, "configure");
        assert!(
            !frame.regions.iter().any(|region| region.id == configure),
            "debug inspector must not be painted over the files activity"
        );

        if !ide {
            app.view.show_dock_add_menu = true;
            tree.set_root(crate::view::build_window_view(
                &app.view,
                size.width,
                size.height,
            ));
            let menu = tree.layout(size);
            assert!(!menu
                .regions
                .iter()
                .any(|region| region.id == "dock_open:debugger"));
            app.view.show_dock_add_menu = false;
            // Stale navigation and inspector actions cannot enter debug from Agents.
            assert!(!app.apply_ide_activity_action(
                &UiAction::Clicked {
                    id: "ide_activity:debug".into()
                },
                &ctx,
            ));
            app.apply_debug_action(
                &UiAction::Clicked {
                    id: "dock_open:debugger".into(),
                },
                &ctx,
            );
            app.apply_debug_action(&UiAction::Clicked { id: configure }, &ctx);
            assert!(!app.view.debug_open);
            assert!(app.debugger.config_job.is_none());
            continue;
        }
        let rect = rail.unwrap().rect;
        let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        let actions = tree.handle_event(UiEvent::PointerUp(point));
        assert_eq!(
            actions,
            vec![UiAction::Clicked {
                id: "ide_activity:debug".into()
            }]
        );
        for action in actions {
            app.apply_action(action, &ctx);
        }
        assert_eq!(
            app.view.ide.activity,
            crate::ide_workspace::IdeActivity::Debug
        );
        assert!(app.view.ide.explorer_open && app.view.debug_open);
        assert!(
            app.debugger.session.is_none(),
            "opening the sidebar must not launch an adapter"
        );
        tree.set_root(crate::view::build_window_view(
            &app.view,
            size.width,
            size.height,
        ));
        let sidebar = tree.layout(size);
        let configure = crate::debug_ui::action(&app.view.debug.scope, "configure");
        assert!(sidebar.regions.iter().any(|region| {
            region.id == configure && region.kind == openide_ui::HitKind::Button
        }));
        let breakpoint = sidebar
            .regions
            .iter()
            .find(|region| {
                region.id.starts_with("editor_breakpoint:")
                    && region.kind == openide_ui::HitKind::Button
            })
            .unwrap();
        let rect = breakpoint.rect;
        let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        tree.handle_event(UiEvent::PointerDown(point));
        let actions = tree.handle_event(UiEvent::PointerUp(point));
        assert_eq!(
            actions,
            vec![UiAction::Clicked {
                id: breakpoint.id.clone()
            }]
        );
        for action in actions {
            app.apply_action(action, &ctx);
        }
        assert_eq!(app.view.editor_workbench.breakpoint_rows.len(), 1);
        app.apply_debug_action(
            &UiAction::Clicked {
                id: crate::debug_ui::action("previous workspace", "hide"),
            },
            &ctx,
        );
        assert!(
            app.view.debug_open,
            "stale scope cannot hide the current sidebar"
        );
        app.apply_debug_action(
            &UiAction::Clicked {
                id: crate::debug_ui::action(&app.view.debug.scope, "hide"),
            },
            &ctx,
        );
        assert!(!app.view.debug_open && !app.view.ide.explorer_open);
    }
    let after = controller.snapshot().unwrap();
    assert_eq!(after.document_id(), before.document_id());
    assert_eq!(after.revision(), before.revision());
    assert_eq!(after.selection(), before.selection());
}

#[test]
fn switching_to_ide_preserves_document_and_enables_explicit_debug_access() {
    let (_root, mut app) = fixture();
    open_fixture_document(&mut app);
    let ctx = egui::Context::default();
    app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;
    app.view.debug_open = false;
    let (tab, path, controller, before) = app.active_editor().unwrap();
    app.apply_action(
        UiAction::Clicked {
            id: "editor_breakpoint".into(),
        },
        &ctx,
    );
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());
    assert!(app.apply_ide_workspace_action(&UiAction::Clicked {
        id: "window_mode_toggle".into()
    }));
    assert_eq!(app.active_editor().unwrap().0, tab);
    assert_eq!(app.active_editor().unwrap().1, path);
    assert!(app.apply_debug_action(
        &UiAction::Clicked {
            id: "ide_debug_toggle".into()
        },
        &ctx
    ));
    assert!(app.view.debug_open);
    assert!(
        app.debugger.session.is_none(),
        "opening the panel must never launch an adapter"
    );
    assert_eq!(
        controller.snapshot().unwrap().document_id(),
        before.document_id()
    );
    assert_eq!(
        controller.snapshot().unwrap().selection(),
        before.selection()
    );
    app.apply_action(
        UiAction::Clicked {
            id: "editor_breakpoint".into(),
        },
        &ctx,
    );
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0]);
    assert!(app.apply_ide_workspace_action(&UiAction::Clicked {
        id: "window_mode_toggle".into()
    }));
    assert_eq!(app.active_editor().unwrap().0, tab);
    assert_eq!(
        &*app.view.editor_workbench.breakpoint_rows,
        &[0],
        "changing presentation preserves the IDE marker"
    );
}

#[test]
fn a_debug_stop_in_agents_never_opens_an_adapter_source_or_moves_the_editor() {
    let (_root, mut app) = fixture();
    open_fixture_document(&mut app);
    app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;
    let (tab, path, controller, before) = app.active_editor().unwrap();
    let navigation = app.editor_services.pending_navigation.clone();
    stopped(&mut app, 11);
    app.debugger.stack_request = Some(91);
    app.handle_debug_event(DebugEvent::StackTrace {
        request_id: 91,
        stop_id: 11,
        thread_id: 7,
        frames: vec![frame(
            30,
            Some(PathBuf::from(&app.view.workspace).join("different.rs")),
        )],
        total_frames: Some(1),
    });
    assert_eq!(app.view.debug.status, DebugStatus::Stopped);
    assert_eq!(
        app.debugger.frame,
        Some(30),
        "inspection remains available upon returning to IDE"
    );
    assert_eq!(app.view.ide.mode, crate::ide_workspace::WindowMode::Agents);
    assert_eq!(app.active_editor().unwrap().0, tab);
    assert_eq!(app.active_editor().unwrap().1, path);
    assert_eq!(app.editor_services.pending_navigation, navigation);
    assert_eq!(
        controller.snapshot().unwrap().selection(),
        before.selection()
    );
}

#[test]
fn debugger_actions_from_previous_stop_or_workspace_cannot_change_current_panel() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 1);
    let old = crate::debug_ui::action(&app.view.debug.scope, "hide");
    stopped(&mut app, 2);
    assert!(app.apply_debug_action(&UiAction::Clicked { id: old }, &egui::Context::default()));
    assert!(
        app.view.debug_open,
        "an earlier stop cannot hide the current inspector"
    );

    let old = crate::debug_ui::action(&app.view.debug.scope, "hide");
    app.view.workspace.push_str("-other");
    // No background reload is needed to exercise owner invalidation.
    app.view.debug_open = false;
    app.reset_debug_workspace();
    app.view.debug_open = true;
    assert!(app.apply_debug_action(&UiAction::Clicked { id: old }, &egui::Context::default()));
    assert!(
        app.view.debug_open,
        "a prior workspace cannot control this panel"
    );
}

#[test]
fn ide_missing_configuration_is_onboarding_but_invalid_configuration_remains_an_error() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    let refresh = |app: &mut AgentsWindow| {
        assert!(app.apply_debug_action(
            &UiAction::Clicked {
                id: "dock_open:debugger".into()
            },
            &ctx
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.debugger.config_job.is_some() {
            app.poll_debugger(&ctx);
            assert!(
                Instant::now() < deadline,
                "configuration worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    refresh(&mut app);
    assert_eq!(app.view.debug.status, DebugStatus::Unconfigured);
    assert_eq!(app.view.debug.message, None, "the panel's empty state says it");
    assert!(!PathBuf::from(&app.view.workspace).join(".openide").exists());
    assert!(app.debugger.session.is_none());
    debug_config::ensure_template(std::path::Path::new(&app.view.workspace)).unwrap();
    std::fs::write(
        PathBuf::from(&app.view.workspace).join(debug_config::CONFIG_PATH),
        "unfinished JSON",
    )
    .unwrap();
    refresh(&mut app);
    assert_eq!(app.view.debug.status, DebugStatus::Failed);
    assert!(app
        .view
        .debug
        .message
        .as_deref()
        .unwrap()
        .contains("Configuración de depuración"));
    assert!(app.debugger.session.is_none());
}

#[test]
fn debugger_late_stack_scopes_and_variables_do_not_replace_current_inspection() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 12);
    app.debugger.stack_request = Some(100);
    for (request_id, stop_id, thread_id) in [(99, 12, 7), (100, 11, 7), (100, 12, 8)] {
        app.handle_debug_event(DebugEvent::StackTrace {
            request_id,
            stop_id,
            thread_id,
            frames: vec![frame(5, None)],
            total_frames: Some(1),
        });
        assert!(app.debugger.frames.is_empty());
        assert_eq!(app.debugger.stack_request, Some(100));
    }
    app.handle_debug_event(DebugEvent::StackTrace {
        request_id: 100,
        stop_id: 12,
        thread_id: 7,
        frames: vec![frame(6, None)],
        total_frames: Some(1),
    });
    assert_eq!(app.debugger.frame, Some(6));
    app.debugger.scope_request = Some(101);
    let scope = || Scope {
        name: "Locals".into(),
        variables_reference: 80,
        expensive: true,
        named_variables: Some(1),
        indexed_variables: None,
    };
    for (request_id, stop_id, frame_id) in [(100, 12, 6), (101, 11, 6), (101, 12, 5)] {
        app.handle_debug_event(DebugEvent::Scopes {
            request_id,
            stop_id,
            frame_id,
            scopes: vec![scope()],
        });
        assert!(app.debugger.nodes.is_empty());
    }
    app.handle_debug_event(DebugEvent::Scopes {
        request_id: 101,
        stop_id: 12,
        frame_id: 6,
        scopes: vec![scope()],
    });
    app.debugger.variable_requests.insert(102, "scope:0".into());
    let value = || Variable {
        name: "counter".into(),
        value: "42".into(),
        type_name: Some("int".into()),
        variables_reference: 0,
        evaluate_name: None,
        named_variables: None,
        indexed_variables: None,
    };
    app.handle_debug_event(DebugEvent::Variables {
        request_id: 102,
        stop_id: 11,
        variables_reference: 80,
        variables: vec![value()],
    });
    assert!(app.debugger.nodes["scope:0"].children.is_empty());
    assert_eq!(
        app.debugger.variable_requests.get(&102).map(String::as_str),
        Some("scope:0")
    );
    app.handle_debug_event(DebugEvent::Variables {
        request_id: 102,
        stop_id: 12,
        variables_reference: 80,
        variables: vec![value()],
    });
    assert_eq!(app.debugger.nodes["scope:0/0"].view.value, "42");
}

#[test]
fn debugger_failed_inspection_releases_only_its_correlated_request() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 4);
    app.debugger.stack_request = Some(31);
    app.debugger.scope_request = Some(32);
    app.handle_debug_event(DebugEvent::RequestFailed {
        request_id: Some(31),
        command: "stackTrace".into(),
        message: "Unavailable".into(),
    });
    assert_eq!(
        app.debugger.stack_request, None,
        "failed stack inspection must be retryable"
    );
    assert_eq!(
        app.debugger.scope_request,
        Some(32),
        "unrelated inspection is preserved"
    );
    app.handle_debug_event(DebugEvent::RequestFailed {
        request_id: Some(32),
        command: "scopes".into(),
        message: "Unavailable".into(),
    });
    assert_eq!(app.debugger.scope_request, None);
    assert_eq!(app.view.debug.status, DebugStatus::Stopped);
}

#[test]
fn debugger_stale_inspection_failure_does_not_replace_current_stop_message() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 5);
    app.debugger.stack_request = Some(50);
    assert_eq!(
        app.view.debug.stop_reason.as_deref(),
        Some("punto de interrupción")
    );
    let current = app.view.debug.message.clone();
    app.handle_debug_event(DebugEvent::RequestFailed {
        request_id: Some(49),
        command: "stackTrace".into(),
        message: "Discarded response from an earlier suspended state".into(),
    });
    assert_eq!(app.view.debug.message, current);
    assert_eq!(app.debugger.stack_request, Some(50));
}

#[test]
fn debugger_failed_step_preserves_stopped_inspection_until_execution_is_confirmed() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 6);
    app.debugger.frames = vec![frame(8, None)];
    app.debugger.frame = Some(8);
    app.debugger.control_request = Some(60);
    let scope = app.view.debug.scope.clone();
    app.handle_debug_event(DebugEvent::RequestFailed {
        request_id: Some(60),
        command: "next".into(),
        message: "The selected thread cannot step".into(),
    });
    assert_eq!(app.debugger.control_request, None);
    assert_eq!(app.view.debug.status, DebugStatus::Stopped);
    assert_eq!(app.debugger.frame, Some(8));
    assert_eq!(app.debugger.frames.len(), 1);
    assert_eq!(app.view.debug.scope, scope);

    app.handle_debug_event(DebugEvent::Continued {
        thread_id: Some(7),
        all_threads_continued: true,
    });
    assert_eq!(app.view.debug.status, DebugStatus::Running);
    assert!(app.debugger.frames.is_empty());
    assert!(app.debugger.frame.is_none());
    assert_ne!(app.view.debug.scope, scope);
}

#[test]
fn debugger_variable_failure_stops_only_the_matching_loader() {
    let (_root, mut app) = fixture();
    stopped(&mut app, 3);
    for (id, key) in [(20, "locals"), (21, "globals")] {
        app.debugger.nodes.insert(key.into(), node(key, id as i64));
        app.debugger.roots.push(key.into());
        app.debugger.variable_requests.insert(id, key.into());
    }
    app.handle_debug_event(DebugEvent::RequestFailed {
        request_id: Some(20),
        command: "variables".into(),
        message: "Cannot expand".into(),
    });
    assert!(!app.debugger.nodes["locals"].view.loading);
    assert!(app.debugger.nodes["locals"]
        .view
        .error
        .as_ref()
        .unwrap()
        .contains("Cannot expand"));
    assert!(app.debugger.nodes["globals"].view.loading);
    assert!(app.debugger.variable_requests.contains_key(&21));
}

#[test]
fn debugger_projection_is_revision_scoped_and_never_moves_or_edits_source() {
    let (_root, mut app) = fixture();
    let path = PathBuf::from(&app.view.workspace).join("main.rs");
    let source = "fn main() {\n    let café = 1;\n}\n";
    std::fs::write(&path, source).unwrap();
    let scope = app.dock_scope();
    app.dock.open(
        &scope,
        DockTabKind::FilePreview {
            path: "main.rs".into(),
        },
    );
    app.dock.set_collapsed(&scope, false);
    app.sync_dock_view();
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .view
        .code_editor
        .as_ref()
        .is_none_or(|view| view.loading)
    {
        app.poll_code_editor(&ctx);
        assert!(Instant::now() < deadline, "isolated editor failed to load");
        std::thread::sleep(Duration::from_millis(2));
    }
    let (_, _, controller, before) = app.active_editor().unwrap();
    stopped(&mut app, 9);
    app.debugger.frames = vec![frame(7, Some(path.clone()))];
    app.debugger.frame = Some(7);
    app.debugger.verified.insert(
        path,
        vec![Breakpoint {
            id: Some(11),
            verified: true,
            requested_line: Some(2),
            line: Some(2),
            ..Default::default()
        }],
    );
    app.view.editor_workbench.breakpoint_rows = vec![1].into();
    assert!(app.project_debug_editor());
    assert_eq!(
        app.view
            .editor_workbench
            .debug_execution
            .as_ref()
            .unwrap()
            .row,
        1
    );
    assert_eq!(
        app.view.editor_workbench.debug_breakpoints[0].state,
        CodeBreakpointState::Verified
    );
    let projected = app.view.editor_workbench.debug_breakpoints.clone();
    assert!(!app.project_debug_editor());
    assert!(Arc::ptr_eq(
        &projected,
        &app.view.editor_workbench.debug_breakpoints
    ));
    let absolute = PathBuf::from(&app.view.workspace).join("main.rs");
    app.debugger.verified.get_mut(&absolute).unwrap()[0].line = Some(3);
    app.debugger.projection = None;
    assert!(app.project_debug_editor());
    assert_eq!(
        app.view.editor_workbench.debug_breakpoints[0].row, 2,
        "adapter relocation must move the visible marker"
    );
    assert_eq!(
        app.debug_requested_breakpoint_row("main.rs", 2),
        1,
        "clicking the relocated dot removes the original request"
    );
    let after = controller.snapshot().unwrap();
    assert_eq!(after.selection(), before.selection());
    assert_eq!(after.revision(), before.revision());
    assert_eq!(after.text_range(0..after.char_count()).unwrap(), source);

    controller
        .shared_buffer()
        .lock()
        .unwrap()
        .apply_transaction(
            &[openide_editor::TextEdit {
                range: 0..0,
                replacement: "// unsaved\n".into(),
            }],
            Selection::caret(0),
            openide_editor::EditKind::Paste,
            1,
        )
        .unwrap();
    assert!(app.project_debug_editor());
    assert!(app.view.editor_workbench.debug_execution.is_none());
    assert_eq!(
        app.view.editor_workbench.debug_breakpoints[0].state,
        CodeBreakpointState::Disabled
    );
    // A new marker in an unsaved source must stay local: sending this row to
    // DAP would target a different line in the on-disk program.
    app.apply_action(
        UiAction::Clicked {
            id: "editor_breakpoint".into(),
        },
        &ctx,
    );
    assert!(app.debug_source_breakpoints().is_empty());
    controller.shared_buffer().lock().unwrap().mark_saved();
    app.sync_editor_structure();
    assert_eq!(app.debug_source_breakpoints()[0].lines, vec![1]);
}

#[cfg(unix)]
#[test]
fn debugger_configuration_never_follows_a_workspace_directory_symlink() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, workspace.join(".openide")).unwrap();
    assert!(debug_config::ensure_template(&workspace).is_err());
    assert!(
        !outside.join("debug.json").exists(),
        "Configure must not write outside the workspace"
    );

    std::fs::write(outside.join("debug.json"), r#"{"version":1,"configurations":[{"name":"Outside","adapter":{"command":"unused-adapter"},"request":"launch","arguments":{}}]}"#).unwrap();
    assert!(
        debug_config::read(&workspace).is_err(),
        "configuration ownership must match the workspace"
    );
}

fn click(app: &mut AgentsWindow, ctx: &egui::Context, id: String) {
    assert!(app.apply_debug_action(&UiAction::Clicked { id }, ctx));
}

fn variable(name: &str, value: &str) -> Variable {
    Variable {
        name: name.into(),
        value: value.into(),
        type_name: None,
        variables_reference: 0,
        evaluate_name: None,
        named_variables: None,
        indexed_variables: None,
    }
}

/// One stop in `frame-6` whose only scope answers with `variables`.
fn inspect(app: &mut AgentsWindow, stop: u64, variables: Vec<Variable>) {
    stopped(app, stop);
    app.debugger.frames = vec![frame(6, None)];
    app.debugger.frame = Some(6);
    app.debugger.scope_request = Some(1);
    app.handle_debug_event(DebugEvent::Scopes {
        request_id: 1,
        stop_id: stop,
        frame_id: 6,
        scopes: vec![Scope {
            name: "Locals".into(),
            variables_reference: 80,
            expensive: true,
            named_variables: None,
            indexed_variables: None,
        }],
    });
    app.debugger.variable_requests.insert(2, "scope:0".into());
    app.debugger.nodes.get_mut("scope:0").unwrap().view.expanded = true;
    app.handle_debug_event(DebugEvent::Variables {
        request_id: 2,
        stop_id: stop,
        variables_reference: 80,
        variables,
    });
}

#[test]
fn a_value_that_differs_from_the_previous_stop_is_marked_and_a_full_page_says_so() {
    let (_root, mut app) = fixture();
    inspect(
        &mut app,
        1,
        vec![variable("counter", "1"), variable("name", "a")],
    );
    assert!(app.view.debug.variables.iter().all(|row| !row.changed));
    app.handle_debug_event(DebugEvent::Continued {
        thread_id: Some(7),
        all_threads_continued: true,
    });
    assert_eq!(app.view.debug.stop_reason, None);
    inspect(
        &mut app,
        2,
        vec![variable("counter", "2"), variable("name", "a")],
    );
    let changed: Vec<_> = app
        .view
        .debug
        .variables
        .iter()
        .map(|row| (row.name.as_str(), row.changed))
        .collect();
    assert_eq!(
        changed,
        [("Locals", false), ("counter", true), ("name", false)]
    );

    app.handle_debug_event(DebugEvent::Continued {
        thread_id: Some(7),
        all_threads_continued: true,
    });
    let page = crate::debug_ui::PAGE;
    inspect(
        &mut app,
        3,
        (0..page + 5).map(|index| variable(&format!("v{index}"), "0")).collect(),
    );
    let rows = &app.view.debug.variables;
    assert_eq!(rows.len(), page + 2, "the scope, one page and its note");
    assert!(rows[page + 1].note && rows[page + 1].name.contains("100"));
}

#[test]
fn the_paused_line_reveals_its_source_and_copy_reads_the_host_value() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    let path = PathBuf::from(&app.view.workspace).join("main.rs");
    std::fs::write(&path, "fn main() {\n    let value = 1;\n}\n").unwrap();
    inspect(&mut app, 1, vec![variable("counter", "41")]);
    app.debugger.frames = vec![frame(6, Some(path))];
    let scope = app.view.debug.scope.clone();
    click(&mut app, &ctx, crate::debug_ui::row_action(&scope, "reveal", 6));
    assert_eq!(
        app.editor_services
            .pending_navigation
            .as_ref()
            .map(|(path, position)| (path.clone(), position.line)),
        Some((PathBuf::from("main.rs"), 1))
    );
    assert_eq!(
        app.view.debug.variables.len(),
        2,
        "revealing keeps the inspection"
    );
    assert_eq!(app.view.debug.scope, scope);

    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        for (what, key) in [("value", "scope:0/0"), ("name", "scope:0/0"), ("value", "gone")] {
            click(
                &mut app,
                ui.ctx(),
                crate::debug_ui::row_action(&scope, "copy", (key, what)),
            );
        }
    });
    output.textures_delta.clear();
    let copied: Vec<_> = output
        .platform_output
        .commands
        .iter()
        .filter_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, ["41", "counter"]);
}

#[test]
fn sidebar_breakpoint_actions_change_what_the_adapter_and_the_gutter_get() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    open_fixture_document(&mut app);
    let add = |app: &mut AgentsWindow| {
        app.apply_action(
            UiAction::Clicked {
                id: "editor_breakpoint".into(),
            },
            &ctx,
        );
        app.poll_debugger(&ctx);
    };
    add(&mut app);
    let scope = app.view.debug.scope.clone();
    let mark = ("main.rs", 1);
    let lines = |app: &AgentsWindow| -> Vec<u32> {
        app.debug_source_breakpoints()
            .into_iter()
            .flat_map(|source| source.lines)
            .collect()
    };
    assert_eq!(lines(&app), [1]);
    assert!(app.view.debug.breakpoints[0].enabled);

    // Disabled: still a mark in the editor, no longer a line for the adapter.
    click(
        &mut app,
        &ctx,
        crate::debug_ui::row_action(&scope, "breakpoint_toggle", mark),
    );
    app.poll_debugger(&ctx);
    assert!(lines(&app).is_empty());
    assert_eq!(&*app.view.editor_workbench.breakpoint_rows, &[0]);
    assert!(!app.view.debug.breakpoints[0].enabled);
    assert_eq!(
        app.view.editor_workbench.debug_breakpoints[0].state,
        CodeBreakpointState::Disabled
    );

    // The header switches every mark at once, on again when none is enabled.
    click(
        &mut app,
        &ctx,
        crate::debug_ui::action(&scope, "toggle_breakpoints"),
    );
    app.poll_debugger(&ctx);
    assert_eq!(lines(&app), [1]);
    click(
        &mut app,
        &ctx,
        crate::debug_ui::action(&scope, "toggle_breakpoints"),
    );
    app.poll_debugger(&ctx);
    assert!(lines(&app).is_empty());

    click(
        &mut app,
        &ctx,
        crate::debug_ui::row_action(&scope, "breakpoint_remove", mark),
    );
    app.poll_debugger(&ctx);
    assert!(app.view.debug.breakpoints.is_empty());
    assert!(app.view.editor_workbench.breakpoint_rows.is_empty());

    // Removing all asks first; cancelling keeps every mark.
    add(&mut app);
    let confirm = crate::debug_ui::action(&scope, "clear_breakpoints_confirm");
    click(&mut app, &ctx, confirm.clone());
    assert_eq!(app.view.debug.breakpoints.len(), 1, "not asked, not removed");
    click(&mut app, &ctx, crate::debug_ui::action(&scope, "clear_breakpoints"));
    assert!(app.view.debug.confirm_clear_breakpoints);
    click(
        &mut app,
        &ctx,
        crate::debug_ui::action(&scope, "clear_breakpoints_cancel"),
    );
    assert!(!app.view.debug.confirm_clear_breakpoints);
    assert_eq!(lines(&app), [1]);
    // Hiding the panel takes its question with it.
    click(&mut app, &ctx, crate::debug_ui::action(&scope, "clear_breakpoints"));
    click(&mut app, &ctx, crate::debug_ui::action(&scope, "hide"));
    assert!(!app.view.debug.confirm_clear_breakpoints);
    click(&mut app, &ctx, "ide_debug_toggle".into());
    click(&mut app, &ctx, crate::debug_ui::action(&scope, "clear_breakpoints"));
    click(&mut app, &ctx, confirm);
    app.poll_debugger(&ctx);
    assert!(!app.view.debug.confirm_clear_breakpoints);
    assert!(app.view.debug.breakpoints.is_empty());
    assert!(lines(&app).is_empty());
}

#[test]
fn the_adapters_answer_reaches_the_sidebar_mark() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    open_fixture_document(&mut app);
    app.apply_action(
        UiAction::Clicked {
            id: "editor_breakpoint".into(),
        },
        &ctx,
    );
    app.poll_debugger(&ctx);
    assert_eq!(app.view.debug.breakpoints[0].verified, None);
    app.debugger.verified.insert(
        PathBuf::from(&app.view.workspace).join("main.rs"),
        vec![Breakpoint {
            verified: false,
            message: Some("No code here".into()),
            requested_line: Some(1),
            ..Default::default()
        }],
    );
    let marks = app.debug_breakpoint_views();
    assert_eq!(marks[0].verified, Some(false));
    assert_eq!(marks[0].message.as_deref(), Some("No code here"));
}

#[test]
fn open_sections_are_saved_with_the_ide_layout() {
    let (root, mut app) = fixture();
    let ctx = egui::Context::default();
    let scope = app.view.debug.scope.clone();
    click(
        &mut app,
        &ctx,
        crate::debug_ui::row_action(&scope, "section", "variables"),
    );
    assert!(!app.view.debug.sections.variables);
    assert!(app.view.debug.sections.stack && app.view.debug.sections.breakpoints);
    let saved = crate::ide_workspace::IdeWorkspaceState::load(&root.path().join("state"));
    assert!(!saved.debug_sections.variables && saved.debug_sections.stack);
    // A reset of the debugger's view, as on a workspace switch, keeps them.
    app.view.debug = DebugView::default();
    app.poll_debugger(&ctx);
    assert!(!app.view.debug.sections.variables);
}

#[test]
fn restart_is_a_stop_that_starts_the_same_configuration_again() {
    let (_root, mut app) = fixture();
    let ctx = egui::Context::default();
    let scope = app.view.debug.scope.clone();
    // Without a session there is nothing to restart.
    click(&mut app, &ctx, crate::debug_ui::action(&scope, "restart"));
    assert!(!app.debugger.restart_after_stop && app.debugger.session.is_none());
    assert!(!app.view.debug.controls.restart);

    // The worker's own disconnect is what starts the next session. A missing
    // executable exercises it without launching a process.
    app.debugger.configurations = vec![serde_json::from_value(serde_json::json!({
        "name": "Never launched", "adapter": {"command": "never-created-adapter"},
        "request": "launch", "arguments": {}
    }))
    .unwrap()];
    app.view.debug.status = DebugStatus::Stopping;
    app.debugger.restart_after_stop = true;
    app.handle_debug_event(DebugEvent::Disconnected {
        reason: "Adapter disconnected".into(),
    });
    assert!(!app.debugger.restart_after_stop);
    assert!(app.debugger.session.is_some());
    assert_eq!(app.view.debug.status, DebugStatus::Starting);
    assert_eq!(app.view.debug.message, None);
    app.refresh_debug_controls();
    assert!(app.view.debug.controls.restart && app.view.debug.controls.stop);

    // A session that never started is a failure, not something to loop on.
    app.debugger.restart_after_stop = true;
    app.handle_debug_event(DebugEvent::Disconnected {
        reason: "Cannot start debug adapter: missing".into(),
    });
    assert_eq!(app.view.debug.status, DebugStatus::Failed);
    assert!(app.debugger.session.is_none() && !app.debugger.restart_after_stop);
}
