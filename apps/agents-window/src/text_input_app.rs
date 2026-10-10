//! Bridge explicit text-menu paste requests through the native clipboard.
//! Clipboard contents are never read merely to open or paint a menu.

use super::*;

#[derive(Clone, Debug, PartialEq)]
struct TextPasteScope {
    workspace: String,
    session: Option<String>,
    page: MainPage,
    settings_visible: bool,
    settings_page: SettingsPage,
    command_center: bool,
    dock_tab: Option<u64>,
    terminal_tab: Option<u64>,
    editor_document: Option<u64>,
}

impl TextPasteScope {
    fn from_view(view: &ViewState) -> Self {
        Self {
            workspace: view.workspace.clone(),
            session: view.selected_session.clone(),
            page: view.main_page,
            settings_visible: view.show_settings,
            settings_page: view.settings_page,
            command_center: view.show_command_center,
            dock_tab: view
                .dock_tabs
                .iter()
                .find(|tab| tab.active)
                .map(|tab| tab.id),
            terminal_tab: view
                .terminal_tabs
                .iter()
                .find(|tab| tab.active)
                .map(|tab| tab.id),
            editor_document: view
                .code_editor
                .as_ref()
                .and_then(|editor| editor.controller.as_ref())
                .and_then(|controller| controller.snapshot().ok())
                .map(|snapshot| snapshot.document_id()),
        }
    }
}

pub(super) struct TextPasteRequest {
    id: String,
    scope: TextPasteScope,
    requested_at: Instant,
}

impl AgentsWindow {
    pub(super) fn request_text_paste(&mut self, id: String, ctx: &egui::Context) {
        let tree = if self.view.show_command_center {
            &self.command_tree
        } else {
            &self.tree
        };
        if tree.focused_id() != Some(id.as_str())
            || !tree.frame().regions.iter().any(|region| region.id == id)
        {
            return;
        }
        self.pending_text_paste = Some(TextPasteRequest {
            id,
            scope: TextPasteScope::from_view(&self.view),
            requested_at: Instant::now(),
        });
        // egui-winit reads its platform clipboard and delivers Event::Paste
        // on the next frame. This also works with the Wayland clipboard.
        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }

    pub(super) fn expire_text_paste_request(&mut self) {
        // An empty/non-text clipboard produces no event. Do not capture a
        // later ordinary keyboard paste as the response to that old request.
        if self
            .pending_text_paste
            .as_ref()
            .is_some_and(|request| request.requested_at.elapsed() >= Duration::from_secs(1))
        {
            self.pending_text_paste = None;
        }
    }

    pub(super) fn route_text_paste(
        &mut self,
        event: UiEvent,
        command_center: bool,
    ) -> Option<UiEvent> {
        let UiEvent::Paste(text) = event else {
            return Some(event);
        };
        let Some(request) = self.pending_text_paste.take() else {
            return Some(UiEvent::Paste(text));
        };
        let tree = if command_center {
            &self.command_tree
        } else {
            &self.tree
        };
        if request.scope != TextPasteScope::from_view(&self.view)
            || request.scope.command_center != command_center
            || tree.focused_id() != Some(request.id.as_str())
            || !tree
                .frame()
                .regions
                .iter()
                .any(|region| region.id == request.id)
        {
            return None;
        }
        Some(UiEvent::PasteTo {
            id: request.id,
            text,
        })
    }

    pub(super) fn handle_targeted_terminal_paste(
        &mut self,
        event: &UiEvent,
        ctx: &egui::Context,
    ) -> bool {
        let UiEvent::PasteTo { id, text } = event else {
            return false;
        };
        if id != "terminal_screen" && !id.starts_with("terminal_screen:") {
            return false;
        }
        if *id != self.active_terminal_screen_id() {
            return true;
        }
        if self.tree.focused_id() == Some(id.as_str())
            && self
                .tree
                .frame()
                .regions
                .iter()
                .any(|region| region.id == *id)
            && self.view.show_terminal_panel
            && self
                .view
                .terminal_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.running)
        {
            self.handle_terminal_event(&UiEvent::Paste(text.clone()), ctx);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_menu_paste_keeps_selection_and_rejects_another_conversation() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.tree = UiTree::new(openide_ui::Element::text_area("prompt", "draft", ""));
        app.tree.layout(Size::new(400.0, 100.0));
        app.tree.focus("prompt");
        app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::A,
            shift: false,
            ctrl: true,
        });
        let selection = app.tree.input_selection("prompt");
        let ctx = egui::Context::default();
        app.request_text_paste("prompt".into(), &ctx);
        assert_eq!(app.tree.input_selection("prompt"), selection);
        let event = app
            .route_text_paste(UiEvent::Paste("nuevo ñ🦀".into()), false)
            .unwrap();
        app.tree.handle_event(event);
        assert_eq!(app.tree.input_value("prompt"), Some("nuevo ñ🦀"));
        app.request_text_paste("prompt".into(), &ctx);
        app.view.selected_session = Some("another-conversation".into());
        assert!(app
            .route_text_paste(UiEvent::Paste("must not enter".into()), false)
            .is_none());
        assert_eq!(app.tree.input_value("prompt"), Some("nuevo ñ🦀"));
    }

    #[test]
    fn terminal_menu_paste_rejects_another_terminal_in_the_same_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.terminal_tabs = vec![crate::view::DockTabView {
            id: 11,
            title: "Fixture".into(),
            full_path: None,
            kind: SideTab::Terminal,
            active: true,
            closable: true,
        }];
        app.tree = UiTree::new(openide_ui::Element::button("terminal_screen", "fixture"));
        app.tree.layout(Size::new(400.0, 100.0));
        assert!(app.tree.focus("terminal_screen"));
        app.request_text_paste("terminal_screen".into(), &egui::Context::default());
        assert!(app.pending_text_paste.is_some());
        app.view.terminal_tabs[0].id = 12;
        assert!(app
            .route_text_paste(UiEvent::Paste("wrong shell".into()), false)
            .is_none());
    }

    #[test]
    fn menu_paste_rejects_a_replaced_editor_document_in_the_same_tab() {
        use openide_ui::Component;
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        let make = |text| {
            openide_ui::EditorController::new(openide_editor::TextBuffer::new(text).unwrap())
        };
        let first = make("original");
        let replacement = make("replacement");
        let editor_id = crate::code_editor_ui::view_id(7);
        app.view.code_editor = Some(crate::code_editor_ui::CodeEditorView {
            conflict: false,
            recovered: false,
            missing: false,
            tab_id: 7,
            path: "example.rs".into(),
            controller: Some(first.clone()),
            loading: false,
            saving: false,
            error: None,
            notice: None,
        });
        app.tree = UiTree::new(openide_ui::CodeEditor::new(&editor_id, first).render());
        app.tree.layout(Size::new(400.0, 100.0));
        app.tree.focus(&editor_id);
        app.request_text_paste(editor_id.clone(), &egui::Context::default());
        app.view.code_editor.as_mut().unwrap().controller = Some(replacement.clone());
        app.tree
            .set_root(openide_ui::CodeEditor::new(&editor_id, replacement).render());
        app.tree.layout(Size::new(400.0, 100.0));
        assert!(app
            .route_text_paste(UiEvent::Paste("late".into()), false)
            .is_none());
    }

    #[test]
    fn expired_empty_clipboard_request_does_not_capture_keyboard_paste() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.pending_text_paste = Some(TextPasteRequest {
            id: "old".into(),
            scope: TextPasteScope::from_view(&app.view),
            requested_at: Instant::now() - Duration::from_secs(2),
        });
        app.expire_text_paste_request();
        assert_eq!(
            app.route_text_paste(UiEvent::Paste("keyboard".into()), false),
            Some(UiEvent::Paste("keyboard".into()))
        );
    }
}
