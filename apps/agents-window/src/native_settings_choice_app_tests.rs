use super::*;
use openide_ui::{HitKind, Key, Point, UiEvent};

fn fixture(root: &Path, page: SettingsPage) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.to_owned());
    app.view.show_settings = true;
    app.view.settings_page = page;
    app.view.settings_choice_menu_open = None;
    app
}

fn apply(app: &mut AgentsWindow, id: &str) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn frame(app: &mut AgentsWindow, width: f32) -> openide_ui::Frame {
    app.tree.set_root(crate::settings_ui::build_settings_page(
        &app.view,
        app.view.settings_page,
        width,
        true,
    ));
    app.tree.set_reduced_motion(true);
    app.tree.layout(Size::new(width, 2_400.0))
}

fn export_visual(frame: &openide_ui::Frame, width: f32, choice: &str) {
    let Some(directory) = std::env::var_os("OPENIDE_SETTINGS_CHOICE_VISUAL_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("settings-choice-{choice}-{width:.0}.svg"));
    crate::settings_ui::visual_qa::write_frame_svg(frame, &path, width, 2_400.0).unwrap();
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let svg = std::fs::read_to_string(&path).unwrap();
    let tree = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 2_400).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
}

fn click(app: &mut AgentsWindow, frame: &openide_ui::Frame, id: &str) {
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == HitKind::Button)
        .unwrap_or_else(|| panic!("missing settings choice action {id}"))
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_eq!(frame.hit_test(point), Some(id));
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert_eq!(actions, vec![UiAction::Clicked { id: id.into() }]);
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

#[test]
fn appearance_menus_survive_the_full_dispatcher_until_dismissed_or_chosen() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Appearance);
    for width in [360.0, 900.0] {
        for choice in [
            "appearance_theme_selection",
            "appearance_motion",
            "appearance_ui_font",
            "appearance_code_font",
        ] {
            let trigger = format!("settings_choice_toggle:{choice}");
            let closed = frame(&mut app, width);
            click(&mut app, &closed, &trigger);
            assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some(choice));
            let opened = frame(&mut app, width);
            assert!(opened
                .access
                .iter()
                .any(|node| node.id == trigger && node.expanded == Some(true)));
            assert!(opened
                .regions
                .iter()
                .any(|region| region.id == "popover_surface:settings_choice_dismiss"));
            for action in app.tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }) {
                app.apply_action(action, &egui::Context::default());
            }
            assert!(app.view.settings_choice_menu_open.is_none());
            let closed = frame(&mut app, width);
            assert!(closed
                .access
                .iter()
                .any(|node| node.id == trigger && node.expanded == Some(false)));
        }
    }
    apply(&mut app, "settings_choice_toggle:appearance_motion");
    let opened = frame(&mut app, 900.0);
    click(&mut app, &opened, "appearance_motion:always");
    assert!(app.view.settings_choice_menu_open.is_none());
    assert_eq!(
        SettingsPreferences::load(root.path())
            .appearance
            .reduce_motion,
        crate::appearance_preferences::MotionPreference::Always
    );
    crate::appearance_preferences::set_current(&Default::default());
    crate::theme_mode::set_appearance(&Default::default());
}

#[test]
fn settings_choice_toggles_are_page_scoped_and_keep_only_one_open_menu() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Data);
    apply(&mut app, "settings_choice_toggle:organization");
    assert_eq!(
        app.view.settings_choice_menu_open.as_deref(),
        Some("organization")
    );
    apply(&mut app, "settings_choice_toggle:sort");
    assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some("sort"));
    apply(&mut app, "settings_choice_toggle:sort");
    assert!(app.view.settings_choice_menu_open.is_none());
    for invalid in ["isolation", "unknown", "", "organization:extra"] {
        apply(&mut app, &format!("settings_choice_toggle:{invalid}"));
        assert!(app.view.settings_choice_menu_open.is_none());
    }
    app.view.settings_page = SettingsPage::Agent;
    for invalid in ["organization", "sort", "unknown"] {
        apply(&mut app, &format!("settings_choice_toggle:{invalid}"));
        assert!(app.view.settings_choice_menu_open.is_none());
    }
    apply(&mut app, "settings_choice_toggle:isolation");
    assert_eq!(
        app.view.settings_choice_menu_open.as_deref(),
        Some("isolation")
    );
    apply(&mut app, "settings_choice_dismiss");
    assert!(app.view.settings_choice_menu_open.is_none());
    app.view.show_settings = false;
    apply(&mut app, "settings_choice_toggle:isolation");
    assert!(app.view.settings_choice_menu_open.is_none());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn browser_permission_and_mcp_scope_menus_work_through_painted_controls() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Browser);
    app.set_browser_site(
        "https://example.test",
        Some(crate::browser_sites::SiteAccess::Block),
    )
    .unwrap();
    for width in [360.0, 900.0] {
        for choice in ["browser_site_draft", "browser_site:0"] {
            let trigger = format!("settings_choice_toggle:{choice}");
            let closed = frame(&mut app, width);
            click(&mut app, &closed, &trigger);
            assert_eq!(app.view.settings_choice_menu_open.as_deref(), Some(choice));
            let opened = frame(&mut app, width);
            assert!(opened
                .access
                .iter()
                .any(|node| node.id == trigger && node.expanded == Some(true)));
            export_visual(&opened, width, &choice.replace(':', "-"));
            apply(&mut app, "settings_choice_dismiss");
        }
        app.view.settings_page = SettingsPage::Mcp;
        let closed = frame(&mut app, width);
        click(&mut app, &closed, "settings_choice_toggle:mcp_scope");
        let opened = frame(&mut app, width);
        export_visual(&opened, width, "mcp-scope");
        click(&mut app, &opened, "mcp_scope:project");
        assert_eq!(
            app.view.mcp_settings_scope,
            crate::mcp_settings::McpSettingsScope::Project
        );
        assert!(app.view.settings_choice_menu_open.is_none());
        apply(&mut app, "mcp_scope:user");
        app.view.settings_page = SettingsPage::Browser;
    }
    let closed = frame(&mut app, 900.0);
    click(&mut app, &closed, "settings_choice_toggle:browser_site:0");
    let opened = frame(&mut app, 900.0);
    click(&mut app, &opened, "browser_site_set:0:ask");
    assert_eq!(
        crate::browser_sites::BrowserSites::load(root.path())
            .unwrap()
            .access("https://example.test"),
        crate::browser_sites::SiteAccess::Ask
    );
    for invalid in [
        "browser_site:",
        "browser_site:-1",
        "browser_site:256",
        "browser_site:0x0",
        "mcp_scope",
    ] {
        apply(&mut app, &format!("settings_choice_toggle:{invalid}"));
        assert!(app.view.settings_choice_menu_open.is_none());
    }
}

#[test]
fn integrated_settings_pages_fit_compact_width_and_export_review_frames() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Browser);
    app.set_browser_site(
        "https://example.test",
        Some(crate::browser_sites::SiteAccess::Block),
    )
    .unwrap();
    let server = crate::mcp_settings::McpServerSettings {
        id: "example-server".into(),
        enabled: true,
        program: "example-mcp".into(),
        args: Vec::new(),
        cwd: None,
        credential_env_names: Vec::new(),
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: Vec::new(),
    };
    McpSettings {
        servers: vec![server.clone()],
    }
    .save(root.path(), Path::new(&app.view.workspace))
    .unwrap();
    let mut overridden = server;
    overridden.enabled = false;
    McpSettings {
        servers: vec![overridden],
    }
    .save_scope(
        root.path(),
        Path::new(&app.view.workspace),
        crate::mcp_settings::McpSettingsScope::Project,
    )
    .unwrap();
    app.refresh_mcp_settings();
    app.view.mcp_inspection.last.insert(
        "example-server".into(),
        crate::mcp_inspection::McpServerStatus::Connected { tools: 12 },
    );
    for page in [
        SettingsPage::Browser,
        SettingsPage::Subagents,
        SettingsPage::Mcp,
    ] {
        app.view.settings_page = page;
        for width in [360.0, 900.0] {
            let painted = frame(&mut app, width);
            for region in &painted.regions {
                if matches!(region.kind, HitKind::Button | HitKind::TextInput) {
                    assert!(
                        region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.01,
                        "{page:?} at {width}: {} {:?}",
                        region.id,
                        region.rect
                    );
                }
            }
            export_visual(
                &painted,
                width,
                page.action_id().trim_start_matches("settings_page:"),
            );
        }
    }
}

#[test]
fn settings_choice_page_navigation_drops_the_previous_menu_and_ignores_stale_toggles() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Data);
    apply(&mut app, "settings_choice_toggle:organization");
    apply(&mut app, "settings_page:agent");
    assert_eq!(app.view.settings_page, SettingsPage::Agent);
    assert!(app.view.settings_choice_menu_open.is_none());
    apply(&mut app, "settings_choice_toggle:sort");
    assert!(app.view.settings_choice_menu_open.is_none());
    apply(&mut app, "settings_choice_toggle:isolation");
    assert_eq!(
        app.view.settings_choice_menu_open.as_deref(),
        Some("isolation")
    );
    apply(&mut app, "settings_page:data");
    assert_eq!(app.view.settings_page, SettingsPage::Data);
    assert!(app.view.settings_choice_menu_open.is_none());
    apply(&mut app, "settings_choice_toggle:isolation");
    assert!(app.view.settings_choice_menu_open.is_none());
    apply(&mut app, "settings_choice_toggle:sort");
    apply(&mut app, "settings_page:general");
    assert!(app.view.settings_choice_menu_open.is_none());
    assert!(app.active_run.is_none() && app.active_resume.is_none());
}

#[test]
fn settings_choice_real_popovers_dispatch_escape_and_keep_controls_inside_narrow_viewports() {
    for width in [360.0, 900.0] {
        for (page, choice) in [
            (SettingsPage::Data, "organization"),
            (SettingsPage::Data, "sort"),
            (SettingsPage::Agent, "isolation"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut app = fixture(root.path(), page);
            let closed = frame(&mut app, width);
            assert!(!closed
                .regions
                .iter()
                .any(|region| region.id == "popover_surface:settings_choice_dismiss"));
            click(
                &mut app,
                &closed,
                &format!("settings_choice_toggle:{choice}"),
            );
            let open = frame(&mut app, width);
            export_visual(&open, width, choice);
            let surface = open
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:settings_choice_dismiss")
                .expect("settings choice uses the shared dropdown popover")
                .rect;
            assert!(surface.x >= 0.0 && surface.x + surface.width <= width);
            assert!(surface.width <= 256.0);
            let actions = app.tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            });
            assert_eq!(
                actions,
                vec![UiAction::Clicked {
                    id: "settings_choice_dismiss".into()
                }]
            );
            for action in actions {
                app.apply_action(action, &egui::Context::default());
            }
            assert!(app.view.settings_choice_menu_open.is_none());
            let dismissed = frame(&mut app, width);
            assert!(!dismissed
                .regions
                .iter()
                .any(|region| region.id == "popover_surface:settings_choice_dismiss"));
        }
    }
}

#[test]
fn settings_choice_organization_and_sort_pointer_selection_persist_after_restart() {
    for width in [360.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let mut app = fixture(root.path(), SettingsPage::Data);
        let baseline_tools = app.agent_tool_settings;
        let closed = frame(&mut app, width);
        click(&mut app, &closed, "settings_choice_toggle:organization");
        let options = frame(&mut app, width);
        click(&mut app, &options, "session_organize:recent");
        assert_eq!(app.view.session_organization, SessionOrganization::Recent);
        assert!(app.view.settings_choice_menu_open.is_none());
        let closed = frame(&mut app, width);
        click(&mut app, &closed, "settings_choice_toggle:sort");
        let options = frame(&mut app, width);
        click(&mut app, &options, "session_sort:created");
        assert_eq!(app.view.session_sort, SessionSort::Created);
        assert!(app.view.settings_choice_menu_open.is_none());
        let saved = SidebarPreferences::load(root.path());
        assert_eq!(saved.organization, SessionOrganization::Recent);
        assert_eq!(saved.sort, SessionSort::Created);
        let reopened = AgentsWindow::with_data_root(root.path().to_owned());
        assert_eq!(
            reopened.view.session_organization,
            SessionOrganization::Recent
        );
        assert_eq!(reopened.view.session_sort, SessionSort::Created);
        assert_eq!(app.agent_tool_settings, baseline_tools);
        assert!(app.active_run.is_none() && app.active_resume.is_none());
    }
}

#[test]
fn settings_choice_isolation_keeps_unsupported_profile_disabled_and_never_executes_a_command() {
    for width in [360.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let mut settings = AgentToolSettings::default();
        settings.set_command_isolation(CommandIsolation::WorkspaceWriteOffline);
        settings.save(root.path()).unwrap();
        let mut app = fixture(root.path(), SettingsPage::Agent);
        assert_eq!(
            app.view.native_command_isolation,
            CommandIsolation::WorkspaceWriteOffline
        );
        let baseline = std::fs::read(root.path().join("agent-tool-settings.json")).unwrap();
        let closed = frame(&mut app, width);
        click(&mut app, &closed, "settings_choice_toggle:isolation");
        let options = frame(&mut app, width);
        if !CommandIsolation::WorkspaceWriteOffline.is_available_on_host() {
            assert!(!options.regions.iter().any(|region| region.id
                == "command_isolation:workspace_write_offline"
                && region.kind == HitKind::Button));
            assert!(!app.tree.focus("command_isolation:workspace_write_offline"));
            apply(&mut app, "command_isolation:workspace_write_offline");
            assert_eq!(
                app.view.native_command_isolation,
                CommandIsolation::WorkspaceWriteOffline
            );
            assert_eq!(
                std::fs::read(root.path().join("agent-tool-settings.json")).unwrap(),
                baseline
            );
            apply(&mut app, "settings_choice_toggle:isolation");
        }
        let options = frame(&mut app, width);
        click(&mut app, &options, "command_isolation:host_unrestricted");
        assert_eq!(
            app.view.native_command_isolation,
            CommandIsolation::HostUnrestricted
        );
        assert_eq!(
            AgentToolSettings::load(root.path())
                .unwrap()
                .command_isolation(),
            CommandIsolation::HostUnrestricted
        );
        assert!(app.view.settings_choice_menu_open.is_none());
        assert!(app.active_run.is_none() && app.active_resume.is_none());
        assert!(app.view.native_commands.is_empty());
        assert!(!app
            .native_command_sessions
            .has_running_in_worktree(root.path())
            .unwrap());
    }
}

#[test]
fn hosted_web_search_is_chosen_from_the_agent_page_for_the_provider_that_supports_it() {
    use openide_harness::HostedWebSearchMode;
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path(), SettingsPage::Agent);
    let ctx = egui::Context::default();
    let ids = |app: &AgentsWindow| {
        let mut tree = openide_ui::UiTree::new(crate::settings_ui::build_settings_page(
            &app.view,
            SettingsPage::Agent,
            900.0,
            false,
        ));
        tree.layout(openide_ui::Size::new(900.0, 3000.0))
            .regions
            .iter()
            .map(|region| region.id.clone())
            .collect::<Vec<_>>()
    };
    let toggle = "settings_choice_toggle:native_web_search".to_owned();

    app.view.native_provider_id = "custom".into();
    assert!(
        !ids(&app).contains(&toggle),
        "other providers have no hosted search"
    );

    app.view.native_provider_id = "openai".into();
    assert!(ids(&app).contains(&toggle));
    app.apply_action(UiAction::Clicked { id: toggle.clone() }, &ctx);
    assert_eq!(
        app.view.settings_choice_menu_open.as_deref(),
        Some("native_web_search")
    );
    for id in [
        "native_web_search:off",
        "native_web_search:cached",
        "native_web_search:indexed",
        "native_web_search:live",
    ] {
        assert!(ids(&app).iter().any(|region| region == id), "missing {id}");
    }
    app.apply_action(
        UiAction::Clicked {
            id: "native_web_search:live".into(),
        },
        &ctx,
    );
    assert_eq!(
        app.view.hosted_web_search_mode,
        Some(HostedWebSearchMode::Live)
    );
    assert!(app.view.settings_choice_menu_open.is_none());
    assert_eq!(
        crate::settings_preferences::SettingsPreferences::load(root.path()).hosted_web_search_mode,
        Some(HostedWebSearchMode::Live)
    );
}

#[test]
fn the_file_tree_follows_the_folder_without_a_manual_refresh() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("keep.md"), "x").unwrap();
    std::fs::write(project.join("src/inner.rs"), "x").unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.open_files_dock();
    let ctx = egui::Context::default();
    let names = |app: &AgentsWindow| {
        app.view
            .file_rows
            .iter()
            .map(|row| row.name.clone())
            .collect::<Vec<_>>()
    };
    let follow_until = |app: &mut AgentsWindow, done: &dyn Fn(&[String]) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.follow_file_tree(&ctx);
            if done(&names(app)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "tree never caught up: {:?}",
                names(app)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(!app.follow_file_tree(&ctx), "nothing changed yet");
    assert!(names(&app).contains(&"keep.md".to_owned()));

    std::fs::write(project.join("created.md"), "x").unwrap();
    follow_until(&mut app, &|rows| rows.contains(&"created.md".to_owned()));
    std::fs::remove_file(project.join("keep.md")).unwrap();
    follow_until(&mut app, &|rows| !rows.contains(&"keep.md".to_owned()));
    std::fs::rename(project.join("created.md"), project.join("moved.md")).unwrap();
    follow_until(&mut app, &|rows| {
        rows.contains(&"moved.md".to_owned()) && !rows.contains(&"created.md".to_owned())
    });

    // A folder is followed from the moment it is expanded.
    let src = app
        .view
        .file_rows
        .iter()
        .position(|row| row.name == "src")
        .unwrap();
    app.apply_action(
        UiAction::Clicked {
            id: format!("file_row:{src}"),
        },
        &ctx,
    );
    follow_until(&mut app, &|rows| {
        rows.iter().any(|row| row.ends_with("inner.rs"))
    });
    std::fs::write(project.join("src/added.rs"), "x").unwrap();
    follow_until(&mut app, &|rows| {
        rows.iter().any(|row| row.ends_with("added.rs"))
    });
    std::fs::remove_file(project.join("src/inner.rs")).unwrap();
    follow_until(&mut app, &|rows| {
        !rows.iter().any(|row| row.ends_with("inner.rs"))
    });
}

#[cfg(unix)]
#[test]
fn a_cli_task_is_created_from_the_home_screen_and_is_its_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let binary = bin.join("claude");
    std::fs::write(&binary, "#!/bin/sh\necho CLI_IS_RUNNING\nsleep 30\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.cli_homes.claude = Some(root.path().join("claude"));
    let ctx = egui::Context::default();
    let home_ids = |app: &AgentsWindow| {
        let mut tree =
            openide_ui::UiTree::new(crate::home::build_home_sized(&app.view, 900.0, 700.0));
        tree.layout(Size::new(900.0, 700.0))
            .regions
            .iter()
            .map(|region| region.id.clone())
            .collect::<Vec<_>>()
    };

    // Nothing is offered until a CLI is actually found.
    *app.login_path.lock().unwrap() = Some("/nonexistent-dir".into());
    assert!(app.follow_agent_clis());
    assert!(
        !app.follow_agent_clis(),
        "an unchanged path is not scanned again"
    );
    assert!(!home_ids(&app).iter().any(|id| id.starts_with("cli_task:")));
    *app.login_path.lock().unwrap() = Some(bin.clone().into_os_string());
    assert!(app.follow_agent_clis());
    assert_eq!(
        home_ids(&app)
            .iter()
            .filter(|id| id.starts_with("cli_task:"))
            .collect::<Vec<_>>(),
        ["cli_task:claude"]
    );

    app.apply_action(
        UiAction::Clicked {
            id: "cli_task:claude".into(),
        },
        &ctx,
    );
    let session = app
        .view
        .selected_session
        .clone()
        .expect("the task is selected");
    assert_eq!(app.view.selected_backend, "cli:claude");
    assert!(crate::view::cli_task(&app.view));
    let row = app
        .view
        .sessions
        .iter()
        .find(|row| row.id == session)
        .unwrap();
    assert_eq!(
        (row.title.as_str(), row.backend.as_str()),
        ("Claude Code", "cli:claude")
    );
    let terminals = app.dock.terminal_tabs(&app.dock_scope());
    assert_eq!(terminals.len(), 1);
    let terminal = terminals[0].id.get();
    let (program, args, _) = app.agent_terminal_command(terminal).unwrap().unwrap();
    assert_eq!(program, binary.as_os_str());
    // Claude Code is given the task's own session to write, and asks as usual.
    let named = args.iter().position(|arg| arg == "--session-id").expect("a session is named");
    let session_name = args[named + 1].to_string_lossy().into_owned();
    assert!(uuid::Uuid::parse_str(&session_name).is_ok(), "{session_name}");
    assert!(!args.iter().any(|arg| arg == "--permission-mode"), "{args:?}");
    // What actually runs in the terminal is the CLI, not a shell.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines = app
            .terminals
            .get(&terminal)
            .map(|session| session.snapshot().lines)
            .unwrap_or_default();
        if lines.iter().any(|line| line.contains("CLI_IS_RUNNING")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the CLI never started: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(app.view.show_terminal_panel);
    // The terminal is the center of the task, not the bottom panel.
    assert_eq!(crate::view::terminal_panel_height(&app.view, 800.0), 0.0);
    let mut tree = openide_ui::UiTree::new(crate::view::build_view_sized(&app.view, 1200.0, 800.0));
    let frame = tree.layout(Size::new(1200.0, 800.0));
    let ids: Vec<_> = frame
        .regions
        .iter()
        .map(|region| region.id.as_str())
        .collect();
    assert!(ids.contains(&"cli_task_restart"), "{ids:?}");
    assert!(!ids.contains(&"prompt"), "a CLI task has no composer");
    let shown: Vec<_> = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(shown.contains(&"Claude Code"), "{shown:?}");
    assert!(
        !shown.contains(&"Problemas"),
        "no bottom panel chrome: {shown:?}"
    );

    // The permissions chosen in the header reach the CLI at its next start;
    // restarting replaces the terminal with a new one for the same CLI.
    app.apply_action(UiAction::Clicked { id: "cli_mode:read_only".into() }, &ctx);
    assert_eq!(app.view.cli_task_mode, crate::cli_tasks::CliMode::ReadOnly);
    assert!(app.view.status.contains("al reiniciar la CLI"), "{}", app.view.status);
    app.apply_action(
        UiAction::Clicked {
            id: "cli_task_restart".into(),
        },
        &ctx,
    );
    let restarted = app.dock.terminal_tabs(&app.dock_scope());
    assert_eq!(restarted.len(), 1);
    assert_ne!(restarted[0].id.get(), terminal);
    let (program, args, _) = app
        .agent_terminal_command(restarted[0].id.get())
        .unwrap()
        .unwrap();
    assert_eq!(program, binary.as_os_str());
    let args: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
    assert!(args.windows(2).any(|pair| pair == ["--permission-mode", "plan"]), "{args:?}");
    assert!(
        args.windows(2).any(|pair| pair == ["--session-id", session_name.as_str()]),
        "named again until it is written: {args:?}"
    );
    // Once Claude Code has written the conversation, a start resumes it.
    let project_sessions = root.path().join("claude/projects/-project");
    std::fs::create_dir_all(&project_sessions).unwrap();
    std::fs::write(project_sessions.join(format!("{session_name}.jsonl")), "{}\n").unwrap();
    app.apply_action(UiAction::Clicked { id: "cli_task_restart".into() }, &ctx);
    let again = app.dock.terminal_tabs(&app.dock_scope());
    let (_, args, _) = app.agent_terminal_command(again[0].id.get()).unwrap().unwrap();
    let args: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
    assert!(args.windows(2).any(|pair| pair == ["--resume", session_name.as_str()]), "{args:?}");

    // Selecting the task again reuses its terminal instead of starting another.
    app.open_session(&session);
    assert_eq!(app.dock.terminal_tabs(&app.dock_scope()).len(), 1);

    // A native task keeps the ordinary layout, and an unknown CLI starts nothing.
    app.new_session();
    assert!(!crate::view::cli_task(&app.view));
    let before = app.view.sessions.len();
    app.apply_action(
        UiAction::Clicked {
            id: "cli_task:unknown".into(),
        },
        &ctx,
    );
    assert_eq!(app.view.sessions.len(), before);
}

#[cfg(unix)]
#[test]
fn ide_agent_tabs_switch_freely_between_cli_tasks_and_close_to_their_neighbour() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex"] {
        let binary = bin.join(name);
        std::fs::write(&binary, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = project.to_string_lossy().into_owned();
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    *app.login_path.lock().unwrap() = Some(bin.clone().into_os_string());
    app.follow_agent_clis();
    let ctx = egui::Context::default();
    let click = |app: &mut AgentsWindow, id: &str| {
        app.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
        app.follow_ide_agent_tabs();
    };

    // The menu offers the native agent and each installed CLI, and closes on a choice.
    click(&mut app, "ide_agent_new_toggle");
    assert!(app.view.ide.agent_new_open);
    let mut tree = openide_ui::UiTree::new(crate::view::build_view_sized(&app.view, 1500.0, 800.0));
    let ids: Vec<_> = tree
        .layout(Size::new(1500.0, 800.0))
        .regions
        .iter()
        .map(|region| region.id.clone())
        .collect();
    for id in ["new_session_header", "cli_task:claude", "cli_task:codex"] {
        assert!(
            ids.iter().any(|region| region == id),
            "missing {id}: {ids:?}"
        );
    }
    click(&mut app, "cli_task:claude");
    assert!(!app.view.ide.agent_new_open);
    let claude = app.view.selected_session.clone().unwrap();
    click(&mut app, "cli_task:codex");
    let codex = app.view.selected_session.clone().unwrap();
    assert_eq!(app.view.ide.agent_tabs, [claude.clone(), codex.clone()]);
    assert_eq!(app.view.selected_backend, "cli:codex");

    // The terminal is in the agent panel; the bottom panel stays closed.
    assert_eq!(crate::view::terminal_panel_height(&app.view, 800.0), 0.0);
    let mut tree = openide_ui::UiTree::new(crate::view::build_view_sized(&app.view, 1500.0, 800.0));
    let frame = tree.layout(Size::new(1500.0, 800.0));
    let ids: Vec<_> = frame
        .regions
        .iter()
        .map(|region| region.id.clone())
        .collect();
    for id in [
        format!("session:{claude}"),
        format!("ide_agent_tab_close:{codex}"),
    ] {
        assert!(ids.contains(&id), "missing {id}: {ids:?}");
    }
    assert!(
        !ids.iter().any(|id| id == "prompt"),
        "a CLI tab has no composer"
    );

    // Switching tabs keeps both CLIs alive.
    let codex_terminal = app.dock.terminal_tabs(&app.dock_scope())[0].id.get();
    click(&mut app, &format!("session:{claude}"));
    assert_eq!(app.view.selected_session.as_deref(), Some(claude.as_str()));
    let claude_terminal = app.dock.terminal_tabs(&app.dock_scope())[0].id.get();
    assert_ne!(claude_terminal, codex_terminal);
    assert!(
        app.terminals.contains_key(&codex_terminal),
        "the other CLI keeps running"
    );

    // The list opens beside the content and offers every task.
    click(&mut app, "ide_agent_list_toggle");
    assert!(app.view.ide.agent_list_open);
    let mut tree = openide_ui::UiTree::new(crate::view::build_view_sized(&app.view, 1500.0, 800.0));
    let ids: Vec<_> = tree
        .layout(Size::new(1500.0, 800.0))
        .regions
        .iter()
        .map(|region| region.id.clone())
        .collect();
    for id in [
        format!("ide_agent_list:{claude}"),
        format!("ide_agent_list:{codex}"),
    ] {
        assert!(ids.contains(&id), "missing {id}");
    }

    // Closing a background tab leaves the selection; closing the last one starts a new task.
    click(&mut app, &format!("ide_agent_tab_close:{codex}"));
    assert_eq!(app.view.ide.agent_tabs, [claude.clone()]);
    assert_eq!(app.view.selected_session.as_deref(), Some(claude.as_str()));
    click(&mut app, &format!("ide_agent_tab_close:{claude}"));
    assert!(app.view.ide.agent_tabs.is_empty());
    assert!(app.view.selected_session.is_none());
    assert_eq!(app.view.sessions.len(), 2, "closed tabs are still tasks");
}

#[cfg(unix)]
#[test]
fn a_cli_task_takes_the_name_the_cli_gives_its_conversation() {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        cli_conversation_title("✳ Arreglar el login", "claude").as_deref(),
        Some("Arreglar el login")
    );
    assert_eq!(
        cli_conversation_title("⠋ Arreglar el login", "claude").as_deref(),
        Some("Arreglar el login")
    );
    for generic in [
        "",
        "✳",
        "claude",
        "✳ Claude Code",
        "~/projects/app",
        "user@host: ~",
        "ok",
    ] {
        assert_eq!(
            cli_conversation_title(generic, "claude"),
            None,
            "{generic:?}"
        );
    }

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let binary = bin.join("claude");
    std::fs::write(
        &binary,
        "#!/bin/sh\nprintf '\\033]0;Claude Code\\007'\nsleep 0.3\nprintf '\\033]0;\\342\\234\\263 Arreglar el login\\007'\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("state"));
    app.view.workspace = project.to_string_lossy().into_owned();
    *app.login_path.lock().unwrap() = Some(bin.into_os_string());
    app.follow_agent_clis();
    let ctx = egui::Context::default();
    app.apply_action(
        UiAction::Clicked {
            id: "cli_task:claude".into(),
        },
        &ctx,
    );
    let session = app.view.selected_session.clone().unwrap();
    assert_eq!(
        app.view.selected_session_title.as_deref(),
        Some("Claude Code")
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while app.view.selected_session_title.as_deref() != Some("Arreglar el login") {
        let ids: Vec<u64> = app.terminals.keys().copied().collect();
        for id in ids {
            if let Some(terminal) = app.terminals.get_mut(&id) {
                terminal.poll();
            }
        }
        app.follow_cli_task_title();
        assert!(
            Instant::now() < deadline,
            "title never arrived: {:?}",
            app.view.selected_session_title
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let row = app
        .view
        .sessions
        .iter()
        .find(|row| row.id == session)
        .unwrap();
    assert_eq!(row.title, "Arreglar el login");
    assert!(
        !app.follow_cli_task_title(),
        "an unchanged title is not saved again"
    );
    let stored = app.store.as_ref().unwrap().load(&session).unwrap().unwrap();
    assert_eq!(stored.session.title.as_deref(), Some("Arreglar el login"));
}

#[test]
fn the_ide_keeps_the_folders_it_worked_in_and_switches_between_them_from_its_explorer() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.display().to_string(), b.display().to_string());
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = a.clone();
    app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
    app.refresh_composer_projects();
    assert_eq!(app.view.ide_folders.len(), 1);
    let ctx = egui::Context::default();
    let paths = |app: &AgentsWindow| app.view.ide_folders.iter().map(|(path, _)| path.clone()).collect::<Vec<_>>();

    // Working in another folder keeps the first among the IDE's folders, and
    // the explorer offers it as a root to switch to.
    app.change_workspace(b.clone());
    assert_eq!(paths(&app), [b.clone(), a.clone()]);
    let mut tree = openide_ui::UiTree::new(crate::view::build_view_sized(&app.view, 1400.0, 800.0));
    let ids: Vec<_> = tree.layout(Size::new(1400.0, 800.0)).regions.iter().map(|region| region.id.clone()).collect();
    assert!(ids.iter().any(|id| id == "ide_folder_toggle") && ids.iter().any(|id| id == "ide_folder_select:1"), "{ids:?}");

    // Switching back from the explorer; the list survives a restart.
    app.apply_action(UiAction::Clicked { id: "ide_folder_select:1".into() }, &ctx);
    assert_eq!(app.view.workspace, a);
    assert_eq!(paths(&app), [a.clone(), b.clone()]);
    assert_eq!(crate::ide_workspace::IdeWorkspaceState::load(&root.path().join("data")).folders, [a.clone(), b.clone()]);
    app.apply_action(UiAction::Clicked { id: "ide_folder_remove".into() }, &ctx);
    assert_eq!(crate::ide_workspace::IdeWorkspaceState::load(&root.path().join("data")).folders, [b.clone()]);
    assert_eq!(paths(&app), [a.clone(), b.clone()], "the folder stays open until another is chosen");
}
