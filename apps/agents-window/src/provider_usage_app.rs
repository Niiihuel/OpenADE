//! Account usage stays separate from model prices and conversation context.
//! Only sanitized snapshots enter the retained UI tree. Credential revisions
//! remain in the host so late results cannot attach to a replacement account.

use super::*;
use crate::claude_limits;
use crate::provider_usage::{self, ProviderUsageStatus, UsageWorker};

pub(super) type UsageIdentities = BTreeMap<
    String,
    (
        Option<[u8; 32]>,
        crate::provider_connections::ConnectionSummary,
    ),
>;

const REFRESH_INTERVAL: Duration = provider_usage::REFRESH_INTERVAL;
const BACKGROUND_INTERVAL: Duration = Duration::from_secs(300);

impl AgentsWindow {
    pub(super) fn sync_provider_usage(&mut self) -> bool {
        self.provider_usage_last_identity_check = Instant::now();
        let summaries = crate::provider_connections::summaries(&self.data_root);
        let identities: UsageIdentities = self
            .view
            .native_provider_definitions
            .keys()
            .filter(|id| crate::provider_connections::supported(id))
            .map(|id| {
                (
                    id.clone(),
                    (
                        provider_usage::credential_revision(&self.data_root, id),
                        summaries.get(id).cloned().unwrap_or_default(),
                    ),
                )
            })
            .collect();
        if identities == self.provider_usage_identities {
            return false;
        }
        // Dropping the old receiver also prevents queued results from a previous
        // login being applied, even when both accounts share the same provider.
        self.provider_usage_worker = None;
        self.view.provider_usage_refreshing = false;
        self.view.provider_connections = summaries;
        let mut initial = provider_usage::initial_snapshots(
            &self.data_root,
            &self.view.native_provider_definitions,
        );
        for (id, identity) in &identities {
            if self.provider_usage_identities.get(id) == Some(identity) {
                if let Some(previous) = self.view.provider_usage.get(id) {
                    initial.insert(id.clone(), previous.clone());
                }
            }
        }
        // Claude Code's limits follow its status line, not a connection.
        if let Some(claude) = self.view.provider_usage.remove(claude_limits::PROVIDER_ID) {
            initial.insert(claude_limits::PROVIDER_ID.into(), claude);
        }
        self.view.provider_usage = initial;
        self.provider_usage_identities = identities;
        self.provider_usage_last_refresh = Instant::now() - BACKGROUND_INTERVAL;
        true
    }

    /// Whether Claude Code's status line is ours, as its settings say now.
    fn sync_claude_limits(&mut self) {
        self.view.claude_limits = match &self.claude_config_dir {
            Some(claude) => claude_limits::status(claude, &self.data_root),
            None => claude_limits::Status::Off,
        };
        if self.view.claude_limits != claude_limits::Status::On {
            self.view.provider_usage.remove(claude_limits::PROVIDER_ID);
        }
    }

    /// The user's own choice in Settings; nothing else installs the script.
    pub(super) fn set_claude_limits(&mut self, enabled: bool) {
        let Some(claude) = self.claude_config_dir.clone() else {
            return;
        };
        let result = if enabled {
            claude_limits::enable(&claude, &self.data_root, provider_usage::now_ms())
        } else {
            claude_limits::disable(&claude, &self.data_root)
        };
        match result {
            Ok(()) if enabled => self.set_settings_notice("Lectura de límites activada", false),
            Ok(()) => self.set_settings_notice("Lectura de límites desactivada", false),
            Err(error) => self.set_settings_notice(error, true),
        }
        self.begin_provider_usage_refresh();
    }

    pub(super) fn begin_provider_usage_refresh(&mut self) {
        if self.sync_provider_usage() {
            self.refresh_composer_routes();
        }
        self.sync_claude_limits();
        if self.provider_usage_worker.is_some() {
            return;
        }
        let mut ids: Vec<String> = self
            .view
            .provider_usage
            .iter()
            .filter(|(id, snapshot)| {
                *id != claude_limits::PROVIDER_ID
                    && !matches!(
                        snapshot.status,
                        ProviderUsageStatus::Disconnected | ProviderUsageStatus::NeedsSignIn
                    )
            })
            .map(|(id, _)| id.clone())
            .collect();
        if self.view.claude_limits == claude_limits::Status::On {
            ids.push(claude_limits::PROVIDER_ID.into());
        }
        self.provider_usage_last_refresh = Instant::now();
        if ids.is_empty() {
            return;
        }
        self.view.provider_usage_refreshing = true;
        self.provider_usage_worker = Some(UsageWorker::spawn(
            self.data_root.clone(),
            ids,
            self.claude_config_dir.clone(),
        ));
    }

    pub(super) fn retry_provider_usage_refresh(&mut self) {
        // A previous HTTP 401 does not mutate the connection record. A manual
        // retry may recheck a still configured key, while an expired local
        // OAuth session continues to require the existing sign-in flow.
        let fresh = provider_usage::initial_snapshots(
            &self.data_root,
            &self.view.native_provider_definitions,
        );
        for (id, initial) in fresh {
            if self
                .view
                .provider_usage
                .get(&id)
                .is_some_and(|snapshot| snapshot.status == ProviderUsageStatus::NeedsSignIn)
                && initial.status != ProviderUsageStatus::NeedsSignIn
            {
                self.view.provider_usage.insert(id, initial);
            }
        }
        self.begin_provider_usage_refresh();
    }

    fn apply_provider_usage_result(
        &mut self,
        result: Result<BTreeMap<String, provider_usage::ProviderUsageSnapshot>, String>,
    ) {
        match result {
            Ok(mut snapshots) => {
                // Absent means its windows have reset or its state is gone.
                let claude = snapshots
                    .remove(claude_limits::PROVIDER_ID)
                    .filter(|_| self.view.claude_limits == claude_limits::Status::On);
                self.view.provider_usage.remove(claude_limits::PROVIDER_ID);
                self.view.provider_usage.extend(
                    claude.map(|snapshot| (claude_limits::PROVIDER_ID.to_owned(), snapshot)),
                );
                for (id, snapshot) in snapshots {
                    if !self.provider_usage_identities.contains_key(&id)
                        || snapshot.provider_id != id
                    {
                        continue;
                    }
                    let incoming = if let Some(previous) = self.view.provider_usage.get(&id) {
                        provider_usage::retain_last_success(previous, snapshot)
                    } else {
                        snapshot
                    };
                    self.view.provider_usage.insert(id, incoming);
                }
            }
            Err(message) => {
                for snapshot in self.view.provider_usage.values_mut() {
                    if matches!(
                        snapshot.status,
                        ProviderUsageStatus::Loading
                            | ProviderUsageStatus::Ready
                            | ProviderUsageStatus::Error
                    ) {
                        snapshot.status = ProviderUsageStatus::Error;
                        snapshot.message = Some(message.clone());
                    }
                }
            }
        }
    }

    fn poll_provider_usage_result(&mut self) -> bool {
        let result = self.provider_usage_worker.as_ref().and_then(|worker| {
            match worker.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err(
                    "La actualización del uso se interrumpió. Inténtalo de nuevo.".into(),
                )),
                Err(TryRecvError::Empty) => None,
            }
        });
        if let Some(result) = result {
            // Recheck current credentials before publishing any account data.
            // A changed identity clears its old measurements and starts a new job.
            if !self.sync_provider_usage() {
                self.apply_provider_usage_result(result);
            } else {
                self.refresh_composer_routes();
            }
            self.provider_usage_worker = None;
            self.view.provider_usage_refreshing = false;
            return true;
        }
        false
    }

    pub(super) fn poll_provider_usage(&mut self) -> bool {
        let mut changed = false;
        if self.provider_usage_last_identity_check.elapsed() >= REFRESH_INTERVAL
            && self.sync_provider_usage()
        {
            self.refresh_composer_routes();
            changed = true;
        }
        changed |= self.poll_provider_usage_result();
        let visible = !self.view.sidebar_collapsed
            || self.view.provider_usage_open
            || (self.view.show_settings && self.view.settings_page == SettingsPage::Usage);
        let interval = if visible {
            REFRESH_INTERVAL
        } else {
            BACKGROUND_INTERVAL
        };
        if self.provider_usage_worker.is_none()
            && self.provider_usage_last_refresh.elapsed() >= interval
        {
            self.begin_provider_usage_refresh();
            changed |= self.view.provider_usage_refreshing;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(root: &Path, key: &str, disabled: bool) {
        credential_for(root, "openrouter", key, disabled, "api");
    }

    fn credential_for(root: &Path, id: &str, key: &str, disabled: bool, service: &str) {
        let directory = root.join("provider-credentials");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{id}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "provider_id": id, "access_token": key,
                "disabled": disabled, "service": service,
            }))
            .unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn claude_limits_are_read_only_after_the_user_enables_them_and_leave_when_disabled() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let ctx = egui::Context::default();
        assert_eq!(app.claude_config_dir, None, "tests never read the real Claude home");
        let claude = root.path().join("claude");
        std::fs::create_dir_all(&claude).unwrap();
        let settings = claude.join("settings.json");
        std::fs::write(&settings, "{\n  \"model\": \"opus\"\n}\n").unwrap();
        app.claude_config_dir = Some(claude);
        std::fs::create_dir_all(&app.data_root).unwrap();
        let state = app.data_root.join(claude_limits::STATE_FILE);
        let payload = r#"{"rate_limits":{"seven_day":{"used_percentage":64}}}"#;
        std::fs::write(&state, payload).unwrap();

        // A state file alone starts nothing: the status line is not ours yet.
        app.begin_provider_usage_refresh();
        assert_eq!(app.view.claude_limits, claude_limits::Status::Off);
        assert!(app.provider_usage_worker.is_none());

        let click = |app: &mut AgentsWindow, id: &str| {
            app.apply_action(UiAction::Clicked { id: id.into() }, &ctx)
        };
        click(&mut app, "claude_limits_enable");
        assert_eq!(app.view.claude_limits, claude_limits::Status::On);
        assert_eq!(app.view.settings_notice.as_deref(), Some("Lectura de límites activada"));
        assert!(std::fs::read_to_string(&settings).unwrap().contains("\"statusLine\""));
        std::fs::write(&state, payload).unwrap();
        let started = Instant::now();
        while !app.poll_provider_usage_result() {
            assert!(started.elapsed() < Duration::from_secs(20), "usage worker did not answer");
            std::thread::sleep(Duration::from_millis(10));
        }
        let usage = &app.view.provider_usage[claude_limits::PROVIDER_ID];
        assert!(usage.local);
        assert_eq!((usage.windows[0].label.as_str(), usage.windows[0].used), ("Semanal", 64.0));
        // A change of provider accounts keeps Claude Code's reading.
        app.view.native_provider_definitions.clear();
        assert!(app.sync_provider_usage());
        assert_eq!(app.view.provider_usage.len(), 1);

        click(&mut app, "claude_limits_disable");
        assert_eq!(app.view.claude_limits, claude_limits::Status::Off);
        assert!(app.view.provider_usage.is_empty());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{\n  \"model\": \"opus\"\n}\n");
        assert!(!state.exists());

        // Someone else's status line is reported and left alone.
        let foreign = "{\"statusLine\":{\"type\":\"command\",\"command\":\"mine\"}}";
        std::fs::write(&settings, foreign).unwrap();
        click(&mut app, "claude_limits_enable");
        assert_eq!(app.view.claude_limits, claude_limits::Status::Foreign);
        assert_eq!(app.view.settings_notice.as_deref(), Some(claude_limits::FOREIGN));
        assert!(app.view.settings_notice_error);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), foreign);
    }

    #[test]
    fn usage_actions_preserve_route_and_prompt_and_open_native_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let ctx = egui::Context::default();
        app.view.native_provider_definitions.clear();
        app.sync_provider_usage();
        let route = app.view.native_provider_id.clone();
        let model = app.view.model.clone();
        app.view.prompt = "Borrador sin enviar".into();
        app.apply_action(
            UiAction::Clicked {
                id: "usage_toggle".into(),
            },
            &ctx,
        );
        assert!(app.view.provider_usage_open);
        app.apply_action(
            UiAction::Clicked {
                id: "usage_dismiss".into(),
            },
            &ctx,
        );
        assert!(!app.view.provider_usage_open);
        app.apply_action(
            UiAction::Clicked {
                id: "usage_details".into(),
            },
            &ctx,
        );
        assert!(app.view.show_settings);
        assert_eq!(app.view.settings_page, SettingsPage::Usage);
        assert_eq!(app.view.native_provider_id, route);
        assert_eq!(app.view.model, model);
        assert_eq!(app.view.prompt, "Borrador sin enviar");
        assert!(!app.view.running);
    }

    #[test]
    fn account_action_emits_only_the_selected_account_url_without_a_network_worker() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        let ctx = egui::Context::default();
        let expected = "https://example.com/synthetic-account";
        let account = app.view.provider_usage.get_mut("codex").unwrap();
        account.account_url = Some(expected.into());
        app.view.prompt = "Borrador sin enviar".into();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.apply_action(UiAction::Clicked { id: "provider_usage_account:codex".into() }, ui.ctx());
        });
        output.textures_delta.clear();
        let urls: Vec<_> = output.platform_output.commands.iter().filter_map(|command| {
            if let egui::OutputCommand::OpenUrl(url) = command { Some(url.url.as_str()) } else { None }
        }).collect();
        assert_eq!(urls, vec![expected]);
        assert_eq!(app.view.prompt, "Borrador sin enviar");
        assert!(app.provider_usage_worker.is_none() && app.provider_connection_worker.is_none());
        app.view.provider_usage.get_mut("codex").unwrap().account_url = None;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for id in ["provider_usage_account:codex", "provider_usage_account:unknown"] {
                app.apply_action(UiAction::Clicked { id: id.into() }, ui.ctx());
            }
        });
        output.textures_delta.clear();
        assert!(!output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::OpenUrl(_))));
    }

    #[test]
    fn quota_view_only_tracks_catalog_accounts_and_no_credentials_starts_no_http() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        assert!(app.provider_usage_worker.is_none());
        assert!(app
            .view
            .provider_usage
            .keys()
            .all(|id| crate::provider_connections::supported(id)
                && app.view.native_provider_definitions.contains_key(id)));
        app.view.native_provider_definitions.clear();
        assert!(app.sync_provider_usage());
        assert!(app.view.provider_usage.is_empty());
        app.begin_provider_usage_refresh();
        assert!(app.provider_usage_worker.is_none());
        assert!(!app.view.provider_usage_refreshing);
    }

    #[test]
    fn external_key_replacement_discards_queued_usage_and_disconnect_clears_account() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        credential(&app.data_root, "placeholder-first-account", false);
        assert!(app.sync_provider_usage());
        assert!(app.view.provider_connections["openrouter"].configured);
        assert_eq!(
            app.view.provider_usage["openrouter"].status,
            ProviderUsageStatus::Loading
        );
        let mut measurement = app.view.provider_usage["openrouter"].clone();
        measurement.status = ProviderUsageStatus::Ready;
        measurement.fetched_at_ms = Some(42);
        measurement.windows.push(provider_usage::UsageWindow {
            label: "Consumo de primera cuenta".into(),
            used: 15.0,
            limit: Some(100.0),
            remaining: Some(85.0),
            unit: provider_usage::UsageUnit::Usd,
            reset_at_ms: None,
            reset_label: None,
        });
        app.view
            .provider_usage
            .insert("openrouter".into(), measurement.clone());
        let (sender, receiver) = mpsc::channel();
        app.provider_usage_worker = Some(UsageWorker::from_receiver(receiver));
        app.view.provider_usage_refreshing = true;
        sender
            .send(Ok(BTreeMap::from([("openrouter".into(), measurement)])))
            .unwrap();
        credential(&app.data_root, "placeholder-second-account", false);
        assert!(app.poll_provider_usage_result());
        assert!(app.view.provider_usage["openrouter"].windows.is_empty());
        assert_eq!(app.view.provider_usage["openrouter"].fetched_at_ms, None);
        // Drop the scheduled new-account request before it can be published.
        app.provider_usage_worker = None;
        credential(&app.data_root, "", true);
        assert!(app.sync_provider_usage());
        assert!(!app.view.provider_connections["openrouter"].configured);
        assert_eq!(
            app.view.provider_usage["openrouter"].status,
            ProviderUsageStatus::Disconnected
        );
        assert!(app.view.provider_usage["openrouter"].windows.is_empty());
        assert!(app.provider_usage_worker.is_none());
    }

    #[test]
    fn service_change_reconciles_usage_and_catalog_from_the_same_connection() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        credential_for(&app.data_root, "opencode", "placeholder-key", false, "zen");
        app.refresh_composer_routes();
        assert_eq!(
            app.view.provider_usage["opencode"].status,
            ProviderUsageStatus::External
        );
        let (sender, receiver) = mpsc::channel();
        app.provider_usage_worker = Some(UsageWorker::from_receiver(receiver));
        sender.send(Ok(BTreeMap::new())).unwrap();
        credential_for(&app.data_root, "opencode", "placeholder-key", false, "go");
        assert!(app.poll_provider_usage_result());
        assert_eq!(app.view.provider_connections["opencode"].service, "go");
        assert_eq!(
            app.view.native_provider_definitions["opencode"].endpoint,
            "https://opencode.ai/zen/go/v1"
        );
        assert_eq!(
            app.view.provider_usage["opencode"].plan_label.as_deref(),
            Some("OpenCode Go")
        );
        assert_eq!(
            app.view.provider_usage["opencode"].status,
            ProviderUsageStatus::Loading
        );
        assert!(app.provider_usage_worker.is_none());
    }
}
