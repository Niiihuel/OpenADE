//! The app side of [`crate::app_update`]: when to check, what the user is
//! told, what the buttons do and the swap started when the app quits.
use super::*;
use crate::app_update::{self, Install, Message, Outcome, Release, UpdateStatus, UpdateView};
use crate::notifications::{Notice, Severity};

/// The one entry about updates in the notification list.
const KEY: &str = "app_update";

pub(super) struct Updates {
    install: Option<Install>,
    /// The check running, and whether the user asked for it.
    worker: Option<(Receiver<Message>, bool)>,
    next_check: Instant,
    staged: Option<PathBuf>,
    /// The version already announced, so a check every few hours does not
    /// announce it again.
    announced: Option<String>,
    /// Open the new version once the swap is done.
    restart: bool,
}

fn release_page(version: &str) -> String {
    format!("https://github.com/Niiihuel/OpenADE/releases/tag/v{version}")
}

impl Updates {
    /// What this copy is, and a version staged by an earlier session.
    pub(super) fn new(automatic: bool) -> (Self, UpdateView) {
        let install = Install::detect();
        let staged = install
            .as_ref()
            .and_then(|install| install.staged(&app_update::current_version()));
        let status = match (&staged, install.as_ref().and_then(Install::last_error)) {
            (Some(_), Some(error)) => UpdateStatus::Failed(error),
            (Some((version, _)), None) => UpdateStatus::Ready {
                version: version.to_string(),
                page: release_page(&version.to_string()),
            },
            (None, _) => UpdateStatus::Idle,
        };
        let view = UpdateView {
            status,
            installable: install.is_some(),
            automatic,
        };
        let updates = Self {
            install,
            worker: None,
            // Tests never reach the network on their own.
            next_check: Instant::now()
                + if cfg!(test) {
                    Duration::from_secs(365 * 24 * 60 * 60)
                } else {
                    app_update::FIRST_CHECK
                },
            staged: staged.map(|(_, path)| path),
            announced: None,
            restart: false,
        };
        (updates, view)
    }

    pub(super) fn busy(&self) -> bool {
        self.worker.is_some()
    }
}

impl AgentsWindow {
    /// Starts a check. One the user asked for says what it found even when
    /// there is nothing new; `download` fetches a newer version this copy
    /// can install.
    pub(super) fn start_update_check(&mut self, manual: bool, download: bool) {
        if self.updates.busy() {
            return;
        }
        let receiver = app_update::spawn_check(self.updates.install.clone(), download);
        self.updates.worker = Some((receiver, manual));
        if manual || !matches!(self.view.update.status, UpdateStatus::Ready { .. }) {
            self.view.update.status = UpdateStatus::Checking;
        }
    }

    pub(super) fn poll_app_update(&mut self, ctx: &egui::Context) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.view.update.automatic
            && !self.updates.busy()
            && self.updates.staged.is_none()
            && now >= self.updates.next_check
        {
            self.start_update_check(false, self.updates.install.is_some());
            changed = true;
        }
        if self.view.update.automatic && !self.updates.busy() {
            ctx.request_repaint_after(self.updates.next_check.saturating_duration_since(now));
        }
        let Some((receiver, manual)) = &self.updates.worker else {
            return changed;
        };
        let manual = *manual;
        loop {
            match receiver.try_recv() {
                Ok(Message::Downloading {
                    version,
                    received,
                    total,
                }) => {
                    let version = version.to_string();
                    if manual {
                        let percent = (received * 100).checked_div(total).unwrap_or(0);
                        self.view.notifications.post(
                            Notice::new(
                                Severity::Info,
                                format!("Descargando openADE {version}"),
                                if total > 0 {
                                    format!("{percent} % de {} MB", total / 1_000_000)
                                } else {
                                    format!("{} MB", received / 1_000_000)
                                },
                            )
                            .key(KEY)
                            .sticky()
                            .progress(received, total),
                        );
                    }
                    self.view.update.status = UpdateStatus::Downloading {
                        version,
                        received,
                        total,
                    };
                    changed = true;
                }
                Ok(Message::Done(result)) => {
                    self.updates.worker = None;
                    self.updates.next_check = Instant::now() + app_update::CHECK_INTERVAL;
                    self.finish_update_check(result, manual);
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => return changed,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.updates.worker = None;
                    self.finish_update_check(Err("La comprobación se interrumpió".into()), manual);
                    return true;
                }
            }
        }
    }

    fn finish_update_check(&mut self, result: Result<Outcome, String>, manual: bool) {
        match result {
            Ok(Outcome::UpToDate) => {
                self.view.update.status = UpdateStatus::UpToDate;
                self.view.notifications.remove_key(KEY);
                if manual {
                    self.set_settings_notice(
                        format!("openADE {} es la versión más reciente", env!("CARGO_PKG_VERSION")),
                        false,
                    );
                }
            }
            Ok(Outcome::Available(release)) => self.announce_available(release, manual),
            Ok(Outcome::Staged(release, path)) => self.announce_ready(release, path),
            Err(error) => {
                // A background check that fails is only noted in Settings;
                // the next one tries again.
                if manual {
                    self.view.notifications.post(
                        Notice::new(Severity::Error, "No se pudo actualizar openADE", error.clone())
                            .key(KEY),
                    );
                }
                self.view.update.status = match &self.updates.staged {
                    Some(_) if !manual => self.view.update.status.clone(),
                    _ => UpdateStatus::Failed(error),
                };
            }
        }
    }

    fn announce_available(&mut self, release: Release, manual: bool) {
        let version = release.version.to_string();
        let signed = release.archive.is_some();
        self.view.update.status = UpdateStatus::Available {
            version: version.clone(),
            page: release.page.clone(),
            signed,
        };
        if !manual && self.updates.announced.as_deref() == Some(version.as_str()) {
            return;
        }
        self.updates.announced = Some(version.clone());
        let notice = Notice::new(
            Severity::Info,
            format!("openADE {version} está disponible"),
            if self.updates.install.is_some() && signed {
                "Se descarga en segundo plano y se instala al reiniciar."
            } else {
                "Esta copia no se actualiza sola: descargá la versión nueva desde GitHub."
            },
        )
        .key(KEY)
        .sticky();
        let notice = if self.updates.install.is_some() && signed {
            notice
                .action("update_download", "Descargar e instalar")
                .action("update_notes", "Novedades")
        } else {
            notice.action("update_notes", "Ver la versión")
        };
        self.view.notifications.post(notice);
    }

    fn announce_ready(&mut self, release: Release, path: PathBuf) {
        let version = release.version.to_string();
        self.updates.staged = Some(path);
        self.updates.announced = Some(version.clone());
        self.view.update.status = UpdateStatus::Ready {
            version: version.clone(),
            page: release.page,
        };
        let title = format!("openADE {version} está lista");
        let detail = "Se instala al reiniciar, o la próxima vez que cierres openADE.";
        if !self.window_focused && self.view.notifications.settings.uses_system() {
            crate::notifications::notify_system(&title, detail);
        }
        self.view.notifications.post(
            Notice::new(Severity::Info, title, detail)
                .key(KEY)
                .sticky()
                .action("update_restart", "Reiniciar ahora")
                .action("update_notes", "Novedades"),
        );
    }

    /// The buttons of the update's card and of Settings.
    pub(super) fn apply_update_action(&mut self, id: &str, ctx: &egui::Context) {
        match id {
            "update_check" => {
                let download = self.view.update.automatic && self.updates.install.is_some();
                self.start_update_check(true, download);
            }
            "update_download" => {
                self.view.notifications.remove_key(KEY);
                self.start_update_check(true, true);
            }
            "update_restart" => {
                if self.updates.staged.is_some() {
                    self.updates.restart = true;
                    self.view.notifications.remove_key(KEY);
                    // Closing goes through the same confirmations as any quit.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            "update_notes" => {
                let page = match &self.view.update.status {
                    UpdateStatus::Available { page, .. } | UpdateStatus::Ready { page, .. } => page.clone(),
                    _ => "https://github.com/Niiihuel/OpenADE/releases/latest".to_owned(),
                };
                ctx.open_url(egui::OpenUrl::new_tab(page));
            }
            "settings_auto_update" => {
                let mut next = self.settings_preferences.clone();
                next.auto_update = !next.auto_update;
                let automatic = next.auto_update;
                if self.commit_settings_preferences(next, "Preferencia de actualizaciones guardada") {
                    self.view.update.automatic = automatic;
                    // Turned on, it checks shortly instead of hours from now.
                    self.updates.next_check = Instant::now() + Duration::from_secs(2);
                }
            }
            _ => {}
        }
    }

    /// A staged version goes in once this process is gone; after "Reiniciar
    /// ahora" it is opened too.
    pub(super) fn apply_staged_update_on_exit(&mut self) {
        let (Some(install), Some(staged)) = (&self.updates.install, &self.updates.staged) else {
            return;
        };
        let _ = install.spawn_apply(staged, std::process::id(), self.updates.restart);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{Size, UiTree};

    fn release(version: &str, signed: bool) -> Release {
        Release {
            version: semver::Version::parse(version).unwrap(),
            page: release_page(version),
            archive: signed.then(|| ("archive".to_owned(), "signature".to_owned())),
        }
    }

    fn action_ids(app: &AgentsWindow) -> Vec<String> {
        app.view
            .notifications
            .by_key(KEY)
            .map(|item| item.actions.iter().map(|action| action.id.clone()).collect())
            .unwrap_or_default()
    }

    fn settings_text(app: &AgentsWindow) -> String {
        let frame = UiTree::new(crate::settings_ui::updates_group(&app.view.update, false))
            .layout(Size::new(760.0, 400.0));
        frame
            .commands
            .iter()
            .filter_map(|command| match command {
                openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn a_found_version_is_announced_once_downloaded_with_progress_and_offered_for_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        let ctx = egui::Context::default();
        assert!(app.view.update.automatic && !app.view.update.installable);
        assert!(settings_text(&app).contains("Buscar actualizaciones"));

        // This test binary is no release archive: a newer version is pointed to.
        app.finish_update_check(Ok(Outcome::Available(release("9.0.0", true))), false);
        assert_eq!(action_ids(&app), ["update_notes"]);
        assert!(settings_text(&app).contains("Ver la versión"));
        app.view.notifications.remove_key(KEY);
        app.finish_update_check(Ok(Outcome::Available(release("9.0.0", true))), false);
        assert!(app.view.notifications.by_key(KEY).is_none(), "not announced again every few hours");
        app.finish_update_check(Ok(Outcome::Available(release("9.0.0", true))), true);
        assert!(app.view.notifications.by_key(KEY).is_some(), "but always when asked");

        // A copy that can install itself offers to.
        app.updates.install = Some(Install { root: root.path().join("openade") });
        app.view.update.installable = true;
        app.finish_update_check(Ok(Outcome::Available(release("9.0.1", true))), false);
        assert_eq!(action_ids(&app), ["update_download", "update_notes"]);
        assert!(settings_text(&app).contains("Descargar e instalar"));
        app.finish_update_check(Ok(Outcome::Available(release("9.0.2", false))), false);
        assert_eq!(action_ids(&app), ["update_notes"], "an unsigned archive is not downloaded");

        // A download the user started shows its progress on its card.
        let (sender, receiver) = mpsc::channel();
        app.updates.worker = Some((receiver, true));
        sender
            .send(Message::Downloading { version: semver::Version::new(9, 0, 2), received: 45, total: 180 })
            .unwrap();
        assert!(app.poll_app_update(&ctx));
        let card = app.view.notifications.by_key(KEY).unwrap();
        assert_eq!((card.title.as_str(), card.progress), ("Descargando openADE 9.0.2", Some((45, 180))));
        assert!(settings_text(&app).contains("Descargando openADE 9.0.2: 25 %"));

        let staged = root.path().join("staged");
        sender.send(Message::Done(Ok(Outcome::Staged(release("9.0.2", true), staged.clone())))).unwrap();
        assert!(app.poll_app_update(&ctx));
        assert!(!app.updates.busy());
        assert_eq!(app.updates.staged.as_ref(), Some(&staged));
        let card = app.view.notifications.by_key(KEY).unwrap();
        assert!(card.sticky && card.progress.is_none() && card.title == "openADE 9.0.2 está lista");
        assert_eq!(action_ids(&app), ["update_restart", "update_notes"]);
        assert!(settings_text(&app).contains("Reiniciar ahora"));
        // A later check that fails keeps the version ready.
        app.finish_update_check(Err("sin red".into()), false);
        assert!(matches!(app.view.update.status, UpdateStatus::Ready { .. }));

        app.apply_action(UiAction::Clicked { id: "update_restart".into() }, &ctx);
        assert!(app.updates.restart && app.view.notifications.by_key(KEY).is_none());
    }

    #[test]
    fn a_check_the_user_asked_for_says_when_there_is_nothing_new_or_what_failed() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.finish_update_check(Ok(Outcome::UpToDate), true);
        assert!(app.view.settings_notice.as_deref().is_some_and(|notice| notice.contains("más reciente")));
        assert!(settings_text(&app).contains("Es la versión más reciente."));
        app.finish_update_check(Err("GitHub respondió 503".into()), true);
        let card = app.view.notifications.by_key(KEY).unwrap();
        assert_eq!((card.severity, card.detail.as_str()), (Severity::Error, "GitHub respondió 503"));
        assert!(settings_text(&app).contains("No se pudo actualizar: GitHub respondió 503"));

        // Switched off, it is saved and a background check no longer starts.
        let ctx = egui::Context::default();
        app.apply_action(UiAction::Clicked { id: "settings_auto_update".into() }, &ctx);
        assert!(!app.view.update.automatic);
        assert!(!SettingsPreferences::load(&app.data_root).auto_update);
        app.updates.next_check = Instant::now();
        app.poll_app_update(&ctx);
        assert!(!app.updates.busy());
    }
}
