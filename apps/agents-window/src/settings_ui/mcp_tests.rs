use super::*;
use openide_ui::{Role, Size, UiTree};

fn sample(workspace: &std::path::Path) -> ViewState {
    let mut state = crate::view::tests::empty_state();
    state.show_settings = true;
    state.settings_page = SettingsPage::Mcp;
    state.workspace = workspace.to_string_lossy().into_owned();
    state.mcp_servers = vec![McpServerSettings {
        id: "local-analysis".into(),
        enabled: true,
        program: "/opt/fixtures/local-packages/analysis-workbench-with-a-long-installation-path/bin/analysis-mcp".into(),
        args: vec![],
        cwd: None,
        credential_env_names: vec![],
        env: Default::default(),
        url: None,
        http_bearer_token_env_name: None,
        http_oauth: None,
        http_env_headers: vec![],
    }];
    let mut disabled = state.mcp_servers[0].clone();
    disabled.id = "documentation-tools-read-only".into();
    disabled.enabled = false;
    disabled.program = "docs-mcp".into();
    let mut remote = state.mcp_servers[0].clone();
    remote.id = "remote-source-control-long-server-name".into();
    remote.program.clear();
    remote.url = Some(
        "https://mcp.fixture.invalid/a/long/synthetic/workspace-specific/service/path/mcp".into(),
    );
    state.mcp_inspection.last.insert(
        remote.id.clone(),
        crate::mcp_inspection::McpServerStatus::Failed(
            "El endpoint de fixture no respondió durante la última inspección.".into(),
        ),
    );
    state.mcp_servers.extend([disabled, remote]);
    state.mcp_effective_summary = "2 servidores habilitados para el próximo turno".into();
    state
}

fn snapshot(state: &mut ViewState, workspace: &std::path::Path) {
    let mut snapshot = crate::mcp_inspection::tests::sample_snapshot();
    let template = snapshot.tools[0].clone();
    snapshot.tools = (0..170)
        .map(|i| {
            let mut tool = template.clone();
            tool.name = format!("analyze_artifact_{i:03}");
            if i == 169 {
                tool.extra.insert("outputSchema".into(), serde_json::json!({
                    "type":"object", "properties":{"tail":{"const":"FINAL_TOOL_COMPLETE_METADATA"}}
                }));
            }
            tool
        })
        .collect();
    snapshot.details_json = serde_json::to_string_pretty(&snapshot.tools).unwrap();
    state.mcp_presentation.details = Some("local-analysis".into());
    state.mcp_inspection.scope = Some(crate::mcp_inspection::McpInspectionScope {
        request_id: 1,
        workspace: workspace.canonicalize().unwrap(),
        server: state.mcp_servers[0].clone(),
    });
    state.mcp_inspection.phase = crate::mcp_inspection::McpInspectionPhase::Completed;
    state.mcp_inspection.snapshot = Some(snapshot);
}

#[test]
fn mcp_inspector_preserves_the_entire_catalog_with_search_and_per_tool_details() {
    let workspace = tempfile::tempdir().unwrap();
    for width in [320.0, 360.0, 900.0, 1400.0] {
        let mut state = sample(workspace.path());
        snapshot(&mut state, workspace.path());
        let mut tree = UiTree::new(Element::column(render(&state, width < 640.0, width)));
        tree.set_reduced_motion(true);
        tree.layout(Size::new(width, 1100.0));
        // Discovery remains complete across the model's 128-tool boundary.
        // Focus must reveal both the first result and the end of the catalog.
        for index in [0, 127, 128, 169] {
            let id = format!("mcp_tool_check:analyze_artifact_{index:03}");
            assert!(tree.focus(&id), "missing catalog result {index} at {width}");
            let frame = tree.layout(Size::new(width, 1100.0));
            assert!(frame.regions.iter().any(|region| region.id == id));
        }
        assert_eq!(
            state.mcp_inspection.snapshot.as_ref().unwrap().tools.len(),
            170
        );
        state.mcp_presentation.tool_query = "169".into();
        let root = || Element::column(render(&state, width < 640.0, width)).gap(16.0);
        let mut tree = UiTree::new(root());
        let frame = tree.layout(Size::new(width, 2400.0));
        assert!(frame
            .regions
            .iter()
            .any(|r| r.id == "mcp_tool_check:analyze_artifact_169"));
        assert!(!frame
            .regions
            .iter()
            .any(|r| r.id == "mcp_tool_check:analyze_artifact_000"));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|t| t.source == state.mcp_inspection.snapshot.as_ref().unwrap().details_json));
        assert!(!frame
            .text_layouts()
            .iter()
            .any(|text| text.source == state.mcp_servers[0].program));
        state.mcp_presentation.expanded_tool = Some("analyze_artifact_169".into());
        let frame = UiTree::new(Element::column(render(&state, width < 640.0, width)))
            .layout(Size::new(width, 2400.0));
        let complete = serde_json::to_string_pretty(
            &state.mcp_inspection.snapshot.as_ref().unwrap().tools[169],
        )
        .unwrap();
        assert!(frame.text_layouts().iter().any(|t| t.source == complete));
        assert_eq!(
            frame
                .access
                .iter()
                .find(|node| node.id == "mcp_technical")
                .unwrap()
                .expanded,
            Some(false)
        );
        state.mcp_presentation.technical = true;
        let frame = UiTree::new(Element::column(render(&state, width < 640.0, width)))
            .layout(Size::new(width, 5000.0));
        assert_eq!(
            frame
                .access
                .iter()
                .find(|node| node.id == "mcp_technical")
                .unwrap()
                .expanded,
            Some(true)
        );
        assert!(frame
            .text_layouts()
            .iter()
            .any(|t| t.source == state.mcp_inspection.snapshot.as_ref().unwrap().details_json));
    }
}

#[test]
fn mcp_inspector_has_unique_controls_and_exports_native_layouts() {
    let workspace = tempfile::tempdir().unwrap();
    let context = eframe::egui::Context::default();
    crate::fonts::install(&context);
    for width in [320.0, 360.0, 900.0, 1400.0] {
        let surface = "settings";
        for mode in [
            "empty",
            "list",
            "disabled",
            "detail",
            "search",
            "expanded",
            "icons",
            "technical",
            "errors",
            "busy",
            "oauth",
        ] {
            let mut state = sample(workspace.path());

            if !matches!(mode, "empty" | "list" | "disabled") {
                snapshot(&mut state, workspace.path());
            }
            if mode == "empty" {
                state.mcp_servers.clear();
                state.mcp_effective_summary =
                    "0 servidores habilitados para el próximo turno".into();
            }
            if mode == "disabled" {
                state.mcp_servers[0].enabled = false;
                state.mcp_effective_summary = "1 servidor habilitado para el próximo turno".into();
            }
            if matches!(mode, "search" | "expanded") {
                state.mcp_presentation.tool_query = "169".into();
            }
            if mode == "expanded" {
                state.mcp_presentation.expanded_tool = Some("analyze_artifact_169".into());
            }
            if mode == "icons" {
                state.mcp_presentation.icon_picker = true;
                state.mcp_presentation.icon_query = "git".into();
            }
            if mode == "technical" {
                state.mcp_presentation.technical = true;
            }
            if mode == "errors" {
                state.mcp_inspection.snapshot = None;
                state.mcp_inspection.phase = crate::mcp_inspection::McpInspectionPhase::Failed;
                state.mcp_inspection.error=Some("No se pudo consultar el catálogo del servidor. Revisá su configuración y volvé a intentar.".into());
                state.mcp_inspection.shutdown_error =
                    Some("No se pudo confirmar el cierre de la conexión.".into());
                state.mcp_inspection.last.insert(
                    "local-analysis".into(),
                    crate::mcp_inspection::McpServerStatus::Failed("Error de fixture".into()),
                );
            }
            if mode == "busy" {
                state.mcp_inspection.snapshot = None;
                state.mcp_inspection.phase = crate::mcp_inspection::McpInspectionPhase::Running;
            }
            if mode == "oauth" {
                let server = &mut state.mcp_servers[0];
                server.program.clear();
                server.url = Some("https://mcp.fixture.invalid/mcp".into());
                server.http_oauth = Some(crate::mcp_settings::McpHttpOAuthSettings {
                    client_id: "synthetic-client".into(),
                    issuer: "https://auth.fixture.invalid".into(),
                    scopes: vec!["read".into()],
                });
                state.mcp_inspection.scope.as_mut().unwrap().server = server.clone();
                state.mcp_oauth_status = Some(crate::view::McpOAuthViewStatus {
                    server_id: server.id.clone(),
                    phase: crate::view::McpOAuthViewPhase::AwaitingBrowser {
                        url: "https://auth.fixture.invalid/authorize?state=synthetic-fixture"
                            .into(),
                    },
                });
            }
            let mut tree = UiTree::new(crate::view::build_view(&state, width));
            tree.set_reduced_motion(true);
            context.begin_pass(Default::default());
            limn_egui::attach_fonts(&mut tree, &context);
            tree.layout(Size::new(width, 1100.0));
            let primary = match mode {
                "empty" => "mcp_new",
                "list" | "disabled" => "mcp_toggle:local-analysis",
                "icons" => "mcp_brand_query",
                "technical" => "mcp_technical",
                "expanded" => "mcp_tool_expand:analyze_artifact_169",
                "errors" => "mcp_inspect:local-analysis",
                "busy" => "mcp_inspection_cancel",
                "oauth" => "mcp_oauth_open",
                _ => "mcp_tool_query",
            };
            assert!(
                tree.focus(primary),
                "missing {primary} in {surface}/{mode} at {width}"
            );
            tree.layout(Size::new(width, 1100.0));
            if mode == "technical" {
                tree.scroll_to_end("settings_page:Mcp");
            }
            let frame = tree.layout(Size::new(width, 1100.0));
            context.end_pass().textures_delta.clear();
            if matches!(mode, "list" | "disabled") {
                let node = frame
                    .access
                    .iter()
                    .find(|node| node.id == "mcp_toggle:local-analysis")
                    .unwrap();
                assert_eq!(node.role, Role::Switch);
                assert_eq!(node.toggled, Some(mode != "disabled"));
                assert!(!node.name.is_empty());
                assert!(!node.disabled && node.actionable);
                assert!(!frame
                    .access
                    .iter()
                    .any(|node| node.id == "mcp_inspect:local-analysis"
                        || node.id == "mcp_select:local-analysis"));
            }
            if mode == "busy" {
                let node = frame
                    .access
                    .iter()
                    .find(|node| node.id == "mcp_inspect:local-analysis");
                assert!(node.is_none_or(|node| node.disabled && !node.actionable));
            }
            if mode == "oauth" {
                let node = frame
                    .access
                    .iter()
                    .find(|node| node.id == "mcp_oauth_authorize:local-analysis");
                assert!(node.is_none_or(|node| node.disabled && !node.actionable));
            }
            let mut ids = std::collections::BTreeSet::new();
            for node in &frame.access {
                assert!(
                    ids.insert(&node.id),
                    "duplicate accessibility ID {} in {mode} at {width}",
                    node.id
                );
            }
            let mut hits = std::collections::BTreeSet::new();
            for region in frame.regions.iter().filter(|region| {
                region.kind == openide_ui::HitKind::Button
                    || region.kind == openide_ui::HitKind::TextInput
            }) {
                assert!(
                    hits.insert(&region.id),
                    "duplicate region ID {} in {surface}/{mode} at {width}",
                    region.id
                );
                assert!(
                    region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                    "control {} overflows {surface}/{mode} at {width}",
                    region.id
                );
            }
            if let Some(dir) = std::env::var_os("OPENIDE_MCP_VISUAL_DIR") {
                let name = format!("mcp-{surface}-{mode}-{width:.0}");
                if std::env::var("OPENIDE_MCP_VISUAL_FILTER")
                    .is_ok_and(|filter| !filter.split(',').any(|part| name.contains(part.trim())))
                {
                    continue;
                }
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{name}.svg"));
                crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, 1100.0)
                    .unwrap();
                let svg = resvg::usvg::Tree::from_data(
                    &std::fs::read(&path).unwrap(),
                    &crate::settings_ui::visual_qa::native_svg_options(),
                )
                .unwrap();
                let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, 1100).unwrap();
                resvg::render(
                    &svg,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                pixmap.save_png(path.with_extension("png")).unwrap();
            }
        }
    }
}
