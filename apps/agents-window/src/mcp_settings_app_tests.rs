use super::*;
use crate::mcp_import::McpImportController;
use crate::mcp_inspection::tests::{wait_until, HttpFixture};
use crate::mcp_inspection::McpInspectionPhase;
use crate::mcp_settings::McpSettingsScope;
use openide_ui::{Point, Size, UiEvent};

fn prepare(root: &Path, workspace: &Path) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.into());
    app.change_workspace(workspace.to_string_lossy().into_owned());
    app.view.show_settings = true;
    app.view.settings_page = SettingsPage::Mcp;
    app
}

fn click(app: &mut AgentsWindow, id: &str, width: f32) {
    if let Some(server) = id.strip_prefix("mcp_inspect:") {
        if app.view.mcp_presentation.details.as_deref() != Some(server) {
            click(app, &format!("mcp_details:{server}"), width);
        }
    }
    let viewport = Size::new(width, 760.0);
    let mut tree = UiTree::new(build_view(&app.view, width));
    tree.layout(viewport);
    assert!(
        tree.focus(id),
        "MCP action {id} must be keyboard reachable at {width}"
    );
    let frame = tree.layout(viewport);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap()
        .rect;
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::Clicked { id: action_id } if action_id == id)),
        "{id}: {actions:?}"
    );
    for action in actions {
        app.apply_action(action, &egui::Context::default());
    }
}

fn scope(app: &mut AgentsWindow, value: McpSettingsScope, width: f32) {
    click(app, "settings_choice_toggle:mcp_scope", width);
    click(app, &format!("mcp_scope:{}", value.key()), width);
    assert_eq!(app.view.mcp_settings_scope, value);
}

fn save_process(app: &mut AgentsWindow, program: &str, width: f32) {
    click(app, "mcp_new", width);
    for (id, value) in [("mcp_draft_id", "shared"), ("mcp_draft_program", program)] {
        app.apply_action(
            UiAction::InputChanged {
                id: id.into(),
                value: value.into(),
            },
            &egui::Context::default(),
        );
    }
    click(app, "mcp_save", width);
    assert!(!app.view.mcp_editor_open);
}

#[test]
fn mcp_scope_pointer_save_reload_and_disabled_project_override_feed_the_effective_catalog() {
    for width in [420.0, 900.0] {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut app = prepare(root.path(), workspace.path());
        save_process(&mut app, "user-program-must-never-launch", width);
        click(&mut app, "mcp_toggle:shared", width);
        let user =
            McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User).unwrap();
        assert!(user.servers[0].enabled);
        scope(&mut app, McpSettingsScope::Project, width);
        assert!(app.view.mcp_servers.is_empty());
        save_process(&mut app, "project-program-must-never-launch", width);
        let effective = McpSettings::load(root.path(), workspace.path()).unwrap();
        assert_eq!(effective.servers.len(), 1);
        assert_eq!(
            effective.servers[0].program,
            "project-program-must-never-launch"
        );
        assert!(!effective.servers[0].enabled);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let discovered = crate::mcp_runtime::discover_mcp_runtime(
                &effective.servers,
                workspace.path(),
                std::sync::Arc::new(openide_harness::LocalTools::new()),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
            assert!(discovered.mcp_tool_names().is_empty());
            discovered.shutdown().await.unwrap();
        });
        assert_eq!(
            McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User).unwrap(),
            user
        );
        assert_eq!(McpSettings::load(root.path(), other.path()).unwrap(), user);
        drop(app);
        let reopened = prepare(root.path(), workspace.path());
        assert_eq!(reopened.view.mcp_settings_scope, McpSettingsScope::User);
        assert_eq!(reopened.view.mcp_servers, user.servers);
        assert_eq!(reopened.view.mcp_shadowed_user_ids, vec!["shared"]);
        assert!(reopened.view.mcp_effective_summary.starts_with("0 servidores habilitados"));
        assert_eq!(
            McpSettings::load(root.path(), workspace.path()).unwrap(),
            effective
        );
    }
}

#[test]
fn mcp_cached_controls_preserve_external_changes_and_recover_by_explicit_reload() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare(root.path(), workspace.path());
    save_process(&mut app, "original-server", 900.0);
    let mut repaired = McpSettings::load(root.path(), workspace.path()).unwrap();
    let path = root.path().join("mcp-servers.json");
    let broken = b"{externally broken configuration";
    std::fs::write(&path, broken).unwrap();
    click(&mut app, "mcp_toggle:shared", 900.0);
    assert_eq!(std::fs::read(&path).unwrap(), broken);
    assert!(app.view.mcp_load_error.is_some());
    repaired.servers[0].program = "repaired-server".into();
    repaired.save(root.path(), workspace.path()).unwrap();
    click(&mut app, "mcp_refresh", 900.0);
    assert!(app.view.mcp_load_error.is_none());
    assert_eq!(app.view.mcp_servers, repaired.servers);
    repaired.servers[0].program = "external-newer-server".into();
    repaired.save(root.path(), workspace.path()).unwrap();
    assert!(!app.delete_mcp_server("shared"));
    assert_eq!(McpSettings::load(root.path(), workspace.path()).unwrap(), repaired);
    click(&mut app, "mcp_refresh", 900.0);
    assert!(app.delete_mcp_server("shared"));
    assert!(McpSettings::load(root.path(), workspace.path()).unwrap().servers.is_empty());
}

#[test]
fn mcp_inspection_uses_the_edited_scope_and_next_turn_discovers_only_effective_project_server() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let user = HttpFixture::new(false, false);
    let project = HttpFixture::new(false, false);
    McpSettings {
        servers: vec![user.server.clone()],
    }
    .save(root.path(), workspace.path())
    .unwrap();
    McpSettings {
        servers: vec![project.server.clone()],
    }
    .save_scope(root.path(), workspace.path(), McpSettingsScope::Project)
    .unwrap();
    let mut app = prepare(root.path(), workspace.path());
    click(&mut app, "mcp_inspect:inspection-local", 900.0);
    wait_until(|| {
        app.poll_mcp_inspection();
        app.active_mcp_inspection.is_none()
    });
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Completed);
    assert!(!user.methods().is_empty());
    assert!(project.methods().is_empty());
    let user_methods = user.methods();
    scope(&mut app, McpSettingsScope::Project, 900.0);
    assert!(app.view.mcp_inspection.last.is_empty());
    click(&mut app, "mcp_inspect:inspection-local", 900.0);
    wait_until(|| {
        app.poll_mcp_inspection();
        app.active_mcp_inspection.is_none()
    });
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Completed);
    assert_eq!(user.methods(), user_methods);
    let before_runtime = project
        .methods()
        .iter()
        .filter(|method| method.as_str() == "server/discover")
        .count();
    assert_eq!(before_runtime, 1, "the explicit inspection performs one modern handshake");
    let before_tool_pages = project.methods().iter().filter(|method| method.as_str() == "tools/list").count();
    assert_eq!(before_tool_pages, 2, "the inspection reads both tool pages");
    let effective = McpSettings::load(root.path(), workspace.path()).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let runtime = crate::mcp_runtime::discover_mcp_runtime(
                &effective.servers,
                workspace.path(),
                std::sync::Arc::new(openide_harness::LocalTools::new()),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(runtime.mcp_tool_names().len(), 2);
            runtime.shutdown().await.unwrap();
        });
    assert_eq!(
        project
            .methods()
            .iter()
            .filter(|method| method.as_str() == "server/discover")
            .count(),
        before_runtime + 1
    );
    assert_eq!(project.methods().iter().filter(|method| method.as_str() == "tools/list").count(), before_tool_pages + 2, "runtime independently reads the complete effective catalog");
    assert_eq!(user.methods(), user_methods);
    assert!(!project
        .methods()
        .iter()
        .any(|method| method == "tools/call"));
}

#[test]
fn a_runtime_failure_from_a_project_override_does_not_replace_the_users_inspection_status() {
    use crate::mcp_inspection::McpServerStatus;
    use openide_harness::PreparedToolSession;
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let user = HttpFixture::new(false, false);
    McpSettings { servers: vec![user.server.clone()] }.save(root.path(), workspace.path()).unwrap();
    let mut project = user.server.clone();
    project.url = None;
    project.program = "this-process-must-never-start".into();
    project.cwd = Some("missing-project-server-directory".into());
    McpSettings { servers: vec![project] }.save_scope(root.path(), workspace.path(), McpSettingsScope::Project).unwrap();
    let mut app = prepare(root.path(), workspace.path());
    click(&mut app, "mcp_inspect:inspection-local", 900.0);
    wait_until(|| { app.poll_mcp_inspection(); app.active_mcp_inspection.is_none() });
    assert_eq!(app.view.mcp_inspection.last.get("inspection-local"), Some(&McpServerStatus::Connected { tools: 2 }));
    let effective = McpSettings::load(root.path(), workspace.path()).unwrap();
    let notices = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let runtime = crate::mcp_runtime::discover_mcp_runtime(&effective.servers, workspace.path(), std::sync::Arc::new(openide_harness::LocalTools::new()), &tokio_util::sync::CancellationToken::new()).await.unwrap();
        assert!(runtime.mcp_tool_names().is_empty());
        let notices = runtime.notices();
        runtime.shutdown().await.unwrap();
        notices
    });
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, openide_core::NOTICE_MCP_UNAVAILABLE);
    assert!(notices[0].1.starts_with("inspection-local\n"));
    let session = app.store.as_ref().unwrap().create(workspace.path(), "fixture").unwrap();
    for (index, scope_value) in [McpSettingsScope::User, McpSettingsScope::Project].into_iter().enumerate() {
        if index > 0 { scope(&mut app, scope_value, 900.0); }
        app.process_event(AgentEvent::new(&session.id, index as u64 + 1, AgentEventKind::HostNotice { code: notices[0].0.clone(), detail: notices[0].1.clone() }));
        if scope_value == McpSettingsScope::User {
            assert_eq!(app.view.mcp_inspection.last.get("inspection-local"), Some(&McpServerStatus::Connected { tools: 2 }), "runtime uses a distinct Project registration with the same ID");
        } else {
            assert!(app.view.mcp_inspection.last.is_empty(), "a runtime notice is not an explicit scoped inspection");
        }
    }
    let stored = app.store.as_ref().unwrap().load(&session.id).unwrap().unwrap();
    assert_eq!(stored.events.iter().filter(|event| matches!(&event.kind, AgentEventKind::HostNotice { code, detail } if code == openide_core::NOTICE_MCP_UNAVAILABLE && detail == &notices[0].1)).count(), 2, "runtime failures remain recorded in the journal");
    assert!(app.projection.items().iter().any(|item| matches!(item, TranscriptItem::Notice { message, .. } if message.contains("Cwd MCP no disponible"))));
}

#[test]
fn mcp_import_worker_commits_only_its_saved_scope_and_discards_scope_aba() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".cursor")).unwrap();
    std::fs::create_dir(workspace.path().join(".cursor")).unwrap();
    std::fs::write(
        home.path().join(".cursor/mcp.json"),
        r#"{"mcpServers":{"personal":{"command":"fixture-user"}}}"#,
    )
    .unwrap();
    std::fs::write(
        workspace.path().join(".cursor/mcp.json"),
        r#"{"mcpServers":{"project":{"command":"fixture-project"}}}"#,
    )
    .unwrap();
    let mut app = prepare(root.path(), workspace.path());
    app.active_mcp_import = Some(
        McpImportController::start(
            home.path().into(),
            workspace.path(),
            McpSettingsScope::User,
            vec![],
        )
        .unwrap(),
    );
    wait_until(|| {
        app.poll_mcp_import();
        app.active_mcp_import.is_none()
    });
    assert_eq!(
        app.view
            .mcp_servers
            .iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["personal"]
    );
    assert!(app.view.mcp_servers.iter().all(|server| !server.enabled));
    scope(&mut app, McpSettingsScope::Project, 900.0);
    app.active_mcp_import = Some(
        McpImportController::start(
            home.path().into(),
            workspace.path(),
            McpSettingsScope::Project,
            vec![],
        )
        .unwrap(),
    );
    scope(&mut app, McpSettingsScope::User, 900.0);
    scope(&mut app, McpSettingsScope::Project, 900.0);
    wait_until(|| {
        app.poll_mcp_import();
        app.active_mcp_import.is_none()
    });
    assert!(
        McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::Project)
            .unwrap()
            .servers
            .is_empty()
    );
    app.active_mcp_import = Some(
        McpImportController::start(
            home.path().into(),
            workspace.path(),
            McpSettingsScope::Project,
            vec![],
        )
        .unwrap(),
    );
    wait_until(|| {
        app.poll_mcp_import();
        app.active_mcp_import.is_none()
    });
    assert_eq!(
        app.view
            .mcp_servers
            .iter()
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>(),
        ["project"]
    );
    assert!(app.view.mcp_servers.iter().all(|server| !server.enabled));
    assert_eq!(
        McpSettings::load(root.path(), workspace.path())
            .unwrap()
            .servers
            .len(),
        2
    );
}
