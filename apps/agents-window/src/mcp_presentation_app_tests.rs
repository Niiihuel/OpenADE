use super::*;
use crate::mcp_inspection::tests::{wait_until, HttpFixture};
use crate::mcp_inspection::McpInspectionPhase;
use crate::mcp_settings::{McpServerSettings, McpSettingsScope};
use crate::view::MainPage;
use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

fn prepare(root: &Path, workspace: &Path) -> AgentsWindow {
    let mut app = AgentsWindow::with_data_root(root.into());
    app.change_workspace(workspace.to_string_lossy().into_owned());
    app.view.show_settings = true;
    app.view.settings_page = SettingsPage::Mcp;
    app.refresh_mcp_settings();
    app
}

fn action(app: &mut AgentsWindow, id: impl Into<String>) {
    app.apply_action(
        UiAction::Clicked { id: id.into() },
        &egui::Context::default(),
    );
}

fn click(app: &mut AgentsWindow, id: &str) {
    let viewport = Size::new(1000.0, 1200.0);
    let mut tree = UiTree::new(build_view(&app.view, viewport.width));
    tree.set_reduced_motion(true);
    let frame = tree.layout(viewport);
    assert!(
        tree.focus(id),
        "MCP control {id} must be keyboard reachable"
    );
    let target = frame
        .regions
        .iter()
        .find(|region| region.id == id)
        .unwrap()
        .rect;
    let point = Point::new(
        target.x + target.width / 2.0,
        target.y + target.height / 2.0,
    );
    tree.handle_event(UiEvent::PointerDown(point));
    let actions = tree.handle_event(UiEvent::PointerUp(point));
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, UiAction::Clicked { id: actual } if actual == id)),
        "{id}: {actions:?}"
    );
    for event in actions {
        app.apply_action(event, &egui::Context::default());
    }
}

fn native_frame(app: &mut AgentsWindow, context: &egui::Context, width: f32) -> UiFrame {
    context.begin_pass(Default::default());
    limn_egui::attach_fonts(&mut app.tree, context);
    let frame = app.layout_workspace_frame(Size::new(width, 1100.0));
    context.end_pass().textures_delta.clear();
    frame
}

fn native_click(app: &mut AgentsWindow, context: &egui::Context, id: &str, width: f32) {
    native_frame(app, context, width);
    assert!(
        app.tree.focus(id),
        "{id} must be keyboard reachable at {width}"
    );
    let frame = native_frame(app, context, width);
    let rect = frame
        .regions
        .iter()
        .find(|region| region.id == id && region.kind == openide_ui::HitKind::Button)
        .unwrap_or_else(|| panic!("missing button {id} at {width}"))
        .rect;
    assert!(
        rect.x >= -0.5 && rect.x + rect.width <= width + 0.5,
        "{id} is horizontally clipped at {width}"
    );
    let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_eq!(
        frame.hit_test(point),
        Some(id),
        "{id} must receive its click"
    );
    app.tree.handle_event(UiEvent::PointerDown(point));
    let actions = app.tree.handle_event(UiEvent::PointerUp(point));
    assert!(actions
        .iter()
        .any(|action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)));
    for action in actions {
        app.apply_action(action, context);
    }
}

fn native_type(app: &mut AgentsWindow, context: &egui::Context, id: &str, text: &str, width: f32) {
    native_frame(app, context, width);
    assert!(app.tree.focus(id));
    native_frame(app, context, width);
    for event in [
        UiEvent::Key {
            key: openide_ui::Key::A,
            shift: false,
            ctrl: true,
        },
        UiEvent::Key {
            key: openide_ui::Key::Backspace,
            shift: false,
            ctrl: false,
        },
        UiEvent::Text(text.into()),
    ] {
        for action in app.tree.handle_event(event) {
            app.apply_action(action, context);
        }
    }
}

fn local_server(id: &str) -> McpServerSettings {
    McpServerSettings {
        id: id.into(),
        enabled: false,
        program: "fixture-must-not-run".into(),
        args: vec![],
        cwd: None,
        credential_env_names: vec![],
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: vec![],
    }
}

fn save_servers(
    root: &Path,
    workspace: &Path,
    scope: McpSettingsScope,
    servers: Vec<McpServerSettings>,
) {
    McpSettings { servers }
        .save_scope(root, workspace, scope)
        .unwrap();
}

fn inspect(app: &mut AgentsWindow, id: &str) {
    action(app, format!("mcp_inspect:{id}"));
    wait_until(|| {
        app.poll_mcp_inspection();
        app.active_mcp_inspection.is_none()
    });
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Completed);
}

fn selection_file(root: &Path, workspace: &Path, scope: McpSettingsScope) -> PathBuf {
    scope
        .storage_root(root, workspace)
        .unwrap()
        .join("mcp-tool-selections.json")
}

#[test]
fn stale_save_without_dirty_changes_cannot_replace_an_explicit_selection_with_all() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = local_server("local");
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![server.clone()],
    );
    let mut selected = McpToolSelections::default();
    selected
        .set_for(&server, Some(vec!["reviewed_tool".into()]))
        .unwrap();
    selected
        .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
        .unwrap();
    let path = selection_file(root.path(), workspace.path(), McpSettingsScope::User);
    let original = std::fs::read(&path).unwrap();
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_details:local");
    assert!(!app.view.mcp_presentation.selection_dirty);
    assert_eq!(
        app.view
            .mcp_presentation
            .selection_draft
            .as_ref()
            .unwrap()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["reviewed_tool"]
    );
    // A stale click must have no authority, even if the transient draft was reset.
    app.view.mcp_presentation.selection_draft = None;
    action(&mut app, "mcp_tools_save");
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        McpToolSelections::load_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap(),
        selected
    );
}

#[test]
fn returning_to_the_list_invalidates_tool_and_brand_actions_from_the_old_details() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = local_server("local");
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![server.clone()],
    );
    let mut selected = McpToolSelections::default();
    selected
        .set_for(&server, Some(vec!["reviewed_tool".into()]))
        .unwrap();
    selected
        .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
        .unwrap();
    let path = selection_file(root.path(), workspace.path(), McpSettingsScope::User);
    let original = std::fs::read(&path).unwrap();
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_details:local");
    action(&mut app, "mcp_tools_all");
    action(&mut app, "mcp_brand_picker");
    assert!(app.view.mcp_presentation.selection_dirty);
    assert!(app.view.mcp_presentation.icon_picker);
    action(&mut app, "mcp_details_back");
    assert!(app.view.mcp_presentation.details.is_none());
    assert!(!app.view.mcp_presentation.selection_dirty);
    assert!(!app.view.mcp_presentation.icon_picker);
    for stale in [
        "mcp_tools_all",
        "mcp_tools_none",
        "mcp_tools_save",
        "mcp_brand_picker",
        "mcp_brand:none",
        "mcp_technical",
    ] {
        action(&mut app, stale);
    }
    assert!(!app.view.mcp_presentation.selection_dirty);
    assert!(!app.view.mcp_presentation.icon_picker);
    assert!(!app.view.mcp_presentation.technical);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!root.path().join("mcp-presentation.json").exists());
}

#[test]
fn explicit_tool_selection_requires_this_registrations_inspected_catalog_and_never_calls_a_tool() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let endpoint = HttpFixture::new(false, false);
    let server = endpoint.server.clone();
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![server.clone()],
    );
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, format!("mcp_details:{}", server.id));
    action(&mut app, "mcp_tool_check:inspect_echo");
    assert!(
        !app.view.mcp_presentation.selection_dirty,
        "an uninspected row cannot select a name"
    );
    action(&mut app, "mcp_tools_none");
    action(&mut app, "mcp_tools_save");
    let path = selection_file(root.path(), workspace.path(), McpSettingsScope::User);
    assert!(!path.exists());
    action(&mut app, "mcp_tools_revert");
    inspect(&mut app, &server.id);
    assert_eq!(
        app.view
            .mcp_inspection
            .snapshot
            .as_ref()
            .unwrap()
            .tools
            .len(),
        2
    );
    action(&mut app, "mcp_tools_none");
    action(&mut app, "mcp_tool_check:inspect_echo");
    let reviewed = app.view.mcp_presentation.selection_draft.clone();
    action(&mut app, "mcp_tool_check:unadvertised_tool");
    assert_eq!(app.view.mcp_presentation.selection_draft, reviewed);
    action(&mut app, "mcp_tools_save");
    assert!(!app.view.mcp_presentation.selection_dirty);
    let selected =
        McpToolSelections::load_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
    assert_eq!(
        selected.selected_for(&server).unwrap().unwrap(),
        ["inspect_echo"]
    );
    let original = std::fs::read(&path).unwrap();
    app.view
        .mcp_presentation
        .selection_draft
        .as_mut()
        .unwrap()
        .insert("not_in_the_catalog".into());
    app.view.mcp_presentation.selection_dirty = true;
    action(&mut app, "mcp_tools_save");
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!endpoint
        .methods()
        .iter()
        .any(|method| method == "tools/call"));
}

#[test]
fn changing_registration_or_selection_on_disk_rejects_cached_save_without_overwriting_it() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = local_server("local");
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![server.clone()],
    );
    let mut selected = McpToolSelections::default();
    selected
        .set_for(&server, Some(vec!["reviewed_tool".into()]))
        .unwrap();
    selected
        .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
        .unwrap();
    let path = selection_file(root.path(), workspace.path(), McpSettingsScope::User);
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_details:local");
    action(&mut app, "mcp_tools_all");
    let mut changed = server.clone();
    changed.args.push("--changed-registration".into());
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![changed],
    );
    let original = std::fs::read(&path).unwrap();
    action(&mut app, "mcp_tools_save");
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![server.clone()],
    );
    selected
        .set_for(&server, Some(vec!["external_selection".into()]))
        .unwrap();
    selected
        .save_scope(root.path(), workspace.path(), McpSettingsScope::User)
        .unwrap();
    let external = std::fs::read(&path).unwrap();
    action(&mut app, "mcp_tools_save");
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(&path).unwrap(), external);
}

#[test]
fn brand_choice_persists_only_in_its_scope_and_does_not_edit_the_registration() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = local_server("local");
    for scope in [McpSettingsScope::User, McpSettingsScope::Project] {
        save_servers(root.path(), workspace.path(), scope, vec![server.clone()]);
    }
    let registration = std::fs::read(root.path().join("mcp-servers.json")).unwrap();
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_details:local");
    action(&mut app, "mcp_brand_picker");
    let brand = crate::brand_icons::catalog()[0].id;
    action(&mut app, format!("mcp_brand:{brand}"));
    let user =
        McpPresentationPreferences::load(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap();
    assert_eq!(
        user.icons.get("local"),
        Some(&BrandChoice::Brand(brand.into()))
    );
    assert!(McpPresentationPreferences::load(
        root.path(),
        workspace.path(),
        McpSettingsScope::Project
    )
    .unwrap()
    .icons
    .is_empty());
    assert_eq!(
        std::fs::read(root.path().join("mcp-servers.json")).unwrap(),
        registration
    );
    action(&mut app, "settings_choice_toggle:mcp_scope");
    action(&mut app, "mcp_scope:project");
    assert_eq!(app.view.mcp_settings_scope, McpSettingsScope::Project);
    action(&mut app, "mcp_details:local");
    action(&mut app, "mcp_brand_picker");
    action(&mut app, "mcp_brand:none");
    assert_eq!(
        McpPresentationPreferences::load(root.path(), workspace.path(), McpSettingsScope::Project)
            .unwrap()
            .icons
            .get("local"),
        Some(&BrandChoice::None)
    );
    assert_eq!(
        McpPresentationPreferences::load(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap(),
        user
    );
    assert_eq!(
        McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User)
            .unwrap()
            .servers,
        [server]
    );
}

#[test]
fn cached_brand_actions_preserve_external_preferences_and_ignore_hidden_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![local_server("local")],
    );
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_details:local");
    action(&mut app, "mcp_brand_picker");
    let mut external = McpPresentationPreferences::default();
    external.icons.insert("local".into(), BrandChoice::None);
    external
        .save(root.path(), workspace.path(), McpSettingsScope::User)
        .unwrap();
    let path = root.path().join("mcp-presentation.json");
    let original = std::fs::read(&path).unwrap();
    action(&mut app, "mcp_brand:auto");
    assert!(app.view.settings_notice_error);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    app.view.settings_page = SettingsPage::Appearance;
    action(&mut app, "mcp_brand:none");
    action(&mut app, "mcp_tools_all");
    action(&mut app, "mcp_tools_save");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(!selection_file(root.path(), workspace.path(), McpSettingsScope::User).exists());
}

#[test]
fn details_pointer_action_works_in_settings_without_starting_the_server() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![local_server("local")],
    );
    let mut app = prepare(root.path(), workspace.path());

    assert!(app.view.shows(SettingsPage::Mcp));
    click(&mut app, "mcp_details:local");
    assert_eq!(app.view.mcp_presentation.details.as_deref(), Some("local"));
    assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Idle);
    assert!(app.active_mcp_inspection.is_none());
    assert!(!app.view.mcp_servers[0].enabled);
}

#[test]
fn mcp_switch_search_details_and_scope_keyboard_work_in_settings() {
    for width in [320.0, 360.0, 900.0, 1400.0] {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        save_servers(
            root.path(),
            workspace.path(),
            McpSettingsScope::User,
            vec![local_server("local-analysis"), local_server("other-server")],
        );
        save_servers(
            root.path(),
            workspace.path(),
            McpSettingsScope::Project,
            vec![local_server("project-only")],
        );
        let mut app = prepare(root.path(), workspace.path());
        app.tree.set_reduced_motion(true);
        app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;

        let context = egui::Context::default();
        crate::fonts::install(&context);
        let frame = native_frame(&mut app, &context, width);
        let switch = frame
            .access
            .iter()
            .find(|node| node.id == "mcp_toggle:local-analysis")
            .unwrap();
        assert_eq!(switch.role, openide_ui::Role::Switch);
        assert_eq!(switch.toggled, Some(false));
        assert!(!switch.name.is_empty());
        let switch_name = switch.name.clone();
        native_click(&mut app, &context, "mcp_toggle:local-analysis", width);
        let saved =
            McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User).unwrap();
        assert!(
            saved
                .servers
                .iter()
                .find(|server| server.id == "local-analysis")
                .unwrap()
                .enabled
        );
        let frame = native_frame(&mut app, &context, width);
        let switch = frame
            .access
            .iter()
            .find(|node| node.id == "mcp_toggle:local-analysis")
            .unwrap();
        assert_eq!(switch.toggled, Some(true));
        assert_eq!(switch.name, switch_name);

        native_type(&mut app, &context, "mcp_search", "local-analysis", width);
        assert_eq!(app.view.mcp_presentation.search, "local-analysis");
        let frame = native_frame(&mut app, &context, width);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "mcp_details:local-analysis"));
        assert!(!frame
            .regions
            .iter()
            .any(|region| region.id == "mcp_details:other-server"));
        native_click(&mut app, &context, "mcp_details:local-analysis", width);
        assert_eq!(
            app.view.mcp_presentation.details.as_deref(),
            Some("local-analysis")
        );
        assert!(!app.view.mcp_presentation.technical);
        native_click(&mut app, &context, "mcp_brand_picker", width);
        assert!(app.view.mcp_presentation.icon_picker);
        native_click(&mut app, &context, "mcp_brand_picker", width);
        assert!(!app.view.mcp_presentation.icon_picker);
        let frame = native_frame(&mut app, &context, width);
        assert!(!frame.access.iter().any(|node| node.id == "mcp_brand_query"));

        // The inspected catalog is a local fixture. Opening or filtering
        // these views must not launch a server or call an MCP tool.
        let mut snapshot = crate::mcp_inspection::tests::sample_snapshot();
        let template = snapshot.tools[0].clone();
        snapshot.tools = (0..170)
            .map(|index| {
                let mut tool = template.clone();
                tool.name = format!("analyze_artifact_{index:03}");
                tool
            })
            .collect();
        snapshot.details_json = serde_json::to_string_pretty(&snapshot.tools).unwrap();
        let full_json = snapshot.details_json.clone();
        let last_tool = serde_json::to_string_pretty(&snapshot.tools[169]).unwrap();
        app.view.mcp_inspection.scope = Some(crate::mcp_inspection::McpInspectionScope {
            request_id: 1,
            workspace: workspace.path().canonicalize().unwrap(),
            server: app
                .view
                .mcp_servers
                .iter()
                .find(|server| server.id == "local-analysis")
                .unwrap()
                .clone(),
        });
        app.view.mcp_inspection.phase = McpInspectionPhase::Completed;
        app.view.mcp_inspection.snapshot = Some(snapshot);
        native_type(&mut app, &context, "mcp_tool_query", "169", width);
        assert_eq!(app.view.mcp_presentation.tool_query, "169");
        assert_eq!(
            app.view
                .mcp_inspection
                .snapshot
                .as_ref()
                .unwrap()
                .tools
                .len(),
            170
        );
        let frame = native_frame(&mut app, &context, width);
        assert!(frame
            .access
            .iter()
            .any(|node| node.id == "mcp_tool_check:analyze_artifact_169"));
        assert!(!frame
            .access
            .iter()
            .any(|node| node.id == "mcp_tool_check:analyze_artifact_000"));
        native_click(
            &mut app,
            &context,
            "mcp_tool_expand:analyze_artifact_169",
            width,
        );
        assert_eq!(
            app.view.mcp_presentation.expanded_tool.as_deref(),
            Some("analyze_artifact_169")
        );
        let frame = native_frame(&mut app, &context, width);
        assert!(frame
            .text_layouts()
            .iter()
            .any(|text| text.source == last_tool));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|text| text.source == full_json));
        native_click(&mut app, &context, "mcp_technical", width);
        assert!(app.view.mcp_presentation.technical);
        native_click(&mut app, &context, "mcp_technical", width);
        assert!(!app.view.mcp_presentation.technical);
        native_click(&mut app, &context, "mcp_details_back", width);
        assert!(app.view.mcp_presentation.details.is_none());
        native_type(&mut app, &context, "mcp_search", "", width);

        assert!(
            app.view.settings_choice_menu_open.is_none(),
            "scope starts closed: Settings, width={width}"
        );
        native_click(
            &mut app,
            &context,
            "settings_choice_toggle:mcp_scope",
            width,
        );
        assert_eq!(
            app.view.settings_choice_menu_open.as_deref(),
            Some("mcp_scope"),
            "scope must open: Settings, width={width}"
        );
        native_frame(&mut app, &context, width);
        let actions = app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Escape,
            shift: false,
            ctrl: false,
        });
        assert!(actions.iter().any(
            |action| matches!(action, UiAction::Clicked { id } if id == "settings_choice_dismiss")
        ));
        for action in actions {
            app.apply_action(action, &context);
        }
        native_frame(&mut app, &context, width);
        assert_eq!(app.view.mcp_settings_scope, McpSettingsScope::User);
        assert_eq!(
            app.tree.focused_id(),
            Some("settings_choice_toggle:mcp_scope")
        );
        native_click(
            &mut app,
            &context,
            "settings_choice_toggle:mcp_scope",
            width,
        );
        native_click(&mut app, &context, "mcp_scope:project", width);
        assert_eq!(app.view.mcp_settings_scope, McpSettingsScope::Project);
        assert_eq!(app.view.mcp_servers, [local_server("project-only")]);
        native_frame(&mut app, &context, width);
        assert_eq!(
            app.tree.focused_id(),
            Some("settings_choice_toggle:mcp_scope")
        );
        assert_eq!(
            McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User).unwrap(),
            saved
        );
        assert_eq!(app.view.mcp_inspection.phase, McpInspectionPhase::Idle);
        assert!(app.active_mcp_inspection.is_none());
        assert!(app.active_mcp_oauth.is_none());
        assert!(app.active_run.is_none());
    }
}

#[test]
fn marketplace_hides_admin_controls_and_legacy_routes_open_canonical_settings() {
    for mode in [
        crate::ide_workspace::WindowMode::Agents,
        crate::ide_workspace::WindowMode::Ide,
    ] {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        save_servers(
            root.path(),
            workspace.path(),
            McpSettingsScope::User,
            vec![local_server("local")],
        );
        let mut app = prepare(root.path(), workspace.path());
        app.view.ide.mode = mode;
        app.view.ide.activity = crate::ide_workspace::IdeActivity::Extensions;
        app.view.ide.extensions_open = true;
        action(&mut app, "open_marketplace");
        assert!(app.view.marketplace_visible());
        assert!(!app.view.shows(SettingsPage::Mcp));
        assert!(!app.view.shows(SettingsPage::Plugins));
        let frame = UiTree::new(build_view(&app.view, 1000.0)).layout(Size::new(1000.0, 1100.0));
        assert!(!frame
            .access
            .iter()
            .any(|node| node.id.starts_with("mcp_") || node.id.starts_with("plugins_")));
        let registration = std::fs::read(root.path().join("mcp-servers.json")).unwrap();
        app.view.mcp_draft_id = "preserved-draft".into();
        app.view.mcp_draft_program = "preserved-program".into();
        for id in [
            "mcp_toggle:local",
            "mcp_new",
            "mcp_select:local",
            "mcp_delete:local",
            "mcp_delete_confirm:local",
            "mcp_save",
        ] {
            action(&mut app, id);
        }
        app.apply_action(
            UiAction::InputChanged {
                id: "mcp_draft_id".into(),
                value: "stale-input".into(),
            },
            &egui::Context::default(),
        );
        assert_eq!(app.view.mcp_draft_id, "preserved-draft");
        assert_eq!(app.view.mcp_draft_program, "preserved-program");
        assert!(!app.view.mcp_editor_open);
        assert!(app.view.mcp_selected_id.is_none());
        assert!(app.view.pending_mcp_delete_id.is_none());
        assert_eq!(
            std::fs::read(root.path().join("mcp-servers.json")).unwrap(),
            registration
        );
        action(&mut app, "settings_choice_toggle:mcp_scope");
        action(&mut app, "mcp_details:local");
        action(&mut app, "mcp_tools_save");
        action(&mut app, "plugins_install_open");
        assert!(app.view.settings_choice_menu_open.is_none());
        assert!(app.view.mcp_presentation.details.is_none());
        assert!(!app.view.plugins.install_open);
        assert!(app.plugin_response.is_none());
        assert!(!selection_file(root.path(), workspace.path(), McpSettingsScope::User).exists());

        for (alias, page) in [
            ("marketplace_tab:mcp", SettingsPage::Mcp),
            ("marketplace_tab:plugins", SettingsPage::Plugins),
        ] {
            action(&mut app, "open_marketplace");
            action(&mut app, alias);
            assert!(app.view.show_settings && app.view.shows(page));
            assert_eq!(app.view.settings_page, page);
            assert!(!app.view.marketplace.sources_open);
        }
        assert_eq!(
            McpSettings::load_scope(root.path(), workspace.path(), McpSettingsScope::User)
                .unwrap()
                .servers,
            [local_server("local")]
        );
        assert!(app.active_mcp_inspection.is_none());
        assert!(app.active_mcp_oauth.is_none());
        assert!(app.active_run.is_none());
    }
}

#[test]
fn leaving_mcp_settings_closes_delete_confirmation_and_rejects_its_stale_confirm() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![local_server("local")],
    );
    let registration_path = root.path().join("mcp-servers.json");
    let registration = std::fs::read(&registration_path).unwrap();
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "mcp_delete:local");
    assert_eq!(app.view.pending_mcp_delete_id.as_deref(), Some("local"));
    let confirmation_visible = |app: &AgentsWindow| {
        UiTree::new(build_view(&app.view, 1000.0))
            .layout(Size::new(1000.0, 1100.0))
            .access
            .iter()
            .any(|node| node.id.starts_with("mcp_delete_confirm:"))
    };
    assert!(confirmation_visible(&app));

    // Rendering also defends against an old saved UI state before action cleanup.
    app.view.show_settings = false;
    assert!(!confirmation_visible(&app));
    app.view.show_settings = true;
    assert!(confirmation_visible(&app));

    action(&mut app, "open_marketplace");
    assert!(app.view.marketplace_visible());
    assert!(app.view.pending_mcp_delete_id.is_none());
    assert!(!app.view.mcp_editor_open);
    assert!(!confirmation_visible(&app));
    action(&mut app, "mcp_delete_confirm:local");
    assert_eq!(std::fs::read(&registration_path).unwrap(), registration);

    action(&mut app, "marketplace_tab:mcp");
    assert!(app.view.shows(SettingsPage::Mcp));
    assert!(app.view.pending_mcp_delete_id.is_none());
    assert!(!confirmation_visible(&app));
    action(&mut app, "mcp_delete_confirm:local");
    assert_eq!(std::fs::read(&registration_path).unwrap(), registration);
    assert_eq!(app.view.mcp_servers, [local_server("local")]);
    assert!(app.active_mcp_inspection.is_none());
    assert!(app.active_run.is_none());
}

#[test]
fn marketplace_sources_modal_and_legacy_aliases_preserve_catalog_context_on_escape() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut app = prepare(root.path(), workspace.path());
    action(&mut app, "open_marketplace");
    wait_until(|| {
        app.poll_marketplace();
        app.marketplace_response.is_none()
    });
    app.view.marketplace.snapshot.items = vec![crate::marketplace::MarketplaceItem {
        id: "fixture:review".into(),
        name: "review".into(),
        description: "Fixture only; never installed or run".into(),
        license: Some("MIT".into()),
        source_name: "fixture".into(),
        source_kind: crate::marketplace::SourceKind::Local,
        installed_scope: Some(crate::marketplace::InstallScope::User),
        enabled: true,
        source_available: true,
    }];
    app.set_marketplace_query("review".into());
    app.set_marketplace_filter(crate::marketplace_view::MarketplaceFilter::Installed);
    app.view.marketplace.selected_item_id = Some("fixture:review".into());
    app.tree.set_reduced_motion(true);
    let context = egui::Context::default();
    crate::fonts::install(&context);
    for open in ["marketplace_sources_open", "marketplace_tab:sources"] {
        action(&mut app, open);
        assert!(app.view.marketplace.sources_open);
        let frame = native_frame(&mut app, &context, 900.0);
        assert!(frame
            .access
            .iter()
            .any(|node| node.id == "marketplace_sources_close:dialog"
                && node.role == openide_ui::Role::Dialog));
        assert_eq!(app.tree.focused_id(), Some("marketplace_add_menu"));
        let actions = app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::Escape,
            shift: false,
            ctrl: false,
        });
        assert!(actions.iter().any(
            |action| matches!(action, UiAction::Clicked { id } if id == "marketplace_sources_close")
        ));
        for action in actions {
            app.apply_action(action, &context);
        }
        assert!(!app.view.marketplace.sources_open);
        assert_eq!(app.view.marketplace.query, "review");
        assert_eq!(
            app.view.marketplace.filter,
            crate::marketplace_view::MarketplaceFilter::Installed
        );
        assert_eq!(
            app.view.marketplace.selected_item_id.as_deref(),
            Some("fixture:review")
        );
        native_frame(&mut app, &context, 900.0);
    }
    action(&mut app, "marketplace_tab:sources");
    action(&mut app, "marketplace_tab:skills");
    assert!(!app.view.marketplace.sources_open);
    assert_eq!(app.view.marketplace.query, "review");
    assert_eq!(
        app.view.marketplace.filter,
        crate::marketplace_view::MarketplaceFilter::Installed
    );
    assert_eq!(
        app.view.marketplace.selected_item_id.as_deref(),
        Some("fixture:review")
    );
    assert!(app.marketplace_response.is_none());
    assert!(app.active_run.is_none());
}

#[test]
fn discovery_discards_external_registration_changes_and_refreshes_for_scope_and_workspace() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![local_server("user-local")],
    );
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::Project,
        vec![local_server("project-local")],
    );
    save_servers(
        root.path(),
        other.path(),
        McpSettingsScope::Project,
        vec![local_server("other-local")],
    );
    let mut app = prepare(root.path(), workspace.path());
    assert!(app.active_mcp_discovery.is_some());
    save_servers(
        root.path(),
        workspace.path(),
        McpSettingsScope::User,
        vec![local_server("externally-replaced")],
    );
    wait_until(|| {
        app.poll_mcp_discovery();
        app.active_mcp_discovery.is_none()
    });
    assert!(
        app.view.mcp_presentation.discovery.is_empty(),
        "worker results cannot publish over newer saved registrations"
    );
    action(&mut app, "settings_choice_toggle:mcp_scope");
    action(&mut app, "mcp_scope:project");
    wait_until(|| {
        app.poll_mcp_discovery();
        app.active_mcp_discovery.is_none()
    });
    assert_eq!(
        app.view
            .mcp_presentation
            .discovery
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["project-local"]
    );
    app.change_workspace(other.path().to_string_lossy().into_owned());
    wait_until(|| {
        app.poll_mcp_discovery();
        app.active_mcp_discovery.is_none()
    });
    assert_eq!(
        app.view
            .mcp_presentation
            .discovery
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["other-local"]
    );
    assert!(!app
        .view
        .mcp_presentation
        .discovery
        .contains_key("project-local"));
}
