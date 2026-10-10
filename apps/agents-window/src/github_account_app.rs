//! Account choice belongs to OpenIDE. Credential helpers remain untouched.

use super::*;
use crate::settings_preferences::GithubAccountPreference;

impl AgentsWindow {
    pub(super) fn github_query_account(&self) -> Option<&str> {
        if matches!(
            self.settings_preferences.github_account,
            GithubAccountPreference::Disconnected
        ) {
            return None;
        }
        match &self.view.github_account {
            GithubAccountStatus::Connected(identity) => Some(&identity.login),
            _ => None,
        }
    }

    pub(super) fn ensure_github_account_worker(&mut self) {
        if self.github_account_worker.is_none() {
            self.github_account_worker =
                GithubAccountWorker::spawn(GithubAccountPreference::Disconnected).ok();
        }
    }

    fn persist_github_account_choice(
        &mut self,
        choice: GithubAccountPreference,
    ) -> Result<(), String> {
        let mut preferences = self.settings_preferences.clone();
        preferences.github_account = choice;
        preferences.save(&self.data_root)?;
        self.settings_preferences = preferences;
        Ok(())
    }

    fn invalidate_github_identity_queries(&mut self) {
        // A response produced with an old identity must not survive a change,
        // including when the repository tab is currently hidden.
        self.review_generation = self.review_generation.wrapping_add(1);
        self.github_in_flight = false;
        self.github_status = GithubStatus::NoRemote;
        if let Some(Ok(review)) = &mut self.view.git_review {
            Arc::make_mut(review).github = self.github_status.clone();
        }
        self.invalidate_github_contributions();
        if let Some(workspace) = self.review_active_workspace.clone() {
            self.request_git_review(workspace);
        }
    }

    pub(super) fn apply_github_account_event(&mut self, event: GithubAccountEvent) {
        match event {
            GithubAccountEvent::DeviceCode(code) => {
                if self.view.github_sign_in.is_some() {
                    self.view.github_sign_in = Some(GithubSignInView::Code {
                        code,
                        continued: false,
                    });
                    self.github_sign_in_focus_pending = Some("github_sign_in_continue");
                }
            }
            GithubAccountEvent::BrowserSignIn => {
                if self.view.github_sign_in.is_some() {
                    self.view.github_sign_in = Some(GithubSignInView::Browser);
                    self.github_sign_in_focus_pending = Some("github_sign_in_cancel");
                }
            }
            GithubAccountEvent::Status(status) => {
                self.view.github_account_refreshing = false;
                let previous = self.github_account_previous.take();
                if let GithubAccountStatus::Unverified { accounts, notice } = &status {
                    if self.view.github_sign_in.is_some() && !accounts.is_empty() {
                        // Account discovery is an intermediate authentication
                        // step, not a failed switch. Keep its choices separate
                        // from the identity used by repository/API queries.
                        let previous = previous.unwrap_or_else(|| self.view.github_account.clone());
                        self.view.github_account = match &previous {
                            GithubAccountStatus::Checking | GithubAccountStatus::Verifying(_) => {
                                GithubAccountStatus::Disconnected
                            }
                            previous => previous.clone(),
                        };
                        self.github_account_previous = Some(previous);
                        self.view.github_account_notice = None;
                        self.view.github_sign_in = Some(GithubSignInView::ChooseAccount {
                            accounts: accounts.clone(),
                            notice: notice.clone(),
                        });
                        self.github_sign_in_focus_pending = Some("github_sign_in_cancel");
                        return;
                    }
                }
                if let GithubAccountStatus::Connected(identity) = &status {
                    let saved = GithubAccountPreference::selected(&identity.login)
                        .and_then(|choice| self.persist_github_account_choice(choice));
                    if let Err(error) = saved {
                        self.view.github_account =
                            previous.unwrap_or(GithubAccountStatus::Disconnected);
                        self.view.github_account_notice = Some(error.clone());
                        if self.view.github_sign_in.is_some() {
                            self.view.github_sign_in = Some(GithubSignInView::Error(error));
                            self.github_sign_in_focus_pending = Some("github_sign_in_retry");
                        }
                        return;
                    }
                    self.view.github_account = status;
                    self.view.github_account_notice = None;
                    self.view.github_sign_in = None;
                    self.github_sign_in_focus_pending = None;
                    self.view.github_account_menu_open = false;
                    self.invalidate_github_identity_queries();
                    return;
                }

                let notice = match &status {
                    GithubAccountStatus::Unavailable(reason) => Some(reason.clone()),
                    GithubAccountStatus::Unverified { notice, .. } => notice.clone(),
                    _ => None,
                };
                let retained =
                    previous.filter(|status| matches!(status, GithubAccountStatus::Connected(_)));
                let notice = if retained.is_some() && notice.is_none() {
                    Some(
                        "No se pudo confirmar otra cuenta. Tu cuenta anterior sigue conectada."
                            .to_owned(),
                    )
                } else {
                    notice
                };
                self.view.github_account = retained.unwrap_or(status);
                self.view.github_account_notice = notice.clone();
                if self.view.github_sign_in.is_some() {
                    self.view.github_sign_in = notice.map(GithubSignInView::Error);
                    self.github_sign_in_focus_pending = self
                        .view
                        .github_sign_in
                        .as_ref()
                        .map(|_| "github_sign_in_retry");
                }
            }
        }
    }

    pub(super) fn refresh_github_account(&mut self) {
        if self.view.github_account_refreshing || self.view.github_sign_in.is_some() {
            return;
        }
        self.view.github_account_menu_open = false;
        self.github_account_previous = Some(self.view.github_account.clone());
        self.view.github_account_notice = None;
        self.view.github_account_refreshing = true;
        self.ensure_github_account_worker();
        if !self
            .github_account_worker
            .as_ref()
            .is_some_and(|worker| worker.refresh(self.settings_preferences.github_account.clone()))
        {
            self.apply_github_account_event(GithubAccountEvent::Status(
                GithubAccountStatus::Unavailable(
                    "No se pudo iniciar la actualización de GitHub.".into(),
                ),
            ));
        }
    }

    pub(super) fn cancel_github_sign_in(&mut self) {
        if let Some(worker) = &self.github_account_worker {
            worker.cancel();
        }
        if let Some(previous) = self.github_account_previous.take() {
            self.view.github_account = match previous {
                GithubAccountStatus::Checking | GithubAccountStatus::Verifying(_) => {
                    GithubAccountStatus::Disconnected
                }
                previous => previous,
            };
        }
        self.view.github_sign_in = None;
        self.view.github_account_refreshing = false;
        self.github_sign_in_focus_pending = None;
    }

    pub(super) fn disconnect_github_account(&mut self) {
        // Persist before changing any state: a failed write must not claim a
        // disconnection that would be undone on the next launch.
        if let Err(error) =
            self.persist_github_account_choice(GithubAccountPreference::Disconnected)
        {
            self.view.github_account_notice = Some(error);
            return;
        }
        if let Some(worker) = &self.github_account_worker {
            worker.cancel();
        }
        self.github_account_previous = None;
        self.view.github_account = GithubAccountStatus::Disconnected;
        self.view.github_account_refreshing = false;
        self.view.github_account_notice = None;
        self.view.github_account_menu_open = false;
        self.view.github_sign_in = None;
        self.github_sign_in_focus_pending = None;
        self.invalidate_github_identity_queries();
    }

    pub(super) fn manage_github_account(&mut self) {
        self.close_composer_floating_controls();
        self.view.github_account_menu_open = false;
        self.view.provider_usage_open = false;
        self.view.show_settings = true;
        self.view.settings_page = SettingsPage::Github;
        self.clear_settings_notice();
        self.visit_navigation();
    }

    pub(super) fn verify_github_account(&mut self, account: &str) {
        let chooser = matches!(
            self.view.github_sign_in,
            Some(GithubSignInView::ChooseAccount { .. })
        );
        if self.view.github_account_refreshing || (self.view.github_sign_in.is_some() && !chooser) {
            return;
        }
        let listed = if let Some(GithubSignInView::ChooseAccount { accounts, .. }) =
            &self.view.github_sign_in
        {
            accounts
                .iter()
                .any(|known| known.eq_ignore_ascii_case(account))
        } else {
            matches!(
                &self.view.github_account,
                GithubAccountStatus::Unverified { accounts, .. }
                    if accounts.iter().any(|known| known.eq_ignore_ascii_case(account))
            )
        };
        if !listed {
            return;
        }
        if self.github_account_previous.is_none() {
            self.github_account_previous = Some(self.view.github_account.clone());
        }
        self.view.github_account_menu_open = false;
        self.view.github_account_notice = None;
        self.view.github_account_refreshing = true;
        if !matches!(self.view.github_account, GithubAccountStatus::Connected(_)) {
            self.view.github_account = GithubAccountStatus::Verifying(account.to_owned());
        }
        if chooser {
            self.view.github_sign_in = Some(GithubSignInView::Preparing);
            self.github_sign_in_focus_pending = Some("github_sign_in_cancel");
        }
        self.ensure_github_account_worker();
        if !self
            .github_account_worker
            .as_ref()
            .is_some_and(|worker| worker.verify(account))
        {
            self.apply_github_account_event(GithubAccountEvent::Status(
                GithubAccountStatus::Unavailable(
                    "No se pudo iniciar la verificación de GitHub.".into(),
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_account::{GithubAccountTestDriver, GithubIdentity};

    fn connected(login: &str) -> GithubAccountStatus {
        GithubAccountStatus::Connected(GithubIdentity {
            login: login.into(),
            avatar: None,
        })
    }

    fn host(root: &Path) -> (AgentsWindow, GithubAccountTestDriver) {
        let mut app = AgentsWindow::with_data_root(root.to_owned());
        let (worker, driver) = GithubAccountWorker::test_fixture();
        app.github_account_worker = Some(worker);
        // Repository and contribution queries are separately covered by their
        // scoped workers. These host cases exercise account transitions only.
        app.github_worker = None;
        app.github_contributions_worker = None;
        app.review_active_workspace = None;
        app.view.github_account_refreshing = false;
        (app, driver)
    }

    fn respond(
        app: &mut AgentsWindow,
        driver: &GithubAccountTestDriver,
        revision: u64,
        status: GithubAccountStatus,
    ) {
        driver.respond(revision, GithubAccountEvent::Status(status));
        assert!(app.update_github_account(&egui::Context::default()));
    }

    #[test]
    fn verified_choice_and_local_disconnect_survive_restart_without_shared_logout() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let (mut app, driver) = host(&data);
        app.start_github_sign_in();
        let (revision, kind) = driver.request();
        assert_eq!(kind, "connect");
        respond(&mut app, &driver, revision, connected("alice"));
        assert_eq!(app.github_query_account(), Some("alice"));
        assert!(app.view.github_sign_in.is_none());
        assert_eq!(
            SettingsPreferences::load(&data).github_account,
            GithubAccountPreference::Selected("alice".into())
        );
        let restored = AgentsWindow::with_data_root(data.clone());
        assert_eq!(
            restored.view.github_account,
            GithubAccountStatus::Verifying("alice".into())
        );
        drop(restored);

        app.view.github_contributions = GithubContributionsStatus::Ready(
            crate::github_contributions::GithubContributionCalendar {
                login: "alice".into(),
                total: 15,
                weeks: Vec::new(),
            },
        );
        let old_review = app.review_generation;
        let old_contributions = app.github_contributions_generation;
        app.start_github_sign_in();
        let (pending, _) = driver.request();
        app.disconnect_github_account();
        assert_eq!(app.view.github_account, GithubAccountStatus::Disconnected);
        assert_eq!(app.github_query_account(), None);
        assert!(app.review_generation > old_review);
        assert!(app.github_contributions_generation > old_contributions);
        assert_eq!(
            app.view.github_contributions,
            GithubContributionsStatus::Idle
        );
        driver.respond(pending, GithubAccountEvent::Status(connected("bob")));
        assert!(!app.update_github_account(&egui::Context::default()));
        assert_eq!(app.view.github_account, GithubAccountStatus::Disconnected);
        let restored = AgentsWindow::with_data_root(data.clone());
        assert_eq!(
            restored.view.github_account,
            GithubAccountStatus::Disconnected
        );
        assert!(!restored.view.github_account_refreshing);
        assert_eq!(
            SettingsPreferences::load(&data).github_account,
            GithubAccountPreference::Disconnected
        );
        // Refreshing a disconnected profile cannot imply reconnecting it.
        app.refresh_github_account();
        let (revision, kind) = driver.request();
        assert_eq!(kind, "refresh_disconnected");
        respond(
            &mut app,
            &driver,
            revision,
            GithubAccountStatus::Disconnected,
        );
        assert_eq!(app.github_query_account(), None);
    }

    #[test]
    fn failed_refresh_or_cancelled_reconnection_keeps_the_previous_identity() {
        let root = tempfile::tempdir().unwrap();
        let (mut app, driver) = host(root.path());
        app.apply_github_account_event(GithubAccountEvent::Status(connected("alice")));
        let saved = app.settings_preferences.clone();
        app.refresh_github_account();
        let (revision, kind) = driver.request();
        assert_eq!(kind, "refresh");
        assert_eq!(app.view.github_account, connected("alice"));
        assert!(app.view.github_account_refreshing);
        respond(
            &mut app,
            &driver,
            revision,
            GithubAccountStatus::Unavailable("Sin conexión".into()),
        );
        assert_eq!(app.view.github_account, connected("alice"));
        assert_eq!(
            app.view.github_account_notice.as_deref(),
            Some("Sin conexión")
        );
        assert_eq!(app.settings_preferences, saved);

        app.start_github_sign_in();
        let (revision, _) = driver.request();
        app.cancel_github_sign_in();
        driver.respond(revision, GithubAccountEvent::DeviceCode("ABCD-1234".into()));
        driver.respond(revision, GithubAccountEvent::Status(connected("bob")));
        assert!(!app.update_github_account(&egui::Context::default()));
        assert_eq!(app.view.github_account, connected("alice"));
        assert!(app.view.github_sign_in.is_none());
        assert!(!app.view.github_account_refreshing);
        assert_eq!(SettingsPreferences::load(root.path()), saved);
    }

    #[test]
    fn multiple_helper_accounts_remain_selectable_without_changing_the_active_identity() {
        for outcome in ["cancel", "failure", "success"] {
            let root = tempfile::tempdir().unwrap();
            let (mut app, driver) = host(root.path());
            app.apply_github_account_event(GithubAccountEvent::Status(connected("alice")));
            let saved = app.settings_preferences.clone();
            let review_revision = app.review_generation;
            app.start_github_sign_in();
            let (revision, _) = driver.request();
            respond(
                &mut app,
                &driver,
                revision,
                GithubAccountStatus::Unverified {
                    accounts: vec!["alice".into(), "bob".into()],
                    notice: Some("Elige la cuenta que quieres verificar.".into()),
                },
            );
            assert_eq!(app.github_query_account(), Some("alice"));
            assert_eq!(app.view.github_account, connected("alice"));
            assert!(!app.view.github_account_refreshing);
            assert!(matches!(&app.view.github_sign_in,
                Some(GithubSignInView::ChooseAccount { accounts, .. }) if accounts == &["alice", "bob"]));
            app.verify_github_account("not-listed");
            assert!(!app.view.github_account_refreshing);
            assert!(matches!(
                app.view.github_sign_in,
                Some(GithubSignInView::ChooseAccount { .. })
            ));
            assert_eq!(SettingsPreferences::load(root.path()), saved);

            if outcome == "cancel" {
                app.cancel_github_sign_in();
            } else {
                // Use the same action that the modal's account option emits.
                app.apply_action(
                    UiAction::Clicked {
                        id: "github_account_verify:bob".into(),
                    },
                    &egui::Context::default(),
                );
                let (verify_revision, kind) = driver.request();
                assert_eq!(kind, "verify");
                assert!(app.view.github_account_refreshing);
                assert_eq!(app.github_query_account(), Some("alice"));
                assert_eq!(SettingsPreferences::load(root.path()), saved);
                let result = if outcome == "success" {
                    connected("bob")
                } else {
                    GithubAccountStatus::Unavailable("No se pudo verificar a bob.".into())
                };
                respond(&mut app, &driver, verify_revision, result);
            }
            if outcome == "success" {
                assert_eq!(app.github_query_account(), Some("bob"));
                assert_eq!(
                    SettingsPreferences::load(root.path()).github_account,
                    GithubAccountPreference::Selected("bob".into())
                );
                assert!(app.review_generation > review_revision);
                assert!(app.view.github_sign_in.is_none());
            } else {
                assert_eq!(app.github_query_account(), Some("alice"));
                assert_eq!(app.review_generation, review_revision);
                assert_eq!(SettingsPreferences::load(root.path()), saved);
            }
        }
    }

    #[test]
    fn saving_failure_cannot_claim_a_new_account_or_a_durable_disconnect() {
        let root = tempfile::tempdir().unwrap();
        let (mut app, driver) = host(&root.path().join("data"));
        app.apply_github_account_event(GithubAccountEvent::Status(connected("alice")));
        let saved = app.settings_preferences.clone();
        let blocked = root.path().join("not-a-directory");
        std::fs::write(&blocked, "private fixture").unwrap();
        app.data_root = blocked;
        app.start_github_sign_in();
        let (revision, _) = driver.request();
        respond(&mut app, &driver, revision, connected("bob"));
        assert_eq!(app.view.github_account, connected("alice"));
        assert_eq!(app.settings_preferences, saved);
        assert!(matches!(
            app.view.github_sign_in,
            Some(GithubSignInView::Error(_))
        ));
        app.disconnect_github_account();
        assert_eq!(app.view.github_account, connected("alice"));
        assert_eq!(app.settings_preferences, saved);
        assert!(app.view.github_account_notice.is_some());
    }

    #[test]
    fn manage_opens_github_settings_and_only_listed_accounts_can_be_verified() {
        let root = tempfile::tempdir().unwrap();
        let (mut app, driver) = host(root.path());
        app.view.github_account = GithubAccountStatus::Unverified {
            accounts: vec!["alice".into()],
            notice: None,
        };
        app.verify_github_account("unknown");
        assert!(!app.view.github_account_refreshing);
        app.verify_github_account("alice");
        let (revision, kind) = driver.request();
        assert_eq!(kind, "verify");
        respond(&mut app, &driver, revision, connected("alice"));
        assert_eq!(app.github_query_account(), Some("alice"));
        app.view.github_account_menu_open = true;
        app.view.provider_usage_open = true;
        app.apply_action(
            UiAction::Clicked {
                id: "github_account_manage".into(),
            },
            &egui::Context::default(),
        );
        assert!(app.view.show_settings);
        assert_eq!(app.view.settings_page, SettingsPage::Github);
        assert!(!app.view.github_account_menu_open);
        assert!(!app.view.provider_usage_open);
    }
}
