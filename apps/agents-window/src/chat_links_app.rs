//! Opening a link of the conversation, and the menu of the link under a
//! secondary click. A link only ever opens from a click, a key or a menu
//! row; text that streams in opens nothing.

use super::*;
use crate::chat_links::{ChatLinkMenu, LinkTarget, MENU_ACTION_PREFIX};

impl AgentsWindow {
    /// The one place a chat link leaves the app. The address is checked
    /// again here: only HTTP(S) without credentials or control characters.
    pub(super) fn open_chat_link(&mut self, url: &str, target: LinkTarget, ctx: &egui::Context) {
        let Some(url) = openide_ui::Markdown::web_link(url) else {
            return;
        };
        let dock_shown = self.view.main_page == MainPage::Workspace && !self.view.show_settings;
        if target == LinkTarget::Integrated && dock_shown {
            self.apply_action(UiAction::Clicked { id: "dock_open:browser".into() }, ctx);
            if let Some((id, url)) = self.active_browser_id().zip(browser_restart_url(&url)) {
                // The tab's context may still be starting: the address
                // waits here and is opened once it is ready.
                self.view.browser.url_draft = url.clone();
                self.tree.set_input_value("browser_url", &url);
                self.browser_restart_urls.insert(id, url);
                return;
            }
        }
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }

    pub(super) fn apply_chat_link_menu_action(&mut self, id: &str, ctx: &egui::Context) {
        let Some(menu) = self.view.chat_link_menu.take() else {
            return;
        };
        match id.trim_start_matches(MENU_ACTION_PREFIX) {
            "integrated" => self.open_chat_link(&menu.url, LinkTarget::Integrated, ctx),
            "system" => self.open_chat_link(&menu.url, LinkTarget::System, ctx),
            "copy" => self.copy_link(menu.url, "", ctx),
            "copy_text" => ctx.copy_text(menu.text),
            _ => {}
        }
    }

    /// A secondary click, or the menu key, on a link opens that link's menu.
    /// The menu is attached to the window only for that link, so the event
    /// is replayed on a tree that has it. Any other target drops a previous
    /// link and is left to its own handler.
    pub(super) fn handle_chat_link_menu_event(
        &mut self,
        event: &UiEvent,
        ctx: &egui::Context,
    ) -> bool {
        let frame = self.tree.frame();
        let menu = match event {
            UiEvent::ContextMenu { position } => frame.hit_test(*position),
            UiEvent::ContextMenuKeyboard => self.tree.focused_id(),
            _ => return false,
        }
        .and_then(|id| ChatLinkMenu::of(frame, id));
        if menu.is_none() && self.view.chat_link_menu.is_none() {
            return false;
        }
        let opened = menu.is_some();
        self.view.chat_link_menu = menu;
        if let Some(viewport) = self.workspace_frame_size {
            self.layout_workspace_frame(viewport);
        }
        if opened {
            for action in self.tree.handle_event(event.clone()) {
                self.apply_action(action, ctx);
            }
        }
        opened
    }
}

#[cfg(test)]
#[path = "chat_links_app_tests.rs"]
mod tests;
