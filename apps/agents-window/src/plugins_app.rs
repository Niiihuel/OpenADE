//! Explicit plugin operations run off the UI thread, scoped to the observed project.
use super::*;
use crate::plugins::{InstalledPlugin, PluginPreview, PluginStore};

pub(super) enum PluginOperation {
    Load,
    PickFolder,
    Inspect(PathBuf),
    Install(PluginPreview),
    Toggle(InstalledPlugin),
    Remove(InstalledPlugin),
}

pub(super) struct PluginResult {
    installed: Vec<InstalledPlugin>,
    preview: Option<PluginPreview>,
    message: Option<String>,
    close_dialogs: bool,
}
pub(super) type PluginResponse = (String, bool, Result<PluginResult, String>);

impl AgentsWindow {
    fn plugin_removal_blocked(&self) -> bool {
        self.view.running
            || self.view.background_run_selected
            || self.active_run.is_some()
            || self.active_resume.is_some()
            || self.active_child_recovery.is_some()
            || !self.subagent_bridge.active_child_ids().is_empty()
            || !self.subagent_dispatches.is_empty()
            || !self.background_automation_runs.is_empty()
    }

    pub(super) fn plugin_operation(&mut self, operation: PluginOperation) {
        if self.plugin_response.is_some() {
            return;
        }
        if matches!(operation, PluginOperation::Remove(_)) && self.plugin_removal_blocked() {
            self.view.plugins.status =
                Some("Espera a que terminen los agentes para desinstalar plugins.".into());
            self.view.plugins.error = true;
            return;
        }
        let workspace = self.view.workspace.clone();
        let inventory_load = matches!(operation, PluginOperation::Load);
        self.plugin_removal_pending = matches!(operation, PluginOperation::Remove(_));
        let root = self.data_root.clone();
        self.view.plugins.workspace = workspace.clone();
        self.view.plugins.busy = true;
        self.view.plugins.error = false;
        self.view.plugins.status = Some(
            match &operation {
                PluginOperation::Load => "Cargando plugins…",
                PluginOperation::PickFolder => "Selecciona la carpeta del plugin…",
                PluginOperation::Inspect(_) => "Revisando el paquete…",
                PluginOperation::Install(_) => "Instalando plugin…",
                PluginOperation::Toggle(plugin) => {
                    if plugin.enabled {
                        "Desactivando plugin…"
                    } else {
                        "Activando plugin…"
                    }
                }
                PluginOperation::Remove(_) => "Desinstalando plugin…",
            }
            .into(),
        );
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("plugin-management".into())
            .spawn(move || {
                let result = (|| {
                    let project = Path::new(&workspace);
                    let mut preview = None;
                    let mut close_dialogs = false;
                    let message = match operation {
                        PluginOperation::Load => None,
                        PluginOperation::PickFolder => {
                            if let Some(path) =
                                choose_workspace_folder(project, "Seleccionar plugin de OpenIDE")?
                            {
                                preview = Some(PluginStore::inspect(&path)?);
                            }
                            None
                        }
                        PluginOperation::Inspect(path) => {
                            preview = Some(PluginStore::inspect(&path)?);
                            None
                        }
                        PluginOperation::Install(observed) => {
                            PluginStore::install(&root, project, &observed)?;
                            close_dialogs = true;
                            Some("Plugin instalado. Puedes activarlo para este proyecto.".into())
                        }
                        PluginOperation::Toggle(observed) => {
                            validate_observed_plugin(&root, project, &observed)?;
                            PluginStore::set_enabled(
                                &root,
                                project,
                                &observed.manifest.id,
                                !observed.enabled,
                            )?;
                            Some(
                                if observed.enabled {
                                    "Plugin desactivado para los próximos turnos."
                                } else {
                                    "Plugin activado para los próximos turnos de ejecución."
                                }
                                .into(),
                            )
                        }
                        PluginOperation::Remove(observed) => {
                            validate_observed_plugin(&root, project, &observed)?;
                            PluginStore::uninstall(&root, &observed.manifest.id)?;
                            close_dialogs = true;
                            Some("Plugin desinstalado. La carpeta original se conserva.".into())
                        }
                    };
                    Ok(PluginResult {
                        installed: PluginStore::list(&root, project)?,
                        preview,
                        message,
                        close_dialogs,
                    })
                })();
                let _ = sender.send((workspace, inventory_load, result));
            }) {
            Ok(_) => self.plugin_response = Some(receiver),
            Err(error) => {
                self.plugin_removal_pending = false;
                self.view.plugins.busy = false;
                self.view.plugins.error = true;
                self.view.plugins.status =
                    Some(format!("No se pudo iniciar la operación: {error}"));
            }
        }
    }

    pub(super) fn poll_plugins(&mut self) -> bool {
        let blocked = self.plugin_removal_blocked();
        let mut changed = self.view.plugins.removal_blocked != blocked;
        self.view.plugins.removal_blocked = blocked;
        let result = self
            .plugin_response
            .as_ref()
            .map(|receiver| receiver.try_recv());
        match result {
            Some(Ok((workspace, inventory_load, result))) => {
                self.plugin_response = None;
                self.plugin_removal_pending = false;
                self.view.plugins.busy = false;
                changed = true;
                if workspace == self.view.workspace {
                    match result {
                        Ok(result) => {
                            self.view.plugins.installed = result.installed;
                            self.view.plugins.status = result.message;
                            self.view.plugins.error = false;
                            self.view.plugins.load_error = false;
                            if result.close_dialogs {
                                self.view.plugins.close_dialogs();
                            } else if self.view.plugins.install_open {
                                if let Some(preview) = result.preview {
                                    self.view.plugins.source = preview.source.display().to_string();
                                    self.view.plugins.preview = Some(preview);
                                }
                            }
                        }
                        Err(error) => {
                            self.view.plugins.status = Some(error);
                            self.view.plugins.error = true;
                            if inventory_load {
                                self.view.plugins.load_error = true;
                            }
                        }
                    }
                } else {
                    self.view.plugins = Default::default();
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.plugin_response = None;
                self.plugin_removal_pending = false;
                self.view.plugins.busy = false;
                self.view.plugins.error = true;
                self.view.plugins.status = Some("La operación de plugins terminó sin resultado. Puedes actualizar para volver a intentarlo.".into());
                changed = true;
            }
            _ => {}
        }
        let visible = self.view.show_settings && self.view.settings_page == SettingsPage::Plugins;
        if visible
            && self.view.plugins.workspace != self.view.workspace
            && self.plugin_response.is_none()
        {
            self.view.plugins = Default::default();
            self.plugin_operation(PluginOperation::Load);
            changed = true;
        }
        changed
    }

    pub(super) fn apply_plugin_action(&mut self, action: &UiAction) -> bool {
        let id = match action {
            UiAction::Clicked { id } | UiAction::InputChanged { id, .. } => id,
            _ => return false,
        };
        if !id.starts_with("plugins_") {
            return false;
        }
        if id == "plugins_open" && matches!(action, UiAction::Clicked { .. }) {
            self.view.composer_add_menu_open = false;
            self.view.show_settings = true;
            self.view.settings_page = SettingsPage::Plugins;
            self.view.plugins.close_dialogs();
            return true;
        }
        if !self.view.show_settings || self.view.settings_page != SettingsPage::Plugins {
            return true;
        }
        if self.view.plugins.workspace != self.view.workspace {
            return true;
        }
        if let UiAction::InputChanged { value, .. } = action {
            match id.as_str() {
                "plugins_search" => self.view.plugins.query = value.chars().take(256).collect(),
                "plugins_source" if self.view.plugins.install_open && !self.view.plugins.busy => {
                    self.view.plugins.source = value.chars().take(4096).collect();
                    self.view.plugins.preview = None;
                }
                _ => {}
            }
            return true;
        }
        match id.as_str() {
            "plugins_dialog_close" | "plugins_remove_cancel" => {
                self.view.plugins.close_dialogs();
                return true;
            }
            _ if self.view.plugins.busy => return true,
            "plugins_refresh" => self.plugin_operation(PluginOperation::Load),
            "plugins_install_open" => {
                self.view.plugins.close_dialogs();
                self.view.plugins.install_open = true;
                self.view.plugins.status = None;
            }
            "plugins_pick_folder" if self.view.plugins.install_open => {
                self.view.plugins.preview = None;
                self.plugin_operation(PluginOperation::PickFolder)
            }
            "plugins_inspect" if self.view.plugins.install_open => {
                self.view.plugins.preview = None;
                self.plugin_operation(PluginOperation::Inspect(PathBuf::from(
                    self.view.plugins.source.trim(),
                )));
            }
            "plugins_install_confirm" if self.view.plugins.install_open => {
                if let Some(preview) = self.view.plugins.preview.clone() {
                    self.plugin_operation(PluginOperation::Install(preview));
                }
            }
            "plugins_remove_confirm" => {
                if let Some(plugin) = self.view.plugins.pending_remove.clone() {
                    self.plugin_operation(PluginOperation::Remove(plugin));
                }
            }
            _ => {
                let Some((verb, plugin_id)) = id.split_once(':') else {
                    return true;
                };
                let Some(plugin) = self
                    .view
                    .plugins
                    .installed
                    .iter()
                    .find(|plugin| plugin.manifest.id == plugin_id)
                    .cloned()
                else {
                    return true;
                };
                match verb {
                    "plugins_details" => {
                        self.view.plugins.close_dialogs();
                        self.view.plugins.selected = Some(plugin_id.to_owned());
                    }
                    "plugins_toggle" if !self.view.plugins.load_error => {
                        self.plugin_operation(PluginOperation::Toggle(plugin))
                    }
                    "plugins_remove"
                        if self.view.plugins.selected.as_deref() == Some(plugin_id)
                            && !self.plugin_removal_blocked() =>
                    {
                        self.view.plugins.pending_remove = Some(plugin)
                    }
                    _ => {}
                }
            }
        }
        true
    }
}

fn validate_observed_plugin(
    root: &Path,
    workspace: &Path,
    observed: &InstalledPlugin,
) -> Result<(), String> {
    let current = PluginStore::list(root, workspace)?
        .into_iter()
        .find(|plugin| plugin.manifest.id == observed.manifest.id);
    match current {
        Some(current)
            if current.digest == observed.digest && current.enabled == observed.enabled =>
        {
            Ok(())
        }
        _ => Err("El plugin cambió. Actualiza la lista y vuelve a revisar la operación.".into()),
    }
}

#[cfg(test)]
#[path = "plugins_app_tests.rs"]
mod tests;
