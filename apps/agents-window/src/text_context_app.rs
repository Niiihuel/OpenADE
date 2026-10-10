//! Host actions for the terminal's context menu. Native text editing stays in UiTree.

use super::*;

impl AgentsWindow {
    pub(super) fn handle_terminal_context_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let Some(target) = id.strip_prefix("terminal_context:") else {
            return false;
        };
        let Some((terminal_id, operation)) = target.split_once(':') else {
            return true;
        };
        let Ok(terminal_id) = terminal_id.parse::<u64>() else {
            return true;
        };
        // A retained menu must never send text/control bytes to a different
        // terminal after the selected session, workspace or dock tab changed.
        if self.active_terminal_id() != Some(terminal_id)
            || !self.terminal_panel_interactive()
            || self.view.show_settings
            || self.view.show_command_center
            || self.view.main_page != MainPage::Workspace
        {
            return true;
        }
        let Some(snapshot) = self.view.terminal_snapshot.as_ref() else {
            return true;
        };
        match operation {
            "copy_selection" => {
                if let Some(text) = snapshot.selection.as_ref().filter(|text| !text.is_empty()) {
                    ctx.copy_text(text.clone());
                }
            }
            "copy_output" => ctx.copy_text(snapshot.lines.join("\n").trim_end().to_owned()),
            "paste" if snapshot.running => {
                self.request_text_paste(self.active_terminal_screen_id(), ctx);
            }
            "clear" if snapshot.running => self.terminal_write(&[12]),
            _ => {}
        }
        true
    }
}
