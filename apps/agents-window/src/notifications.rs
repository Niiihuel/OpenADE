//! Events that happened away from what the user was looking at.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

const MAX_NOTIFICATIONS: usize = 50;
const SETTINGS_FILE: &str = "notification-settings.json";
const MAX_SETTINGS_BYTES: u64 = 16 * 1024;
const TOAST_DURATION: Duration = Duration::from_secs(6);
/// Cards on screen at once; older ones stay in the list.
const VISIBLE_TOASTS: usize = 3;

/// What produced a notification; each kind can be switched off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    TurnCompleted,
    TurnFailed,
    Approval,
    /// A watched pull request changed on GitHub.
    PullRequest,
}

impl Event {
    pub const fn severity(self) -> Severity {
        match self {
            Self::TurnCompleted => Severity::Info,
            Self::TurnFailed => Severity::Error,
            Self::Approval | Self::PullRequest => Severity::Attention,
        }
    }
}

/// A user preference. A missing or unreadable file means the defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub turn_completed: bool,
    pub turn_failed: bool,
    pub approvals: bool,
    pub pull_requests: bool,
    /// Brief cards inside the window.
    pub toasts: bool,
    /// Desktop notifications while the window is in the background.
    pub system: bool,
    /// A short chime with each notice.
    pub sound: bool,
    /// Events are still listed, without cards or desktop notifications.
    pub do_not_disturb: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            turn_completed: true,
            turn_failed: true,
            approvals: true,
            pull_requests: true,
            toasts: true,
            system: true,
            sound: true,
            do_not_disturb: false,
        }
    }
}

impl NotificationSettings {
    pub const NAMES: [&'static str; 8] = [
        "turn_completed",
        "turn_failed",
        "approvals",
        "pull_requests",
        "toasts",
        "system",
        "sound",
        "do_not_disturb",
    ];

    pub fn load(root: &Path) -> Self {
        let path = root.join(SETTINGS_FILE);
        if !fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_SETTINGS_BYTES) {
            return Self::default();
        }
        fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let failed = |error: std::io::Error| {
            format!("No se pudieron guardar los ajustes de notificaciones: {error}")
        };
        fs::create_dir_all(root).map_err(failed)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        let temporary = root.join(format!(".{SETTINGS_FILE}.{}.tmp", std::process::id()));
        fs::write(&temporary, bytes).map_err(failed)?;
        crate::agent_tool_settings::replace_atomically(&temporary, &root.join(SETTINGS_FILE))
            .map_err(|error| {
                let _ = fs::remove_file(&temporary);
                failed(error)
            })
    }

    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "turn_completed" => &mut self.turn_completed,
            "turn_failed" => &mut self.turn_failed,
            "approvals" => &mut self.approvals,
            "pull_requests" => &mut self.pull_requests,
            "toasts" => &mut self.toasts,
            "system" => &mut self.system,
            "sound" => &mut self.sound,
            "do_not_disturb" => &mut self.do_not_disturb,
            _ => return None,
        })
    }

    pub fn get(&self, name: &str) -> bool {
        let mut copy = *self;
        copy.slot(name).is_some_and(|value| *value)
    }

    /// False when the name is not a known setting.
    pub fn toggle(&mut self, name: &str) -> bool {
        self.slot(name).map(|value| *value = !*value).is_some()
    }

    pub const fn allows(&self, event: Event) -> bool {
        match event {
            Event::TurnCompleted => self.turn_completed,
            Event::TurnFailed => self.turn_failed,
            Event::Approval => self.approvals,
            Event::PullRequest => self.pull_requests,
        }
    }

    pub const fn shows_toasts(&self) -> bool {
        self.toasts && !self.do_not_disturb
    }

    pub const fn uses_system(&self) -> bool {
        self.system && !self.do_not_disturb
    }

    pub const fn uses_sound(&self) -> bool {
        self.sound && !self.do_not_disturb
    }
}

/// Hands the event to the desktop's notification service. Never blocks the
/// interface, and a desktop without such a service is not an error.
pub fn notify_system(title: &str, detail: &str) {
    #[cfg(not(test))]
    {
        let title = crate::i18n::text(title).unwrap_or_else(|| title.to_owned());
        let detail = crate::i18n::text(detail).unwrap_or_else(|| detail.to_owned());
        let _ = std::thread::Builder::new()
            .name("desktop-notification".into())
            .spawn(move || {
                let _ = notify_rust::Notification::new()
                    .appname("openADE")
                    .summary(&title)
                    .body(&detail)
                    .show();
            });
    }
    #[cfg(test)]
    let _ = (title, detail);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Attention,
    Error,
}

/// Where selecting a notification takes the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Session(String),
}

/// A button on a notification's card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub id: u64,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub target: Option<Target>,
    /// How many times the same event was reported.
    pub count: u32,
    pub read: bool,
    pub at: SystemTime,
    /// Its buttons, the main one first. Selecting the entry in the list runs
    /// the first when it leads nowhere else.
    pub actions: Vec<Action>,
    /// Its card stays until it is dismissed or the entry changes.
    pub sticky: bool,
    /// Bytes or steps done of a total, while the work runs.
    pub progress: Option<(u64, u64)>,
    /// One entry per key: posting it again replaces that entry in place.
    pub key: Option<&'static str>,
}

/// What [`NotificationCenter::post`] records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    severity: Severity,
    title: String,
    detail: String,
    target: Option<Target>,
    actions: Vec<Action>,
    sticky: bool,
    progress: Option<(u64, u64)>,
    key: Option<&'static str>,
}

impl Notice {
    pub fn new(severity: Severity, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            severity,
            title: title.into(),
            detail: detail.into(),
            target: None,
            actions: Vec::new(),
            sticky: false,
            progress: None,
            key: None,
        }
    }

    pub fn target(mut self, target: Option<Target>) -> Self {
        self.target = target;
        self
    }

    pub fn action(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.actions.push(Action {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    pub fn sticky(mut self) -> Self {
        self.sticky = true;
        self
    }

    pub fn progress(mut self, done: u64, total: u64) -> Self {
        self.progress = Some((done, total));
        self
    }

    pub fn key(mut self, key: &'static str) -> Self {
        self.key = Some(key);
        self
    }
}

/// Newest first. Shared by every window of the app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationCenter {
    pub open: bool,
    pub settings: NotificationSettings,
    items: Vec<Notification>,
    next_id: u64,
    /// Entries announced in cards, oldest first, and when each appeared.
    toasts: Vec<(u64, Instant)>,
}

impl NotificationCenter {
    pub fn with_settings(settings: NotificationSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    /// A repeated event raises the count of its entry instead of adding one.
    pub fn push(
        &mut self,
        severity: Severity,
        title: impl Into<String>,
        detail: impl Into<String>,
        target: Option<Target>,
    ) {
        self.post(Notice::new(severity, title, detail).target(target));
    }

    /// Records `notice` and announces it in a card. A notice with a key
    /// replaces that key's entry; one with a new title is announced again,
    /// while a new detail or progress only updates what is shown.
    pub fn post(&mut self, notice: Notice) {
        let Notice {
            severity,
            title,
            detail,
            target,
            actions,
            sticky,
            progress,
            key,
        } = notice;
        let earlier = self.items.iter().position(|item| match key {
            Some(key) => item.key == Some(key),
            None => {
                item.key.is_none()
                    && item.severity == severity
                    && item.title == title
                    && item.detail == detail
                    && item.target == target
            }
        });
        let (mut item, announce) = match earlier {
            Some(index) => {
                let mut item = self.items.remove(index);
                let announce = key.is_none() || item.title != title;
                if key.is_none() {
                    item.count = item.count.saturating_add(1);
                }
                (item, announce)
            }
            None => {
                self.next_id += 1;
                let item = Notification {
                    id: self.next_id,
                    severity,
                    title: String::new(),
                    detail: String::new(),
                    target: None,
                    count: 1,
                    read: false,
                    at: SystemTime::now(),
                    actions: Vec::new(),
                    sticky: false,
                    progress: None,
                    key,
                };
                (item, true)
            }
        };
        item.severity = severity;
        item.title = title;
        item.detail = detail;
        item.target = target;
        item.actions = actions;
        item.sticky = sticky;
        item.progress = progress;
        if announce {
            item.read = false;
            item.at = SystemTime::now();
            self.toasts.retain(|(id, _)| *id != item.id);
            if self.settings.shows_toasts() {
                self.toasts.push((item.id, Instant::now()));
            }
        }
        self.items.insert(0, item);
        self.items.truncate(MAX_NOTIFICATIONS);
        let items = &self.items;
        self.toasts.retain(|(id, _)| items.iter().any(|item| item.id == *id));
    }

    pub fn items(&self) -> &[Notification] {
        &self.items
    }

    pub fn by_key(&self, key: &str) -> Option<&Notification> {
        self.items.iter().find(|item| item.key == Some(key))
    }

    /// Removes the entry with this key, and its card.
    pub fn remove_key(&mut self, key: &str) {
        if let Some(id) = self.by_key(key).map(|item| item.id) {
            self.take(id);
        }
    }

    /// The entries to announce in cards, oldest first: the newest few, each
    /// briefly unless it is sticky, and none while the list itself is open.
    pub fn toasts(&self, now: Instant) -> Vec<&Notification> {
        if self.open {
            return Vec::new();
        }
        let shown: Vec<_> = self
            .toasts
            .iter()
            .filter_map(|(id, shown)| {
                let item = self.items.iter().find(|item| item.id == *id)?;
                (item.sticky || now.saturating_duration_since(*shown) < TOAST_DURATION).then_some(item)
            })
            .collect();
        shown[shown.len().saturating_sub(VISIBLE_TOASTS)..].to_vec()
    }

    /// How long until the next card that is not sticky leaves the screen.
    pub fn toast_remaining(&self, now: Instant) -> Option<Duration> {
        let shown = self.toasts(now);
        self.toasts
            .iter()
            .filter(|(id, _)| shown.iter().any(|item| item.id == *id && !item.sticky))
            .map(|(_, at)| TOAST_DURATION.saturating_sub(now.saturating_duration_since(*at)))
            .min()
    }

    pub fn dismiss_toast(&mut self, id: u64) {
        self.toasts.retain(|(shown, _)| *shown != id);
    }

    pub fn dismiss_toasts(&mut self) {
        self.toasts.clear();
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|item| !item.read).count()
    }

    /// Entries stay marked as new while the list is open and are read once it closes.
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open = true;
        }
    }

    pub fn close(&mut self) {
        if std::mem::take(&mut self.open) {
            for item in &mut self.items {
                item.read = true;
            }
        }
    }

    /// Removes the entry and returns it.
    pub fn take(&mut self, id: u64) -> Option<Notification> {
        let index = self.items.iter().position(|item| item.id == id)?;
        self.dismiss_toast(id);
        Some(self.items.remove(index))
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.toasts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> Option<Target> {
        Some(Target::Session(id.into()))
    }

    #[test]
    fn repeated_events_merge_and_the_list_is_newest_first_and_bounded() {
        let mut center = NotificationCenter::default();
        center.push(Severity::Info, "Turno completado", "a", session("1"));
        center.push(Severity::Error, "Turno fallido", "b", session("2"));
        center.push(Severity::Info, "Turno completado", "a", session("1"));
        let items = center.items();
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].detail.as_str(), items[0].count), ("a", 2));
        assert_eq!((items[1].detail.as_str(), items[1].count), ("b", 1));
        assert_eq!(center.unread(), 2);

        // The same text for another session is a different event.
        center.push(Severity::Info, "Turno completado", "a", session("3"));
        assert_eq!(center.items().len(), 3);
        for index in 0..MAX_NOTIFICATIONS * 2 {
            center.push(Severity::Info, "Evento", index.to_string(), None);
        }
        assert_eq!(center.items().len(), MAX_NOTIFICATIONS);
        assert_eq!(center.items()[0].detail, (MAX_NOTIFICATIONS * 2 - 1).to_string());
    }

    #[test]
    fn entries_are_read_when_the_list_closes_and_a_repeat_is_new_again() {
        let mut center = NotificationCenter::default();
        center.push(Severity::Attention, "Permiso pendiente", "shell", session("1"));
        center.toggle();
        assert!(center.open);
        assert_eq!(center.unread(), 1, "still marked while the list is open");
        center.toggle();
        assert!(!center.open);
        assert_eq!(center.unread(), 0);
        center.close();
        center.push(Severity::Attention, "Permiso pendiente", "shell", session("1"));
        assert_eq!((center.unread(), center.items()[0].count), (1, 2));
    }

    #[test]
    fn the_newest_entries_are_announced_briefly_unless_cards_are_off_or_the_list_is_open() {
        let mut center = NotificationCenter::default();
        let now = Instant::now();
        assert!(center.toasts(now).is_empty());
        center.push(Severity::Info, "Turno completado", "a", None);
        center.push(Severity::Error, "Turno fallido", "b", None);
        let titles = |center: &NotificationCenter, at| {
            center.toasts(at).iter().map(|item| item.title.clone()).collect::<Vec<_>>()
        };
        assert_eq!(titles(&center, Instant::now()), ["Turno completado", "Turno fallido"]);
        let remaining = center.toast_remaining(Instant::now()).unwrap();
        assert!(remaining <= TOAST_DURATION && remaining > TOAST_DURATION / 2);
        assert!(center.toasts(Instant::now() + TOAST_DURATION).is_empty());
        assert!(center.toast_remaining(Instant::now() + TOAST_DURATION).is_none());

        // Only the newest few are cards at once; the rest stay in the list.
        for index in 0..5 {
            center.push(Severity::Info, "Evento", index.to_string(), None);
        }
        let shown = center.toasts(Instant::now());
        assert_eq!(shown.iter().map(|item| item.detail.as_str()).collect::<Vec<_>>(), ["2", "3", "4"]);

        center.toggle();
        assert!(center.toasts(Instant::now()).is_empty());
        center.toggle();
        center.clear();
        center.push(Severity::Info, "Otro", "c", None);
        let announced = center.toasts(Instant::now())[0].id;
        center.take(announced);
        assert!(center.toasts(Instant::now()).is_empty());
        center.push(Severity::Info, "Otro", "d", None);
        center.push(Severity::Info, "Otro", "e", None);
        let first = center.items()[1].id;
        center.dismiss_toast(first);
        assert_eq!(center.toasts(Instant::now())[0].detail, "e");
        center.dismiss_toasts();
        assert!(center.toasts(Instant::now()).is_empty());

        for name in ["toasts", "do_not_disturb"] {
            let mut quiet = NotificationCenter::default();
            assert!(quiet.settings.toggle(name));
            quiet.push(Severity::Info, "Turno completado", "a", None);
            assert!(quiet.toasts(Instant::now()).is_empty(), "{name}");
            assert_eq!(quiet.unread(), 1, "still listed with {name}");
        }
    }

    #[test]
    fn a_keyed_notice_is_one_entry_updated_quietly_and_announced_again_when_its_title_changes() {
        let mut center = NotificationCenter::default();
        let later = Instant::now() + TOAST_DURATION * 10;
        center.post(
            Notice::new(Severity::Info, "Descargando openADE 0.4.0", "0 %")
                .key("update")
                .sticky()
                .progress(0, 100),
        );
        // Opening and closing the list marks it read; its card stays.
        center.toggle();
        center.toggle();
        assert_eq!(center.unread(), 0);
        center.push(Severity::Info, "Turno completado", "a", None);
        center.post(
            Notice::new(Severity::Info, "Descargando openADE 0.4.0", "50 %")
                .key("update")
                .sticky()
                .progress(50, 100),
        );
        assert_eq!(center.items().len(), 2);
        let update = center.by_key("update").unwrap();
        assert_eq!((update.detail.as_str(), update.progress, update.read), ("50 %", Some((50, 100)), true));
        // Sticky, it outlasts the brief card beside it.
        let shown = center.toasts(later);
        assert_eq!(shown.iter().map(|item| item.key).collect::<Vec<_>>(), [Some("update")]);

        center.post(
            Notice::new(Severity::Info, "openADE 0.4.0 está lista", "Se instala al reiniciar.")
                .key("update")
                .sticky()
                .action("update_restart", "Reiniciar")
                .action("update_later", "Más tarde"),
        );
        let ready = center.by_key("update").unwrap();
        assert!(!ready.read && ready.progress.is_none());
        assert_eq!(ready.actions.iter().map(|action| action.id.as_str()).collect::<Vec<_>>(), ["update_restart", "update_later"]);
        let shown = center.toasts(later);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title, "openADE 0.4.0 está lista");
        assert_eq!(center.toast_remaining(Instant::now()).map(|left| left <= TOAST_DURATION), Some(true), "the brief card still counts");
        assert_eq!(center.toast_remaining(later), None, "a sticky card has no deadline");

        center.remove_key("update");
        assert!(center.by_key("update").is_none() && center.toasts(later).is_empty());
        assert_eq!(center.items().len(), 1);
    }

    #[test]
    fn settings_default_on_survive_a_round_trip_and_ignore_unknown_or_damaged_files() {
        let root = tempfile::tempdir().unwrap();
        let defaults = NotificationSettings::default();
        assert_eq!(NotificationSettings::load(root.path()), defaults);
        assert!(defaults.allows(Event::TurnCompleted) && defaults.shows_toasts() && defaults.uses_system());

        let mut settings = defaults;
        assert!(settings.toggle("turn_completed") && settings.toggle("do_not_disturb"));
        assert!(!settings.toggle("shell"));
        assert!(!settings.allows(Event::TurnCompleted) && settings.allows(Event::TurnFailed));
        assert!(!settings.shows_toasts() && !settings.uses_system());
        assert!(settings.get("do_not_disturb") && !settings.get("turn_completed") && !settings.get("shell"));
        settings.save(root.path()).unwrap();
        assert_eq!(NotificationSettings::load(root.path()), settings);
        for name in NotificationSettings::NAMES {
            assert!(settings.toggle(name), "{name}");
        }

        let path = root.path().join(SETTINGS_FILE);
        fs::write(&path, r#"{"approvals": false, "later": 1}"#).unwrap();
        let partial = NotificationSettings::load(root.path());
        assert!(!partial.approvals && partial.turn_failed && partial.toasts);
        fs::write(&path, "{").unwrap();
        assert_eq!(NotificationSettings::load(root.path()), defaults);
    }

    #[test]
    fn taking_an_entry_removes_it_and_returns_its_target() {
        let mut center = NotificationCenter::default();
        center.push(Severity::Info, "Turno completado", "a", session("7"));
        center.push(Severity::Info, "Aviso", "sin destino", None);
        let with_target = center.items()[1].id;
        let without = center.items()[0].id;
        assert_eq!(center.take(with_target).unwrap().target, session("7"));
        assert_eq!(center.take(with_target), None);
        assert_eq!(center.take(without).unwrap().target, None);
        assert!(center.items().is_empty());
        center.push(Severity::Info, "x", "y", None);
        center.clear();
        assert_eq!(center.unread(), 0);
    }
}
