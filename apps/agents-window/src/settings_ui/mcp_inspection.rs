//! Read-only metadata from the last explicit, temporary MCP review.
//! Received schemas stay complete in a bounded scroll area; no action invokes
//! an advertised MCP tool. Inspection actions and errors belong to its parent.

use crate::mcp_inspection::McpInspectionUiState;
use crate::{settings_parts, theme};
use openide_ui::{
    Component, ControlSize, DescriptionItem, DescriptionList, Element, ScrollArea, Text, TextRole,
};

fn fact(label: &str, value: impl Into<String>) -> DescriptionItem {
    DescriptionItem::with(
        label,
        Text::new(value, theme::component_palette())
            .role(TextRole::Small)
            .mono()
            .render()
            .flex(1.0),
    )
}

fn note(value: &str) -> Element {
    Text::new(value, theme::component_palette())
        .role(TextRole::Small)
        .muted()
        .render()
}

/// The technical disclosure owns its heading. This body adds no inspection
/// controls or errors already presented beside the server's tools.
pub(super) fn metadata(state: &McpInspectionUiState, compact: bool) -> Element {
    let mut facts = Vec::new();
    if let Some(scope) = &state.scope {
        facts.push(fact("Servidor guardado", &scope.server.id));
        facts.push(fact(
            "Proyecto de esta consulta",
            scope.workspace.display().to_string(),
        ));
    }
    if let Some(snapshot) = &state.snapshot {
        facts.extend([
            fact(
                "Protocolo negociado",
                &snapshot.server_info.protocol_version,
            ),
            fact("Servidor anunciado", &snapshot.server_info.name),
            fact("Versión anunciada", &snapshot.server_info.version),
            fact("Herramientas recibidas", snapshot.tools.len().to_string()),
        ]);
        let observed = i64::try_from(snapshot.observed_at_unix_ms)
            .ok()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%d/%m/%Y %H:%M:%S %Z")
                    .to_string()
            })
            .unwrap_or_else(|| "Fecha no disponible".to_owned());
        facts.push(fact("Observado", observed));
        facts.push(fact("Huella SHA-256", &snapshot.metadata_digest));
    }
    let mut list = DescriptionList::new(facts, theme::component_palette())
        .size(ControlSize::Compact)
        .label_width(146.0)
        .dividers(false);
    if compact {
        list = list.stacked();
    }
    let mut rows = vec![list.render()];
    if let Some(snapshot) = &state.snapshot {
        rows.push(note("Esta huella identifica la inspección temporal. Los catálogos congelados de turnos anteriores conservan sus propias identidades."));
        if snapshot.catalog_changed {
            rows.push(note("El servidor notificó un cambio de catálogo durante esta conexión. Repetí la inspección para obtener otro listado."));
        }
        rows.push(
            ScrollArea::new(
                "mcp_inspection_metadata",
                Text::new(&snapshot.details_json, theme::component_palette())
                    .role(TextRole::Small)
                    .mono()
                    .render()
                    .padding(8.0),
            )
            .render()
            .max_height(if compact { 260.0 } else { 340.0 }),
        );
    }
    Element::column(rows)
        .gap(10.0)
        .padding(settings_parts::ROW_PADDING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_inspection::{McpInspectionPhase, McpInspectionScope};
    use crate::mcp_settings::McpServerSettings;
    use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn technical_metadata_wraps_with_native_fonts_keeps_padding_and_adds_no_actions() {
        let workspace = std::path::PathBuf::from(
            "/workspace/proyecto-con-un-nombre-extenso/componentes/de-la-integracion-y-sus-herramientas",
        );
        let snapshot = crate::mcp_inspection::tests::sample_snapshot();
        let state = McpInspectionUiState {
            scope: Some(McpInspectionScope {
                request_id: 1,
                workspace: workspace.clone(),
                server: server(),
            }),
            phase: McpInspectionPhase::Completed,
            snapshot: Some(snapshot.clone()),
            error: Some("El error pertenece al panel de herramientas".into()),
            ..Default::default()
        };
        let workspace_text = workspace.to_string_lossy().into_owned();
        for width in [280.0, 360.0, 900.0] {
            let (context, mut tree) =
                crate::view::ide_visual_tests::native_tree(metadata(&state, width < 640.0));
            let frame = tree.layout(Size::new(width, 2000.0));
            let blocks = frame.text_layouts();
            for value in [
                workspace_text.as_str(),
                snapshot.metadata_digest.as_str(),
                "2025-06-18",
            ] {
                let block = blocks
                    .iter()
                    .find(|block| block.source == value)
                    .expect("complete technical fact");
                assert!(!block.runs.is_empty());
                for run in &block.runs {
                    assert!(run.rect.x >= settings_parts::ROW_PADDING - 0.1);
                    assert!(
                        run.rect.x + run.rect.width <= width - settings_parts::ROW_PADDING + 0.1,
                        "{width}: {:?}",
                        run.text
                    );
                    assert!(
                        run.visible
                            .is_some_and(|visible| visible.height >= run.rect.height - 0.1)
                    );
                }
            }
            assert!(
                blocks
                    .iter()
                    .any(|block| block.source == snapshot.details_json)
            );
            assert!(
                !blocks
                    .iter()
                    .any(|block| block.source.contains("El error pertenece"))
            );
            assert!(
                frame
                    .regions
                    .iter()
                    .all(|region| region.kind != openide_ui::HitKind::Button)
            );
            assert!(frame.access.iter().all(|node| !node.actionable));
            context.end_pass().textures_delta.clear();
        }
    }

    #[test]
    fn inspection_details_preserve_complete_schema_diagnostics_and_negotiated_version() {
        let root = tempfile::tempdir().unwrap();
        let scope = McpInspectionScope {
            request_id: 71,
            workspace: root.path().to_path_buf(),
            server: server(),
        };
        let snapshot = crate::mcp_inspection::tests::sample_snapshot();
        let error = format!("{}FIN_ERROR", "Diagnóstico MCP completo. ".repeat(80));
        for width in [360.0, 900.0] {
            let state = McpInspectionUiState {
                scope: Some(scope.clone()),
                phase: McpInspectionPhase::Failed,
                snapshot: Some(snapshot.clone()),
                error: Some(error.clone()),
                shutdown_error: Some("Cierre no confirmado: FIN_CIERRE".into()),
                ..Default::default()
            };
            let mut view = crate::view::tests::empty_state();
            view.show_settings = true;
            view.settings_page = crate::settings_ui::SettingsPage::Mcp;
            view.workspace = root.path().to_string_lossy().into_owned();
            view.mcp_servers = vec![server()];
            view.mcp_presentation.details = Some(server().id);
            view.mcp_presentation.technical = true;
            view.mcp_inspection = state;
            let mut tree = UiTree::new(crate::view::build_view(&view, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 5000.0));
            let sources = frame.text_layouts();
            for full in [&snapshot.details_json, &error] {
                assert!(sources.iter().any(|layout| &layout.source == full));
            }
            for label in ["Protocolo negociado", "Observado"] {
                assert!(
                    frame.commands.iter().any(|command| matches!(command,
                    openide_ui::DrawCommand::Text { text, .. } if text == label)),
                    "complete metadata label must be painted: {label} at {width}"
                );
            }
            assert!(sources.iter().any(|layout| layout.source == "2025-06-18"));
            assert!(
                sources
                    .iter()
                    .any(|layout| layout.source.as_bytes().get(2) == Some(&b'/')
                        && layout.source.as_bytes().get(5) == Some(&b'/')
                        && layout.source.as_bytes().get(10) == Some(&b' ')
                        && layout.source.contains(':'))
            );
            for id in [
                "mcp_inspection_metadata",
                "mcp_detail_error:0",
                "mcp_detail_error:1",
            ] {
                assert!(frame.regions.iter().any(|region| region.id == id));
            }
        }
    }

    fn server() -> McpServerSettings {
        McpServerSettings {
            id: "inspection-local".into(),
            enabled: true,
            program: String::new(),
            args: vec![],
            cwd: None,
            credential_env_names: vec![],
            env: Default::default(),
            url: Some("http://127.0.0.1:7777/mcp".into()),
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        }
    }

    #[test]
    fn explicit_inspect_and_cancel_actions_are_reachable_and_busy_states_disable_duplicates() {
        let workspace = tempfile::tempdir().unwrap();
        for width in [420.0, 900.0] {
            let mut state = crate::view::tests::empty_state();
            state.show_settings = true;
            state.settings_page = crate::settings_ui::SettingsPage::Mcp;
            state.workspace = workspace.path().to_string_lossy().into_owned();
            state.mcp_servers = vec![server()];
            state.mcp_presentation.details = Some("inspection-local".into());
            let id = "mcp_inspect:inspection-local";
            let mut tree = UiTree::new(crate::view::build_view(&state, width));
            let viewport = Size::new(width, 700.0);
            tree.layout(viewport);
            assert!(tree.focus(id));
            let frame = tree.layout(viewport);
            let region = frame.regions.iter().find(|region| region.id == id).unwrap();
            assert!(region.rect.x >= 0.0 && region.rect.x + region.rect.width <= width + 0.1);
            let point = Point::new(
                region.rect.x + region.rect.width / 2.0,
                region.rect.y + region.rect.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert!(
                tree.handle_event(UiEvent::PointerUp(point))
                    .contains(&UiAction::Clicked { id: id.into() })
            );

            for phase in [McpInspectionPhase::Running, McpInspectionPhase::Cancelling] {
                state.mcp_presentation.details = Some("inspection-local".into());
                state.mcp_inspection.scope = Some(McpInspectionScope {
                    request_id: 1,
                    workspace: workspace.path().canonicalize().unwrap(),
                    server: server(),
                });
                state.mcp_inspection.phase = phase;
                let mut tree = UiTree::new(crate::view::build_view(&state, width));
                tree.layout(viewport);
                assert!(!tree.focus(id));
                if phase == McpInspectionPhase::Running {
                    assert!(tree.focus("mcp_inspection_cancel"));
                    let frame = tree.layout(viewport);
                    let region = frame
                        .regions
                        .iter()
                        .find(|region| region.id == "mcp_inspection_cancel")
                        .unwrap();
                    let point = Point::new(
                        region.rect.x + region.rect.width / 2.0,
                        region.rect.y + region.rect.height / 2.0,
                    );
                    tree.handle_event(UiEvent::PointerDown(point));
                    assert!(tree.handle_event(UiEvent::PointerUp(point)).contains(
                        &UiAction::Clicked {
                            id: "mcp_inspection_cancel".into()
                        }
                    ));
                } else {
                    assert!(!tree.focus("mcp_inspection_cancel"));
                }
            }
            state.mcp_inspection.phase = McpInspectionPhase::Idle;
            state.mcp_servers[0].enabled = false;
            let mut tree = UiTree::new(crate::view::build_view(&state, width));
            tree.layout(viewport);
            assert!(!tree.focus(id));
        }
    }
}
