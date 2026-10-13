//! The last explicit, temporary MCP review. Received schemas stay complete in
//! a bounded scroll area; no action dispatches an advertised MCP tool.

use super::{outline, page_note};
use crate::mcp_inspection::{McpInspectionPhase, McpInspectionUiState};
use crate::{settings_parts, theme};
use openide_ui::{Component, Element, Icon, ScrollArea, SkeletonGroup, Text, TextRole};

pub(super) fn render(state: &McpInspectionUiState, compact: bool) -> Element {
    let mut rows = vec![page_note(
        "Consulta explícita",
        "Inspeccionar conecta el servidor guardado, consulta sus herramientas y cierra la conexión. El listado observado puede cambiar; los turnos Build comprueban su propio catálogo al iniciar.",
        super::SettingsPage::Mcp.icon(),
    )];
    if let Some(scope) = &state.scope {
        rows.push(
            Text::new(
                format!("Servidor {}", scope.server.id),
                theme::component_palette(),
            )
            .render(),
        );
        rows.push(
            Text::new(
                format!("Proyecto de esta consulta: {}", scope.workspace.display()),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render(),
        );
    }
    let status = match state.phase {
        McpInspectionPhase::Idle => "Sin inspección en esta ventana",
        McpInspectionPhase::Running => "Conectando y consultando herramientas",
        McpInspectionPhase::Cancelling => "Cancelando y cerrando la conexión",
        McpInspectionPhase::Completed => "Inspección terminada · conexión cerrada",
        McpInspectionPhase::Failed => "La inspección o su cierre falló",
        McpInspectionPhase::Cancelled => "Inspección cancelada",
        McpInspectionPhase::Discarded => "Resultado descartado por cambio del alcance guardado",
    };
    rows.push(
        Text::new(status, theme::component_palette())
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render()
            .padding(settings_parts::ROW_PADDING),
    );
    if state.phase == McpInspectionPhase::Running {
        rows.push(
            SkeletonGroup::rows("mcp_inspection_progress", theme::component_palette(), 3)
                .row_height(26.0)
                .icon(true)
                .render()
                .padding(settings_parts::ROW_PADDING),
        );
    }
    if state.is_busy() {
        rows.push(
            outline("mcp_inspection_cancel", "Cancelar inspección")
                .icon(Icon::Close)
                .enabled(state.phase == McpInspectionPhase::Running)
                .loading(state.phase == McpInspectionPhase::Cancelling)
                .render(),
        );
    }
    if let Some(snapshot) = &state.snapshot {
        rows.push(
            Text::new(
                format!(
                    "Protocolo negociado: {}",
                    snapshot.server_info.protocol_version
                ),
                theme::component_palette(),
            )
            .render(),
        );
        rows.push(
            Text::new(
                format!(
                    "Servidor: {} · versión {} · {} herramientas recibidas",
                    snapshot.server_info.name,
                    snapshot.server_info.version,
                    snapshot.tools.len()
                ),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render(),
        );
        let observed = i64::try_from(snapshot.observed_at_unix_ms)
            .ok()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%d/%m/%Y %H:%M:%S %Z")
                    .to_string()
            })
            .unwrap_or_else(|| "Fecha no disponible".to_owned());
        rows.push(
            Text::new(format!("Observado: {observed}"), theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .render(),
        );
        rows.push(
            Text::new(
                format!(
                    "Huella SHA-256 de estos metadatos: {}",
                    snapshot.metadata_digest
                ),
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render(),
        );
        rows.push(Text::new("Esta huella identifica la inspección temporal. Los catálogos congelados de turnos anteriores conservan sus propias identidades.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
        if snapshot.catalog_changed {
            rows.push(Text::new("El servidor notificó un cambio de catálogo durante esta conexión. Repetí la inspección para obtener otro listado.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
        }
        rows.push(
            ScrollArea::new(
                "mcp_inspection_metadata",
                Text::new(&snapshot.details_json, theme::component_palette())
                    .mono()
                    .render()
                    .padding(10.0),
            )
            .render()
            .max_height(if compact { 260.0 } else { 340.0 }),
        );
    }
    if let Some(error) = &state.error {
        rows.push(
            ScrollArea::new(
                "mcp_inspection_error",
                Text::new(error, theme::component_palette())
                    .render()
                    .padding(10.0),
            )
            .render()
            .max_height(110.0),
        );
    }
    if let Some(error) = &state.shutdown_error {
        rows.push(
            Text::new(
                "No se pudo confirmar un cierre completo:",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
            .render(),
        );
        rows.push(
            ScrollArea::new(
                "mcp_inspection_shutdown_error",
                Text::new(error, theme::component_palette())
                    .render()
                    .padding(10.0),
            )
            .render()
            .max_height(110.0),
        );
    }
    settings_parts::settings_group("Última inspección temporal", rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_inspection::McpInspectionScope;
    use crate::mcp_settings::McpServerSettings;
    use openide_ui::{Point, Size, UiAction, UiEvent, UiTree};

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
            };
            let mut tree = UiTree::new(render(&state, width < 640.0));
            let frame = tree.layout(Size::new(width, 2400.0));
            let sources = frame.text_layouts();
            for full in [&snapshot.details_json, &error] {
                assert!(sources.iter().any(|layout| &layout.source == full));
            }
            assert!(sources
                .iter()
                .any(|layout| layout.source == "Protocolo negociado: 2025-06-18"));
            assert!(sources
                .iter()
                .any(|layout| layout.source.starts_with("Observado: ")
                    && !layout.source.contains("Unix")));
            for id in [
                "mcp_inspection_metadata",
                "mcp_inspection_error",
                "mcp_inspection_shutdown_error",
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
            url: Some("http://127.0.0.1:7777/mcp".into()),
            http_bearer_token_env_name: None,
            http_oauth: None,
            http_env_headers: Vec::new(),
        }
    }

    #[test]
    fn explicit_inspect_and_cancel_actions_are_reachable_and_busy_states_disable_duplicates() {
        for width in [420.0, 900.0] {
            let mut state = crate::view::tests::empty_state();
            state.show_settings = true;
            state.settings_page = crate::settings_ui::SettingsPage::Mcp;
            state.mcp_servers = vec![server()];
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
            assert!(tree
                .handle_event(UiEvent::PointerUp(point))
                .contains(&UiAction::Clicked { id: id.into() }));

            for phase in [McpInspectionPhase::Running, McpInspectionPhase::Cancelling] {
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
