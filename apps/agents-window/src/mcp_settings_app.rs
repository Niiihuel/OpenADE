//! Import only local metadata on a worker. Its result belongs to the exact
//! saved scope the user reviewed when starting the action.
use super::*;

impl AgentsWindow {
    pub(super) fn invalidate_mcp_import(&mut self) {
        if let Some(controller) = &mut self.active_mcp_import {
            controller.invalidate();
        }
    }

    pub(super) fn start_mcp_import(&mut self) {
        if !self.view.shows(SettingsPage::Mcp) || self.view.mcp_editor_open {
            return;
        }
        if self.active_mcp_import.is_some() {
            self.set_settings_notice("La importación MCP sigue en curso", false);
            return;
        }
        let scope = self.view.mcp_settings_scope;
        let workspace = Path::new(&self.view.workspace);
        let saved = match McpSettings::load_scope(&self.data_root, workspace, scope) {
            Ok(saved) => saved,
            Err(error) => {
                self.set_settings_notice(error, true);
                return;
            }
        };
        if saved.servers != self.view.mcp_servers {
            self.refresh_mcp_settings();
            self.set_settings_notice(
                "La configuración MCP guardada cambió. Revisá la lista actual antes de importar.",
                true,
            );
            return;
        }
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        if home.as_os_str().is_empty() {
            self.set_settings_notice(
                "No se pudo encontrar la carpeta del usuario para importar MCP",
                true,
            );
            return;
        }
        match crate::mcp_import::McpImportController::start(home, workspace, scope, saved.servers) {
            Ok(controller) => {
                self.active_mcp_import = Some(controller);
                self.set_settings_notice(
                    "Buscando configuraciones MCP de Codex, Claude Code y Cursor…",
                    false,
                );
            }
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    pub(super) fn poll_mcp_import(&mut self) -> bool {
        let Some(controller) = self.active_mcp_import.as_mut() else {
            return false;
        };
        let Some(result) = controller.try_recv() else {
            return false;
        };
        let controller = self.active_mcp_import.take().expect("polled MCP import");
        let saved = McpSettings::load_scope(
            &self.data_root,
            Path::new(&self.view.workspace),
            self.view.mcp_settings_scope,
        );
        if !self.view.shows(SettingsPage::Mcp)
            || !saved.as_ref().is_ok_and(|saved| {
                controller.matches_saved(
                    Path::new(&self.view.workspace),
                    self.view.mcp_settings_scope,
                    saved,
                )
            })
            || self.view.mcp_servers != controller.known
        {
            self.set_settings_notice("La configuración, el ámbito o el proyecto cambió; se descartó la importación MCP. Repetí la búsqueda para el alcance actual.", true);
            return true;
        }
        let found = match result {
            Ok(found) => found,
            Err(error) => {
                self.set_settings_notice(error, true);
                return true;
            }
        };
        let room = crate::mcp_settings::MAX_SERVERS.saturating_sub(self.view.mcp_servers.len());
        let count = found.servers.len().min(room);
        let omitted = found.skipped + found.servers.len().saturating_sub(count);
        let diagnostics = if found.errors.is_empty() {
            String::new()
        } else {
            format!(" {}", found.errors.join(" · "))
        };
        if count == 0 {
            let message = if room == 0 && !found.servers.is_empty() {
                "La lista de este ámbito está completa: eliminá algún servidor antes de importar"
            } else {
                "No se encontraron servidores MCP nuevos para este ámbito"
            };
            let skipped = if omitted == 0 {
                String::new()
            } else {
                format!(" {omitted} no se pudieron importar.")
            };
            self.set_settings_notice(
                format!("{message}.{skipped}{diagnostics}"),
                omitted > 0 || !found.errors.is_empty(),
            );
        } else {
            let mut servers = self.view.mcp_servers.clone();
            servers.extend(found.servers.into_iter().take(count));
            let skipped = if omitted == 0 {
                String::new()
            } else {
                format!(" {omitted} quedaron fuera.")
            };
            self.commit_mcp_settings(servers, &format!("Se importaron {count} servidores MCP en este ámbito, deshabilitados para que los revises.{skipped}{diagnostics}"));
        }
        true
    }
}

#[cfg(test)]
#[path = "mcp_settings_app_tests.rs"]
mod tests;
