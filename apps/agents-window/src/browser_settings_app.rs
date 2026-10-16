//! Browser settings use the saved policy and update every live agent client.
use super::*;
use crate::browser_sites::{BrowserSites, SiteAccess};

impl AgentsWindow {
    pub(super) fn browser_automation_available(&self) -> bool {
        self.settings_preferences.browser_automation_enabled
            && self.view.browser_sites_error.is_none()
    }

    pub(super) fn sync_browser_agent_policy(&self) {
        for runtime in self.browsers.values() {
            runtime.set_agent_policy(
                self.settings_preferences.browser_automation_enabled,
                &self.browser_sites,
                self.view.browser_sites_error.as_deref(),
            );
        }
    }

    pub(super) fn browser_for_tools(
        &self,
        scope: DockScope,
        session: &str,
    ) -> Option<crate::browser_runtime::ConversationBrowser> {
        self.browser_automation_available()
            .then(|| self.conversation_browser(scope, session))
    }

    pub(super) fn apply_browser_settings_action(&mut self, action: &UiAction) -> bool {
        let id = match action {
            UiAction::Clicked { id }
                if id == "browser_automation" || id.starts_with("browser_site_") =>
            {
                id
            }
            UiAction::InputChanged { id, .. } | UiAction::Submitted { id, .. }
                if id == "browser_site_draft" =>
            {
                id
            }
            _ => return false,
        };
        if !self.view.shows(SettingsPage::Browser) {
            return true;
        }
        if let UiAction::InputChanged { value, .. } = action {
            if value.len() <= 4096 {
                self.view.browser_site_draft.clone_from(value);
            } else {
                self.tree.set_input_value(id, &self.view.browser_site_draft);
                self.set_settings_notice("La dirección del sitio es demasiado larga", true);
            }
            return true;
        }
        if id == "browser_automation" {
            let mut next = self.settings_preferences.clone();
            next.browser_automation_enabled = !next.browser_automation_enabled;
            let enabled = next.browser_automation_enabled;
            if self.commit_settings_preferences(next, "Preferencia de automatización guardada") {
                self.view.browser_automation_enabled = enabled;
                self.sync_browser_agent_policy();
            }
            return true;
        }
        match id.as_str() {
            "browser_site_reload" => {
                match BrowserSites::load(&self.data_root) {
                    Ok(sites) => {
                        self.view.browser_sites = sites.sites.clone();
                        self.browser_sites = sites;
                        self.view.browser_sites_error = None;
                        self.view.browser_sites_reset_confirm = false;
                        self.set_settings_notice("Permisos por sitio recargados", false);
                    }
                    Err(error) => {
                        self.view.browser_sites_error = Some(error.clone());
                        self.set_settings_notice(error, true);
                    }
                }
                self.sync_browser_agent_policy();
                return true;
            }
            "browser_site_reset" if self.view.browser_sites_error.is_some() => {
                self.view.browser_sites_reset_confirm = true;
                return true;
            }
            "browser_site_reset_cancel" => {
                self.view.browser_sites_reset_confirm = false;
                return true;
            }
            "browser_site_reset_confirm"
                if self.view.browser_sites_error.is_some()
                    && self.view.browser_sites_reset_confirm =>
            {
                match BrowserSites::reset_with_backup(&self.data_root) {
                    Ok((sites, backup)) => {
                        self.view.browser_sites = sites.sites.clone();
                        self.browser_sites = sites;
                        self.view.browser_sites_error = None;
                        self.view.browser_sites_reset_confirm = false;
                        self.set_settings_notice(
                            format!(
                                "Permisos restablecidos. Copia guardada: {}",
                                backup.display()
                            ),
                            false,
                        );
                    }
                    Err(error) => self.set_settings_notice(error, true),
                }
                self.sync_browser_agent_policy();
                return true;
            }
            _ => {}
        }
        if self.view.browser_sites_error.is_some() {
            return true;
        }
        if let Some(key) = id.strip_prefix("browser_site_draft_access:") {
            if let Some(access) = SiteAccess::from_key(key) {
                self.view.browser_site_draft_access = access;
                self.view.settings_choice_menu_open = None;
            }
            return true;
        }
        let changed = if id == "browser_site_add" || matches!(action, UiAction::Submitted { .. }) {
            let draft = self.view.browser_site_draft.clone();
            let added = self.set_browser_site(&draft, Some(self.view.browser_site_draft_access));
            if added.is_ok() {
                self.view.browser_site_draft.clear();
                self.tree.set_input_value("browser_site_draft", "");
            }
            Some(added)
        } else if let Some(rest) = id.strip_prefix("browser_site_set:") {
            rest.split_once(':').and_then(|(index, key)| {
                let access = SiteAccess::from_key(key)?;
                let site = self
                    .view
                    .browser_sites
                    .get(index.parse::<usize>().ok()?)?
                    .clone();
                Some(self.set_browser_site(&site.origin, Some(access)))
            })
        } else if let Some(index) = id.strip_prefix("browser_site_remove:") {
            index
                .parse::<usize>()
                .ok()
                .and_then(|index| self.view.browser_sites.get(index).cloned())
                .map(|site| self.set_browser_site(&site.origin, None))
        } else {
            None
        };
        if let Some(result) = changed {
            self.view.settings_choice_menu_open = None;
            match result {
                Ok(()) => self.set_settings_notice("Permisos por sitio guardados", false),
                Err(error) => self.set_settings_notice(error, true),
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn click(app: &mut AgentsWindow, id: &str) {
        app.apply_action(
            UiAction::Clicked { id: id.into() },
            &egui::Context::default(),
        );
    }

    #[test]
    fn browser_settings_persist_and_stale_actions_do_not_change_policy() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Browser;
        let appearance = app.settings_preferences.appearance.clone();
        assert!(app.browser_automation_available());
        click(&mut app, "browser_automation");
        assert!(!app.browser_automation_available());
        assert!(!SettingsPreferences::load(root.path()).browser_automation_enabled);
        app.view.browser_site_draft = "https://example.test/path".into();
        click(&mut app, "browser_site_draft_access:block");
        click(&mut app, "browser_site_add");
        assert_eq!(
            BrowserSites::load(root.path())
                .unwrap()
                .access("https://example.test"),
            SiteAccess::Block
        );
        click(&mut app, "browser_site_set:0:ask");
        assert_eq!(
            BrowserSites::load(root.path())
                .unwrap()
                .access("https://example.test"),
            SiteAccess::Ask
        );
        app.view.settings_page = SettingsPage::Appearance;
        click(&mut app, "browser_automation");
        click(&mut app, "browser_site_remove:0");
        assert!(!SettingsPreferences::load(root.path()).browser_automation_enabled);
        assert_eq!(BrowserSites::load(root.path()).unwrap().sites.len(), 1);
        assert_eq!(app.settings_preferences.appearance, appearance);
        let reopened = AgentsWindow::with_data_root(root.path().to_owned());
        assert!(!reopened.browser_automation_available());
        assert_eq!(reopened.view.browser_sites, app.view.browser_sites);
    }

    #[test]
    fn invalid_site_policy_requires_explicit_confirmed_recovery_and_preserves_a_backup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("browser-sites.json");
        let broken = b"{not valid permissions";
        std::fs::write(&path, broken).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Browser;
        assert!(app.view.browser_sites_error.is_some());
        assert!(!app.browser_automation_available());
        assert!(
            app.set_browser_site("https://example.test", Some(SiteAccess::Allow))
                .is_err()
        );
        click(&mut app, "browser_site_reset_confirm");
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        click(&mut app, "browser_site_reset");
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        click(&mut app, "browser_site_reset_cancel");
        click(&mut app, "browser_site_reset_confirm");
        assert_eq!(std::fs::read(&path).unwrap(), broken);
        click(&mut app, "browser_site_reset");
        click(&mut app, "browser_site_reset_confirm");
        assert!(app.view.browser_sites_error.is_none());
        assert!(app.browser_automation_available());
        assert!(BrowserSites::load(root.path()).unwrap().sites.is_empty());
        let backups = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("browser-sites-backup-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), broken);
    }

    #[tokio::test]
    async fn disabling_automation_removes_the_hosts_catalog_and_revokes_a_client_already_delivered()
    {
        use crate::browser_runtime::{self, BrowserCommand, BrowserScope, BrowserSnapshot};
        use openide_harness::{LocalTools, McpToolClient, ToolPreparation};
        use tokio_util::sync::CancellationToken;
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Browser;
        let scope = DockScope::for_owner(root.path(), "policy");
        let catalog = |browser| {
            crate::environment_runtime::EnvironmentToolPreparation::new(
                root.path().to_owned(),
                "policy".into(),
                Arc::new(Vec::new()),
                Vec::new(),
                browser,
            )
        };
        let active = catalog(app.browser_for_tools(scope.clone(), "policy"))
            .prepare(
                root.path(),
                Arc::new(LocalTools::new()),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            active
                .tools()
                .definitions()
                .iter()
                .any(|tool| tool.name.starts_with(browser_runtime::NATIVE_TOOL_PREFIX))
        );
        let (runtime, commands) = browser_runtime::tests::fixture(
            BrowserScope {
                workspace: root.path().to_owned(),
                session_id: "policy".into(),
            },
            BrowserSnapshot {
                started: true,
                ..Default::default()
            },
        );
        let client = runtime.mcp_client();
        app.browsers.insert(900, runtime);
        click(&mut app, "browser_automation");
        let off = catalog(app.browser_for_tools(scope.clone(), "policy"))
            .prepare(
                root.path(),
                Arc::new(LocalTools::new()),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            !off.tools()
                .definitions()
                .iter()
                .any(|tool| tool.name.starts_with(browser_runtime::NATIVE_TOOL_PREFIX))
        );
        assert!(
            client
                .list_tools(&CancellationToken::new())
                .await
                .unwrap()
                .is_empty()
        );
        let reply = client
            .call_tool("browser_snapshot", json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(reply.is_error);
        assert!(
            commands.finish_next().is_none(),
            "a revoked client must not enqueue a page read"
        );
        let manual = app.browsers[&900]
            .submit_ui(BrowserCommand::TabList {})
            .unwrap();
        assert!(matches!(
            commands.finish_next(),
            Some(BrowserCommand::TabList {})
        ));
        drop(manual);
        app.view.browser_sites_error = Some("invalid policy".into());
        app.settings_preferences.browser_automation_enabled = true;
        app.sync_browser_agent_policy();
        assert!(app.browser_for_tools(scope, "policy").is_none());
        for name in browser_runtime::native_tool_names() {
            let call = ToolCall {
                id: "denied".into(),
                name,
                arguments: json!({}),
            };
            assert_eq!(
                app.native_permission_policy()
                    .unwrap()
                    .mode_for_call_with_session_grants(
                        "policy",
                        root.path(),
                        root.path(),
                        "",
                        &call,
                        &[]
                    ),
                PermissionMode::Deny
            );
        }
        let reply = client
            .call_tool("browser_snapshot", json!({}), &CancellationToken::new())
            .await
            .unwrap();
        assert!(reply.is_error);
        assert!(commands.finish_next().is_none());
    }
}
