//! Command picker navigation keeps the query focused while revealing its active row.
use super::*;

impl AgentsWindow {
    pub(super) fn reset_command_navigation(&mut self) {
        self.view.command_selected_index = 0;
        self.command_tree.scroll_to_start("command_results");
        // InputChanged can occur in the middle of a query or an IME composition.
        // Re-focusing an already focused input would move its caret to the end.
        self.command_focus_pending |= self.command_tree.focused_id() != Some("command_query");
    }

    pub(super) fn reveal_selected_command_result(&mut self) {
        if self.view.show_command_center && !self.view.command_results.is_empty() {
            self.command_tree.scroll_to_index(
                "command_results",
                crate::view::command_result_row(&self.view, self.view.command_selected_index),
            );
        }
    }

    pub(super) fn move_command_selection(&mut self, down: bool) {
        if !self.view.show_command_center || self.command_tree.context_menu_open() {
            return;
        }
        self.view.command_selected_index = if down {
            self.view.command_selected_index.saturating_add(1)
        } else {
            self.view.command_selected_index.saturating_sub(1)
        }
        .min(self.view.command_results.len().saturating_sub(1));
        // Tab can leave focus on a row or filter. Once arrow navigation chooses
        // a result, Enter must submit that selection rather than the old button.
        // Preserve an active query's caret instead of focusing it again.
        self.command_focus_pending |= self.command_tree.focused_id() != Some("command_query");
        self.reveal_selected_command_result();
    }

    pub(super) fn set_command_results(&mut self, results: Vec<command_center::ResultItem>) {
        let changed = self.view.command_results != results;
        if changed {
            // Stable row IDs retain focus across layout. Their action aliases are
            // resolved on release, so cancel a held press before a worker replaces
            // its target. PointerLeave keeps keyboard/input focus intact.
            self.command_tree.handle_event(UiEvent::PointerLeave);
        }
        self.view.command_results = results;
        self.view.command_selected_index = self
            .view
            .command_selected_index
            .min(self.view.command_results.len().saturating_sub(1));
        if changed {
            self.reveal_selected_command_result();
        }
    }

    pub(super) fn activate_scoped_command_result(&mut self, action: &str) {
        if !self.view.show_command_center {
            return;
        }
        let Some(index) = action
            .strip_prefix("command_pick:")
            .and_then(|payload| payload.split_once(':'))
            .and_then(|(index, _)| index.parse::<usize>().ok())
            .filter(|index| *index < self.view.command_results.len())
        else {
            return;
        };
        if action == crate::view::command_result_action(&self.view, index) {
            self.activate_command_result(index);
        }
    }
}

#[cfg(test)]
#[path = "command_center_interaction_app_tests.rs"]
mod tests;
