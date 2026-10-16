use super::*;
use crate::mcp_presentation::{BrandChoice, McpPresentationPreferences};
use crate::mcp_tool_selection::McpToolSelections;

impl AgentsWindow {
    pub(super) fn refresh_mcp_presentation(&mut self) {
        if let Some(controller) = &mut self.active_mcp_discovery {
            controller.invalidate();
        }
        self.active_mcp_discovery = None;
        let workspace = Path::new(&self.view.workspace);
        let scope = self.view.mcp_settings_scope;
        let ui = &mut self.view.mcp_presentation;
        ui.workspace = self.view.workspace.clone();
        ui.canonical_workspace = workspace.canonicalize().ok();
        ui.discovery.clear();
        ui.discovery_error = None;
        match McpPresentationPreferences::load(&self.data_root, workspace, scope) {
            Ok(preferences) => {
                ui.preferences = preferences;
                ui.preferences_error = None;
            }
            Err(error) => {
                ui.preferences = Default::default();
                ui.preferences_error = Some(error);
            }
        }
        match McpToolSelections::load_scope(&self.data_root, workspace, scope) {
            Ok(selections) => {
                ui.selections = selections;
                ui.selection_error = None;
            }
            Err(error) => {
                ui.selections = Default::default();
                ui.selection_error = Some(error);
            }
        }
        if !ui
            .details
            .as_ref()
            .is_some_and(|id| self.view.mcp_servers.iter().any(|server| &server.id == id))
        {
            ui.details = None;
        }
        ui.icon_picker = false;
        self.reset_mcp_tool_draft();
        if self.view.shows(SettingsPage::Mcp) {
            self.start_mcp_discovery();
        }
    }

    fn start_mcp_discovery(&mut self) {
        match crate::mcp_discovery::McpDiscoveryController::start(
            Path::new(&self.view.workspace),
            self.view.mcp_settings_scope,
            self.view.mcp_servers.clone(),
        ) {
            Ok(controller) => self.active_mcp_discovery = Some(controller),
            Err(error) => self.view.mcp_presentation.discovery_error = Some(error),
        }
    }

    pub(super) fn poll_mcp_discovery(&mut self) -> bool {
        // Start lazily on either Settings or Marketplace's MCP surface.
        if !self.view.shows(SettingsPage::Mcp) {
            return false;
        }
        if self.active_mcp_discovery.is_none()
            && self.view.mcp_presentation.discovery.is_empty()
            && self.view.mcp_presentation.discovery_error.is_none()
            && !self.view.mcp_servers.is_empty()
        {
            self.refresh_mcp_presentation();
        }
        let Some(controller) = &mut self.active_mcp_discovery else {
            return false;
        };
        let Some(result) = controller.try_recv() else {
            return false;
        };
        let controller = self
            .active_mcp_discovery
            .take()
            .expect("polled MCP discovery");
        let workspace = Path::new(&self.view.workspace);
        if !controller.matches_saved(
            workspace,
            self.view.mcp_settings_scope,
            &self.view.mcp_servers,
        ) || !McpSettings::load_scope(&self.data_root, workspace, self.view.mcp_settings_scope)
            .is_ok_and(|saved| saved.servers == self.view.mcp_servers)
        {
            return false;
        }
        match result {
            Ok(found) => self.view.mcp_presentation.discovery = found,
            Err(error) => self.view.mcp_presentation.discovery_error = Some(error),
        }
        true
    }

    fn reset_mcp_tool_draft(&mut self) {
        let ui = &mut self.view.mcp_presentation;
        ui.selection_server = ui
            .details
            .as_ref()
            .and_then(|id| self.view.mcp_servers.iter().find(|server| &server.id == id))
            .cloned();
        // A stale registration keeps the visible old choice until the user
        // explicitly replaces it; it must never appear to have reverted to All.
        ui.selection_draft = ui
            .selection_server
            .as_ref()
            .and_then(|server| ui.selections.servers.get(&server.id))
            .map(|selection| selection.names.iter().cloned().collect());
        ui.selection_dirty = false;
    }

    fn save_mcp_icon(&mut self, choice: BrandChoice) -> Result<(), String> {
        let ui = &self.view.mcp_presentation;
        if !ui.icon_picker {
            return Err("Abrí el selector de iconos antes de elegir una marca".into());
        }
        if let Some(error) = &ui.preferences_error {
            return Err(error.clone());
        }
        let id = ui.details.as_ref().ok_or("Elegí un servidor MCP")?;
        if !self.view.mcp_servers.iter().any(|server| &server.id == id) {
            return Err("Servidor MCP no encontrado".into());
        }
        let workspace = Path::new(&self.view.workspace);
        let saved = McpPresentationPreferences::load(
            &self.data_root,
            workspace,
            self.view.mcp_settings_scope,
        )?;
        if saved != ui.preferences {
            return Err(
                "Los iconos guardados cambiaron. Recargá la configuración antes de guardar.".into(),
            );
        }
        let mut next = saved;
        if choice == BrandChoice::Auto {
            next.icons.remove(id);
        } else {
            next.icons.insert(id.clone(), choice);
        }
        // Discard presentation records for removed registrations, not unknown brand IDs.
        next.icons
            .retain(|id, _| self.view.mcp_servers.iter().any(|server| &server.id == id));
        next.save(&self.data_root, workspace, self.view.mcp_settings_scope)?;
        self.view.mcp_presentation.preferences = next;
        self.view.mcp_presentation.icon_picker = false;
        Ok(())
    }

    fn save_mcp_tool_selection(&mut self) -> Result<(), String> {
        let ui = &self.view.mcp_presentation;
        if !ui.selection_dirty {
            return Err("No hay cambios de selección para guardar".into());
        }
        if let Some(error) = &ui.selection_error {
            return Err(error.clone());
        }
        let server = ui
            .selection_server
            .as_ref()
            .ok_or("Elegí un servidor MCP")?;
        if ui.details.as_deref() != Some(server.id.as_str()) {
            return Err("El detalle del servidor ya está cerrado".into());
        }
        let workspace = Path::new(&self.view.workspace);
        let saved =
            McpSettings::load_scope(&self.data_root, workspace, self.view.mcp_settings_scope)?;
        if saved.servers != self.view.mcp_servers || !saved.servers.contains(server) {
            return Err(
                "La configuración del servidor cambió. Recargá e inspeccioná antes de guardar."
                    .into(),
            );
        }
        if ui.selection_draft.is_some()
            && !self
                .view
                .mcp_inspection
                .scope
                .as_ref()
                .is_some_and(|scope| {
                    scope.matches_saved(
                        workspace,
                        &McpSettings {
                            servers: vec![server.clone()],
                        },
                    )
                })
        {
            return Err("Inspeccioná esta configuración antes de seleccionar herramientas.".into());
        }
        if let Some(names) = &ui.selection_draft {
            let snapshot = self
                .view
                .mcp_inspection
                .snapshot
                .as_ref()
                .ok_or("Inspeccioná el catálogo antes de guardar la selección")?;
            if names
                .iter()
                .any(|name| !snapshot.tools.iter().any(|tool| &tool.name == name))
            {
                return Err("La selección contiene herramientas ausentes del catálogo actual. Elegí Ninguna y seleccioná las herramientas disponibles.".into());
            }
        }
        let mut selections = McpToolSelections::load_scope(
            &self.data_root,
            workspace,
            self.view.mcp_settings_scope,
        )?;
        if selections != ui.selections {
            return Err("La selección guardada cambió. Recargá antes de guardar.".into());
        }
        selections.set_for(
            server,
            ui.selection_draft
                .as_ref()
                .map(|names| names.iter().cloned().collect()),
        )?;
        selections.save_scope(&self.data_root, workspace, self.view.mcp_settings_scope)?;
        self.view.mcp_presentation.selections = selections;
        self.view.mcp_presentation.selection_dirty = false;
        Ok(())
    }

    pub(super) fn handle_mcp_presentation_action(&mut self, id: &str) -> bool {
        if !(id.starts_with("mcp_details:")
            || id.starts_with("mcp_brand:")
            || id.starts_with("mcp_tool_check:")
            || id.starts_with("mcp_tool_expand:")
            || matches!(
                id,
                "mcp_details_back"
                    | "mcp_brand_picker"
                    | "mcp_brand_cancel"
                    | "mcp_technical"
                    | "mcp_tools_all"
                    | "mcp_tools_none"
                    | "mcp_tools_save"
                    | "mcp_tools_revert"
            ))
        {
            return false;
        }
        if !self.view.shows(SettingsPage::Mcp) || self.view.mcp_editor_open {
            return true;
        }
        if !id.starts_with("mcp_details:")
            && id != "mcp_details_back"
            && self.view.mcp_presentation.details.is_none()
        {
            return true;
        }
        let result = if let Some(server_id) = id.strip_prefix("mcp_details:") {
            if self
                .view
                .mcp_servers
                .iter()
                .any(|server| server.id == server_id)
            {
                let ui = &mut self.view.mcp_presentation;
                ui.details = Some(server_id.to_owned());
                ui.tool_query.clear();
                ui.expanded_tool = None;
                ui.technical = false;
                ui.icon_picker = false;
                self.reset_mcp_tool_draft();
                self.tree.set_input_value("mcp_tool_query", "");
            }
            Ok(())
        } else if let Some(key) = id.strip_prefix("mcp_brand:") {
            match key {
                "auto" => self.save_mcp_icon(BrandChoice::Auto),
                "none" => self.save_mcp_icon(BrandChoice::None),
                id if crate::brand_icons::get(id).is_some() => {
                    self.save_mcp_icon(BrandChoice::Brand(id.to_owned()))
                }
                _ => Err("El icono no pertenece al catálogo disponible".into()),
            }
        } else if let Some(name) = id.strip_prefix("mcp_tool_expand:") {
            let ui = &mut self.view.mcp_presentation;
            ui.expanded_tool = (ui.expanded_tool.as_deref() != Some(name)).then(|| name.to_owned());
            Ok(())
        } else if let Some(name) = id.strip_prefix("mcp_tool_check:") {
            let valid = self
                .view
                .mcp_inspection
                .scope
                .as_ref()
                .zip(self.view.mcp_presentation.selection_server.as_ref())
                .is_some_and(|(scope, server)| {
                    scope.matches_saved(
                        Path::new(&self.view.workspace),
                        &McpSettings {
                            servers: vec![server.clone()],
                        },
                    )
                });
            if valid {
                if let Some(snapshot) = &self.view.mcp_inspection.snapshot {
                    if snapshot.tools.iter().any(|tool| tool.name == name) {
                        let ui = &mut self.view.mcp_presentation;
                        let names = ui.selection_draft.get_or_insert_with(|| {
                            snapshot
                                .tools
                                .iter()
                                .map(|tool| tool.name.clone())
                                .collect()
                        });
                        if !names.remove(name) {
                            names.insert(name.to_owned());
                        }
                        ui.selection_dirty = true;
                    }
                }
            }
            Ok(())
        } else {
            match id {
                "mcp_details_back" => {
                    self.view.mcp_presentation.details = None;
                    self.view.mcp_presentation.icon_picker = false;
                    self.reset_mcp_tool_draft();
                    Ok(())
                }
                "mcp_brand_picker" => {
                    self.view.mcp_presentation.icon_picker ^= true;
                    if self.view.mcp_presentation.icon_picker {
                        self.view.mcp_presentation.icon_query.clear();
                        self.tree.set_input_value("mcp_brand_query", "");
                    }
                    Ok(())
                }
                "mcp_brand_cancel" => {
                    self.view.mcp_presentation.icon_picker = false;
                    Ok(())
                }
                "mcp_technical" => {
                    self.view.mcp_presentation.technical ^= true;
                    Ok(())
                }
                "mcp_tools_all" => {
                    self.view.mcp_presentation.selection_draft = None;
                    self.view.mcp_presentation.selection_dirty = true;
                    Ok(())
                }
                "mcp_tools_none" => {
                    self.view.mcp_presentation.selection_draft = Some(Default::default());
                    self.view.mcp_presentation.selection_dirty = true;
                    Ok(())
                }
                "mcp_tools_revert" => {
                    self.reset_mcp_tool_draft();
                    Ok(())
                }
                "mcp_tools_save" => self.save_mcp_tool_selection().map(|()| {
                    self.set_settings_notice("Selección guardada para los próximos turnos", false)
                }),
                _ => Ok(()),
            }
        };
        if let Err(error) = result {
            self.set_settings_notice(error, true);
        }
        true
    }
}

#[cfg(test)]
#[path = "mcp_presentation_app_tests.rs"]
mod tests;
