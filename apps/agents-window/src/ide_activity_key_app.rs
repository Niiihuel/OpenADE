//! Familiar activity shortcuts, without stealing editor keys from popovers or
//! dialogs. Focus happens after the new sidebar has been laid out.
use super::*;
use crate::ide_workspace::{IdeActivity, IdePanel};

impl AgentsWindow {
    pub(super) fn focus_ide_activity_if_pending(&mut self) -> bool {
        let Some(target) = self.view.ide.activity_focus.clone() else {
            return false;
        };
        let requested = self.view.ide.explorer_open
            && (crate::ide_workspace::nominal_layout(&self.view, self.viewport_width).left > 0.0
                || self.view.ide.compact_panel == Some(IdePanel::Explorer));
        let expected = match self.view.ide.activity {
            IdeActivity::Search => Some("ide_search_query"),
            IdeActivity::Extensions => Some("ide_extensions_search"),
            _ => None,
        };
        if !self.ide_workspace_active()
            || !requested
            || expected != Some(target.as_str())
            || self.view.show_command_center
            || self.view.ide.new_file.is_some()
            || self.tree.context_menu_open()
            || self.tree.has_open_popover_except("ide_panel_dismiss")
        {
            self.view.ide.activity_focus = None;
            return false;
        }
        // An opening panel starts at zero width. Keep the request until its
        // retained input has room for both the icon and editable text, rather
        // than focusing a clipped sliver or dropping the request on frame one.
        let usable_input = self.tree.frame().regions.iter().any(|region| {
            region.id == target
                && region.rect.width >= crate::sidebar_chrome::TOOL_SIZE * 2.0
                && region.rect.height >= crate::sidebar_chrome::TOOL_SIZE
        });
        if !usable_input {
            return false;
        }
        if self.tree.focus(&target) {
            self.view.ide.activity_focus = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Key;
    use openide_ui::Size;

    fn fixture(width: f32) -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.viewport_width = width;
        app.viewport_height = 700.0;
        (root, app)
    }

    fn dispatch_shortcut(app: &mut AgentsWindow, key: Key, ctx: &egui::Context) -> bool {
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: !cfg!(target_os = "macos"),
                    command: true,
                    mac_cmd: cfg!(target_os = "macos"),
                    shift: true,
                    alt: false,
                },
            }],
            ..Default::default()
        });
        let handled = app
            .handle_keymap_shortcuts(
                ctx,
                app.view.ide.new_file.is_some(),
                app.tree.context_menu_open(),
            )
            .0;
        ctx.end_pass().textures_delta.clear();
        // Deliver the real release before the next independent shortcut.
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        });
        app.handle_keymap_shortcuts(ctx, false, false);
        ctx.end_pass().textures_delta.clear();
        handled
    }

    fn layout(app: &mut AgentsWindow) {
        app.tree.set_root(crate::view::build_window_view(
            &app.view,
            app.viewport_width,
            app.viewport_height,
        ));
        app.tree
            .layout(Size::new(app.viewport_width, app.viewport_height));
    }

    #[test]
    fn ide_activity_search_shortcut_reveals_instead_of_collapsing_and_focuses_input() {
        let (_root, mut app) = fixture(1440.0);
        let ctx = egui::Context::default();
        for _ in 0..2 {
            assert!(dispatch_shortcut(&mut app, Key::F, &ctx));
            assert_eq!(app.view.ide.activity, IdeActivity::Search);
            assert!(app.view.ide.explorer_open);
            layout(&mut app);
            assert!(app.focus_ide_activity_if_pending());
            assert_eq!(app.tree.focused_id(), Some("ide_search_query"));
        }
        assert!(app
            .tree
            .handle_event(UiEvent::Text("needle".into()))
            .contains(&UiAction::InputChanged {
                id: "ide_search_query".into(),
                value: "needle".into(),
            }));
        assert!(app
            .tree
            .handle_event(UiEvent::Key {
                key: openide_ui::Key::Enter,
                ctrl: false,
                shift: false
            })
            .contains(&UiAction::Submitted {
                id: "ide_search_query".into(),
                value: "needle".into()
            }));
    }

    #[test]
    fn ide_activity_click_focuses_compact_search_drawer_without_stealing_menu_focus() {
        let (_root, mut app) = fixture(320.0);
        app.apply_ide_activity_action(
            &UiAction::Clicked {
                id: "ide_activity:search".into(),
            },
            &egui::Context::default(),
        );
        layout(&mut app);
        assert!(app.tree.has_open_popover());
        assert!(app.focus_ide_activity_if_pending());
        assert_eq!(app.tree.focused_id(), Some("ide_search_query"));
        assert!(dispatch_shortcut(
            &mut app,
            Key::G,
            &egui::Context::default()
        ));
        assert_eq!(app.view.ide.activity, IdeActivity::Git);
    }

    #[test]
    fn activity_focus_waits_for_a_usable_opening_panel_and_cancels_when_closed() {
        for (activity, target) in [
            (IdeActivity::Search, "ide_search_query"),
            (IdeActivity::Extensions, "ide_extensions_search"),
        ] {
            let (_root, mut app) = fixture(1440.0);
            app.view.ide.explorer_open = false;
            app.select_ide_activity(activity, false);
            for width in [0.0, 16.0, 40.0] {
                app.view.left_presentation_width = Some(width);
                layout(&mut app);
                assert!(!app.tree.has_open_popover());
                assert!(!app.focus_ide_activity_if_pending(), "width {width}");
                assert_eq!(app.view.ide.activity_focus.as_deref(), Some(target));
            }
            app.view.left_presentation_width = Some(180.0);
            layout(&mut app);
            assert!(app.focus_ide_activity_if_pending());
            assert_eq!(app.tree.focused_id(), Some(target));
            assert!(app.view.ide.activity_focus.is_none());

            app.select_ide_activity(activity, false);
            app.view.left_presentation_width = Some(60.0);
            app.toggle_ide_panel(IdePanel::Explorer);
            layout(&mut app);
            assert!(!app.focus_ide_activity_if_pending());
            assert!(app.view.ide.activity_focus.is_none());
        }
    }

    #[test]
    fn composer_focus_waits_for_the_opening_agent_and_cancels_during_close() {
        let (_root, mut app) = fixture(1440.0);
        app.view.prompt = "Borrador del agente".into();
        // with_data_root has already seeded the retained input. Match
        // restore_draft's synchronization instead of replacing only the view.
        app.tree.set_input_value("prompt", app.view.prompt.clone());
        app.view.ide.agent_open = true;
        app.view.ide.agent_content_width = Some(380.0);
        app.prompt_focus_pending = true;
        app.composer_cursor_pending = Some(3);
        for width in [0.0, 8.0, 32.0] {
            app.view.right_presentation_width = Some(width);
            layout(&mut app);
            assert!(app.tree.focus("ide_agent_toggle"));
            app.focus_prompt_if_pending();
            assert!(app.prompt_focus_pending, "width {width}");
            assert_eq!(app.composer_cursor_pending, Some(3));
            assert_eq!(app.tree.focused_id(), Some("ide_agent_toggle"));
        }
        app.view.right_presentation_width = Some(180.0);
        layout(&mut app);
        app.focus_prompt_if_pending();
        assert!(!app.prompt_focus_pending);
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert_eq!(app.tree.input_cursor("prompt"), Some(3));

        app.prompt_focus_pending = true;
        app.composer_cursor_pending = Some(5);
        app.toggle_ide_panel(IdePanel::Agent);
        app.view.right_presentation_width = Some(120.0);
        layout(&mut app);
        // The mounted composer is still focusable in UiTree during clipping;
        // the host must honor the close request instead of stealing focus.
        assert!(app.tree.focus("prompt"));
        assert!(app.tree.focus("ide_agent_toggle"));
        app.focus_prompt_if_pending();
        assert!(!app.prompt_focus_pending);
        assert!(app.composer_cursor_pending.is_none());
        assert_eq!(app.tree.focused_id(), Some("ide_agent_toggle"));
    }

    #[test]
    fn composer_focus_supports_the_compact_drawer_and_agents_window() {
        let (_root, mut app) = fixture(520.0);
        app.view.prompt = "Borrador compartido".into();
        app.tree.set_input_value("prompt", app.view.prompt.clone());
        app.view.ide.agent_open = false;
        app.reveal_ide_agent();
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Agent));
        app.prompt_focus_pending = true;
        app.composer_cursor_pending = Some(4);
        layout(&mut app);
        app.focus_prompt_if_pending();
        assert!(!app.prompt_focus_pending);
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert_eq!(app.tree.input_cursor("prompt"), Some(4));

        app.prompt_focus_pending = true;
        app.apply_ide_workspace_action(&UiAction::Clicked {
            id: "ide_panel_dismiss".into(),
        });
        layout(&mut app);
        assert!(app.tree.focus("ide_agent_toggle"));
        app.focus_prompt_if_pending();
        assert!(!app.prompt_focus_pending);
        assert_eq!(app.tree.focused_id(), Some("ide_agent_toggle"));

        app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;
        app.view.ide.agent_open = false;
        app.prompt_focus_pending = true;
        app.composer_cursor_pending = Some(2);
        layout(&mut app);
        app.focus_prompt_if_pending();
        assert!(!app.prompt_focus_pending);
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert_eq!(app.tree.input_cursor("prompt"), Some(2));
    }

    #[test]
    fn ide_activity_shortcuts_respect_classic_profile_and_modal_navigation() {
        let (_root, mut app) = fixture(1440.0);
        let ctx = egui::Context::default();
        app.view.keymap_profile = crate::keymap::KeymapProfile::OpenIde;
        assert!(!dispatch_shortcut(&mut app, Key::D, &ctx));
        app.view.show_command_center = true;
        assert!(!dispatch_shortcut(&mut app, Key::F, &ctx));
        app.view.show_command_center = false;
        app.view.ide.mode = crate::ide_workspace::WindowMode::Agents;
        assert!(!dispatch_shortcut(&mut app, Key::F, &ctx));
        assert_eq!(app.view.ide.activity, IdeActivity::Files);
    }
}
