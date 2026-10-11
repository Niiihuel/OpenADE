//! Starts, walks and remembers the guided tours of [`crate::tour`].
//!
//! A tour only shows itself and opens the panel a step points into. It never
//! starts on its own: the first time a window mode is shown it is offered
//! once, and from then on it is started from a menu or from Settings.
use super::*;
use crate::settings_preferences::TourRecord;
use crate::tour::{ActiveTour, Ensure, TourId};

impl AgentsWindow {
    pub(super) fn tour_showing(&self) -> bool {
        crate::tour_ui::showing(&self.view, self.viewport_width)
    }

    pub(super) fn apply_tour_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(key) = id.strip_prefix("tour_start:") {
            if let Some(tour) = TourId::from_key(key) {
                self.start_tour(tour);
            }
            return true;
        }
        let active = self.view.tour.active;
        match (id.as_str(), active) {
            ("tour_invite_decline", _) => self.view.tour.invites.clear(),
            ("tour_reset", _) => {
                let mut next = self.settings_preferences.clone();
                next.tours.clear();
                if self.commit_settings_preferences(next, "Los recorridos se ofrecerán de nuevo") {
                    self.view.tour.invites.clear();
                }
            }
            ("tour" | "tour:skip", _) => self.end_tour(false),
            ("tour:next", Some(active)) if active.step + 1 < active.tour.steps().len() => {
                self.tour_go(active.step + 1, false);
            }
            ("tour:next", _) => self.end_tour(true),
            ("tour:back", Some(active)) => self.tour_go(active.step.saturating_sub(1), true),
            ("tour:back", None) => {}
            _ => return false,
        }
        true
    }

    /// Show `tour` from its first step, in its own window mode. Refused while
    /// a dialog or a request for permission is waiting for an answer.
    pub(super) fn start_tour(&mut self, tour: TourId) {
        if !crate::tour_ui::can_start(&self.view, self.viewport_width) {
            self.set_settings_notice(
                "Responde lo que está pendiente antes de empezar el recorrido",
                false,
            );
            return;
        }
        if self.view.ide.mode != tour.mode() {
            self.apply_ide_workspace_action(&UiAction::Clicked {
                id: "window_mode_toggle".into(),
            });
        }
        let changed_page = self.view.show_settings || self.view.main_page != MainPage::Workspace;
        self.view.show_settings = false;
        self.view.main_page = MainPage::Workspace;
        if changed_page {
            self.clear_settings_notice();
            self.visit_navigation();
        }
        // Nothing left floating under a bubble that makes it unreachable.
        self.close_composer_floating_controls();
        self.close_session_menu();
        self.view.github_account_menu_open = false;
        self.view.provider_usage_open = false;
        self.view.ide.menu_open = None;
        self.tree.dismiss_context_menu();
        self.view.tour.invites.clear();
        self.view.tour.active = Some(ActiveTour {
            tour,
            step: 0,
            backwards: false,
            ensured: false,
        });
    }

    fn tour_go(&mut self, step: usize, backwards: bool) {
        if let Some(active) = &mut self.view.tour.active {
            *active = ActiveTour {
                step,
                backwards,
                ensured: false,
                ..*active
            };
        }
    }

    fn end_tour(&mut self, completed: bool) {
        if let Some(active) = self.view.tour.active.take() {
            self.record_tour(active.tour, completed);
        }
    }

    /// Remember that `tour` was offered, so it is not offered again until its
    /// version is raised. A tour once completed stays completed.
    fn record_tour(&mut self, tour: TourId, completed: bool) {
        let previous = self.settings_preferences.tours.get(tour.key()).copied();
        let record = TourRecord {
            version: tour.version(),
            completed: completed
                || previous.is_some_and(|record| {
                    record.completed && record.version == tour.version()
                }),
        };
        if previous == Some(record) {
            return;
        }
        let mut next = self.settings_preferences.clone();
        next.tours.insert(tour.key().to_owned(), record);
        match next.save(&self.data_root) {
            Ok(()) => self.settings_preferences = next,
            Err(error) => self.set_settings_notice(error, true),
        }
    }

    /// Before each frame: a tour belongs to one window mode and ends when the
    /// other is shown, and a mode shown for the first time offers its tour.
    pub(super) fn sync_tour(&mut self) -> bool {
        let before = self.view.tour.clone();
        let here = TourId::for_mode(self.view.ide.mode);
        if self.view.tour.active.is_some_and(|active| active.tour != here) {
            self.end_tour(false);
        }
        let offered = self
            .settings_preferences
            .tours
            .get(here.key())
            .is_some_and(|record| record.version >= here.version());
        if !offered
            && self.view.tour.active.is_none()
            && !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
        {
            self.view.tour.invites.push(here);
            self.record_tour(here, false);
        }
        self.view.tour != before
    }

    /// Open the panel a step points into. A panel the window has no room for
    /// would open as a drawer over the editor: it is left closed and the step
    /// is skipped instead.
    fn tour_ensure(&mut self, ensure: Ensure, ctx: &egui::Context) -> bool {
        use crate::ide_workspace::{nominal_layout, IdeActivity};
        let action = match ensure {
            Ensure::Nothing => None,
            Ensure::Sidebar => self.view.sidebar_collapsed.then_some("toggle_sidebar"),
            Ensure::IdeBottom => (!self.view.show_terminal_panel && !self.view.ide.bottom_open)
                // Problems opens the panel without starting a shell.
                .then_some("ide_bottom:problems"),
            Ensure::IdeExplorer => {
                let files = self.view.ide.activity == IdeActivity::Files;
                let shown = nominal_layout(&self.view, self.viewport_width).left > 0.0;
                let was_open = std::mem::replace(&mut self.view.ide.explorer_open, true);
                let fits = nominal_layout(&self.view, self.viewport_width).left > 0.0;
                self.view.ide.explorer_open = was_open;
                match (fits, shown, files) {
                    (false, ..) | (true, true, true) => None,
                    (true, _, false) => Some("ide_activity:files"),
                    (true, false, true) => Some("ide_explorer_toggle"),
                }
            }
            Ensure::IdeAgent => {
                let shown = nominal_layout(&self.view, self.viewport_width).right.is_some();
                let was_open = std::mem::replace(&mut self.view.ide.agent_open, true);
                let fits = nominal_layout(&self.view, self.viewport_width).right.is_some();
                self.view.ide.agent_open = was_open;
                (fits && !shown).then_some("ide_agent_toggle")
            }
        };
        let Some(id) = action else {
            return false;
        };
        self.apply_action(UiAction::Clicked { id: id.into() }, ctx);
        true
    }

    /// After a layout and before it is painted: open what the step needs, and
    /// leave a step whose target is not on screen in the direction of travel.
    /// Past either end the tour turns round or ends, so it never shows a
    /// bubble with nothing to point at.
    pub(super) fn settle_tour(
        &mut self,
        mut frame: UiFrame,
        viewport: Size,
        ctx: &egui::Context,
    ) -> UiFrame {
        // Each pass leaves a step, so twice the longest tour is a safe bound.
        for _ in 0..24 {
            let Some(active) = self.view.tour.active.filter(|_| self.tour_showing()) else {
                break;
            };
            let steps = active.tour.steps();
            if !active.ensured {
                if let Some(active) = &mut self.view.tour.active {
                    active.ensured = true;
                }
                if self.tour_ensure(steps[active.step].ensure, ctx) {
                    self.update_dock_motion(viewport.width, Instant::now());
                    frame = self.layout_workspace_frame(viewport);
                    continue;
                }
            }
            if frame.missing_coach_target().is_none() {
                break;
            }
            if self.view.left_presentation_width.is_some()
                || self.view.right_presentation_width.is_some()
                || self.view.bottom_presentation_height.is_some()
            {
                // The panel is still opening: its controls arrive with it.
                ctx.request_repaint();
                break;
            }
            match (active.backwards, active.step) {
                (true, 0) => self.tour_go(1, false),
                (true, step) => self.tour_go(step - 1, true),
                (false, step) if step + 1 < steps.len() => self.tour_go(step + 1, false),
                (false, _) => self.end_tour(true),
            }
            frame = self.layout_workspace_frame(viewport);
        }
        frame
    }
}

#[cfg(test)]
#[path = "tour_app_tests.rs"]
mod tests;
