//! Host delegation preferences affect future turn profiles, never running workers.
use super::*;
use crate::subagent_settings::{self, SubagentLimit, SubagentSettings};

impl AgentsWindow {
    pub(super) fn refresh_subagent_settings(&mut self) {
        match SubagentSettings::load(&self.data_root) {
            Ok(settings) => {
                self.subagent_settings = settings;
                self.subagent_settings_error = None;
            }
            Err(error) => self.subagent_settings_error = Some(error),
        }
        self.view.subagent_settings = self.subagent_settings;
        self.view.subagent_settings_error = self.subagent_settings_error.clone();
        self.view.subagent_settings_reset_pending = false;
        self.view.subagent_personas = subagent_settings::personas(&self.data_root);
        for limit in SubagentLimit::ALL {
            self.tree.set_input_value(
                &format!("subagent_setting:{}", limit.key()),
                limit.value(&self.subagent_settings).to_string(),
            );
        }
    }

    /// Reload even when Settings has never been opened; malformed disk state
    /// must not silently admit a new turn with remembered/default authority.
    pub(super) fn subagent_settings_for_run(&self) -> Result<SubagentSettings, String> {
        SubagentSettings::load(&self.data_root)
    }

    pub(super) fn apply_subagent_settings_action(
        &mut self,
        action: &UiAction,
        _ctx: &egui::Context,
    ) -> bool {
        let id = match action {
            UiAction::Clicked { id }
                if id.starts_with("subagent_settings_") || id.starts_with("subagent_setting:") =>
            {
                id
            }
            UiAction::InputChanged { id, .. } | UiAction::Submitted { id, .. }
                if id.starts_with("subagent_setting:") =>
            {
                id
            }
            _ => return false,
        };
        if !self.view.shows(SettingsPage::Subagents) {
            return true;
        }
        match id.as_str() {
            "subagent_settings_reload" => {
                self.refresh_subagent_settings();
                if let Some(error) = self.subagent_settings_error.clone() {
                    self.set_settings_notice(error, true);
                } else {
                    self.set_settings_notice("Configuración de subagentes recargada", false);
                }
                return true;
            }
            "subagent_settings_reset" if self.subagent_settings_error.is_some() => {
                self.view.subagent_settings_reset_pending = true;
                return true;
            }
            "subagent_settings_reset_cancel" => {
                self.view.subagent_settings_reset_pending = false;
                return true;
            }
            "subagent_settings_reset_confirm" => {
                if self.subagent_settings_error.is_some()
                    && self.view.subagent_settings_reset_pending
                {
                    match SubagentSettings::reset_with_backup(&self.data_root) {
                        Ok((_, backup)) => {
                            self.refresh_subagent_settings();
                            self.set_settings_notice(
                                format!(
                                    "Configuración restablecida. Copia guardada: {}",
                                    backup.display()
                                ),
                                false,
                            );
                        }
                        Err(error) => self.set_settings_notice(error, true),
                    }
                }
                return true;
            }
            _ => {}
        }
        // Protect external edits as well as errors discovered at startup.
        let mut next = match SubagentSettings::load(&self.data_root) {
            Ok(settings) => settings,
            Err(error) => {
                self.subagent_settings_error = Some(error.clone());
                self.view.subagent_settings_error = Some(error.clone());
                self.set_settings_notice(error, true);
                return true;
            }
        };
        if self.subagent_settings_error.is_some() {
            return true; // Explicit reload adopts a repaired file.
        }
        let mut sync = None;
        if id == "subagent_settings_enabled" {
            next.enabled = !next.enabled;
        } else if let Some(key) = id.strip_prefix("subagent_setting:") {
            let (key, step) = key
                .split_once(':')
                .map_or((key, None), |(key, step)| (key, Some(step)));
            let Some(limit) = SubagentLimit::from_key(key) else {
                return true;
            };
            let field = format!("subagent_setting:{key}");
            let value = match action {
                UiAction::InputChanged { value, .. } | UiAction::Submitted { value, .. } => {
                    let value = value
                        .trim()
                        .parse::<u16>()
                        .ok()
                        .filter(|value| (1..=limit.max()).contains(value));
                    if matches!(action, UiAction::Submitted { .. }) {
                        sync = Some(field.clone());
                    }
                    let Some(value) = value else {
                        if sync.is_some() {
                            self.tree
                                .set_input_value(&field, limit.value(&next).to_string());
                        }
                        return true;
                    };
                    value
                }
                UiAction::Clicked { .. } => {
                    sync = Some(field.clone());
                    if step == Some("reset") {
                        limit.value(&SubagentSettings::default())
                    } else {
                        let Some(value) = openide_ui::NumberInput::adjusted_value(
                            id,
                            &field,
                            &limit.value(&next).to_string(),
                            Some(1.0),
                            Some(f64::from(limit.max())),
                            1.0,
                            0,
                        )
                        .and_then(|value| value.parse().ok()) else {
                            return true;
                        };
                        value
                    }
                }
                _ => return true,
            };
            if limit.set(&mut next, value).is_err() {
                return true;
            }
        } else {
            return true;
        }
        match next.save(&self.data_root) {
            Ok(()) => {
                self.subagent_settings = next;
                self.view.subagent_settings = next;
                self.set_settings_notice("Configuración guardada para los próximos turnos", false);
            }
            Err(error) => self.set_settings_notice(error, true),
        }
        if let Some(field) = sync {
            if let Some(limit) = field
                .strip_prefix("subagent_setting:")
                .and_then(SubagentLimit::from_key)
            {
                self.tree
                    .set_input_value(&field, limit.value(&self.subagent_settings).to_string());
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(app: &mut AgentsWindow, action: UiAction) {
        app.apply_action(action, &egui::Context::default());
    }
    fn click(app: &mut AgentsWindow, id: &str) {
        action(app, UiAction::Clicked { id: id.into() });
    }
    fn settings_app(root: &std::path::Path) -> AgentsWindow {
        let mut app = AgentsWindow::with_data_root(root.to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Subagents;
        app
    }

    #[test]
    fn subagent_settings_actions_persist_bounds_reset_and_reopen_without_changing_other_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut app = settings_app(root.path());
        let original = app.settings_preferences.clone();
        for limit in SubagentLimit::ALL {
            let field = format!("subagent_setting:{}", limit.key());
            action(
                &mut app,
                UiAction::InputChanged {
                    id: field.clone(),
                    value: limit.max().to_string(),
                },
            );
            click(&mut app, &format!("{field}:increment"));
            assert_eq!(
                limit.value(&SubagentSettings::load(root.path()).unwrap()),
                limit.max()
            );
            action(
                &mut app,
                UiAction::InputChanged {
                    id: field.clone(),
                    value: String::new(),
                },
            );
            assert_eq!(limit.value(&app.subagent_settings), limit.max());
            click(&mut app, &format!("{field}:decrement"));
            assert_eq!(limit.value(&app.subagent_settings), limit.max() - 1);
            action(
                &mut app,
                UiAction::Submitted {
                    id: field.clone(),
                    value: "0".into(),
                },
            );
            assert_eq!(limit.value(&app.subagent_settings), limit.max() - 1);
            click(&mut app, &format!("{field}:reset"));
            assert_eq!(
                limit.value(&app.subagent_settings),
                limit.value(&SubagentSettings::default())
            );
        }
        click(&mut app, "subagent_settings_enabled");
        assert!(!app.subagent_settings.enabled);
        assert_eq!(app.settings_preferences, original);
        let reopened = settings_app(root.path());
        assert_eq!(reopened.subagent_settings, app.subagent_settings);
        app.view.settings_page = SettingsPage::Appearance;
        click(&mut app, "subagent_settings_enabled");
        assert!(!SubagentSettings::load(root.path()).unwrap().enabled);
    }

    #[test]
    fn corrupt_subagent_settings_block_runtime_without_opening_settings_and_need_confirmed_backup()
    {
        let root = tempfile::tempdir().unwrap();
        let mut app = settings_app(root.path());
        let path = root.path().join("subagent-settings.json");
        let broken = b"{broken settings";
        std::fs::write(&path, broken).unwrap();
        assert!(app.subagent_settings_for_run().is_err());
        click(&mut app, "subagent_settings_enabled");
        assert!(app.subagent_settings_error.is_some());
        click(&mut app, "subagent_settings_reset_confirm");
        click(&mut app, "subagent_settings_reset");
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        click(&mut app, "subagent_settings_reset_cancel");
        click(&mut app, "subagent_settings_reset_confirm");
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        click(&mut app, "subagent_settings_reset");
        click(&mut app, "subagent_settings_reset_confirm");
        assert_eq!(
            app.subagent_settings_for_run().unwrap(),
            SubagentSettings::default()
        );
        let backups = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("subagent-settings-backup-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), broken);
        let mut external = SubagentSettings::default();
        external.enabled = false;
        external.max_depth = 1;
        external.save(root.path()).unwrap();
        assert_eq!(app.subagent_settings_for_run().unwrap(), external);
        click(&mut app, "subagent_settings_reload");
        assert_eq!(app.view.subagent_settings, external);
    }
}
