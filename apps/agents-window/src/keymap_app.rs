//! Context-aware dispatch, before native keys are translated for widgets/PTYs.
use super::*;
use crate::keymap::{KeymapProfile, ShortcutAction};

impl AgentsWindow {
    fn vim_owns_native_key(&self, event: &egui::Event) -> bool {
        let egui::Event::Key { key, modifiers, .. } = event else {
            return false;
        };
        if !self.view.vim_mode
            || !modifiers.ctrl
            || modifiers.shift
            || modifiers.alt
            || modifiers.mac_cmd
        {
            return false;
        }
        let key = match key {
            egui::Key::B => openide_ui::Key::B,
            egui::Key::D => openide_ui::Key::D,
            egui::Key::F => openide_ui::Key::F,
            egui::Key::J => openide_ui::Key::J,
            egui::Key::N => openide_ui::Key::N,
            egui::Key::P => openide_ui::Key::P,
            egui::Key::R => openide_ui::Key::R,
            egui::Key::U => openide_ui::Key::U,
            _ => return false,
        };
        self.tree.focused_code_editor_vim_consumes(key, true)
    }

    pub(super) fn apply_keymap_settings_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if id == "settings_keymap_dismiss" {
            self.view.settings_keymap_menu_open = false;
            return true;
        }
        if id != "settings_keymap_toggle"
            && id != "settings_vim_toggle"
            && !id.starts_with("settings_keymap:")
        {
            return false;
        }
        if !self.view.show_settings || self.view.settings_page != SettingsPage::Shortcuts {
            return true;
        }
        if id == "settings_vim_toggle" {
            let mut next = self.settings_preferences.clone();
            next.vim_mode = !next.vim_mode;
            let enabled = next.vim_mode;
            if self.commit_settings_preferences(next, "Modo de edición guardado") {
                self.view.vim_mode = enabled;
            }
            return true;
        }
        if id == "settings_keymap_toggle" {
            self.view.settings_keymap_menu_open = !self.view.settings_keymap_menu_open;
            return true;
        }
        self.view.settings_keymap_menu_open = false;
        if let Some(profile) = id
            .strip_prefix("settings_keymap:")
            .and_then(KeymapProfile::from_id)
        {
            let mut next = self.settings_preferences.clone();
            next.keymap_profile = profile;
            if self.commit_settings_preferences(next, "Perfil de atajos guardado") {
                self.view.keymap_profile = profile;
            }
        }
        true
    }

    fn keymap_action_available(&self, action: ShortcutAction) -> bool {
        use ShortcutAction::*;
        let workspace = !self.view.show_settings && self.view.main_page == MainPage::Workspace;
        let ide = self.ide_workspace_active();
        let palette = self.view.show_command_center;
        let terminal_focused = self.view.show_terminal_panel
            && self.tree.focused_id() == Some(self.active_terminal_screen_id().as_str());
        match action {
            CommandPalette => {
                !(terminal_focused && self.view.keymap_profile == KeymapProfile::OpenIde)
            }
            Settings => !palette,
            QuickOpen => workspace,
            ToggleSidebar | ToggleBottomPanel | ToggleTerminal => workspace && !palette,
            NewFile => workspace && !palette && !terminal_focused,
            NewTask => !palette && !terminal_focused,
            Problems => workspace && !palette,
            Files | Search | Git | Extensions | Debug | Output | DebugConsole => ide && !palette,
            ModelPicker => {
                workspace
                    && !palette
                    && !terminal_focused
                    && self.view.selected_backend == "native"
                    && (!self.view.composer_model_options.is_empty()
                        || !self.view.composer_effort_options.is_empty())
            }
            DuplicateLines => {
                workspace
                    && !palette
                    && self.view.code_editor.as_ref().is_some_and(|editor| {
                        self.tree.focused_id()
                            == Some(crate::code_editor_ui::view_id(editor.tab_id).as_str())
                    })
            }
        }
    }

    /// Modals and popovers own input. A compact IDE sidebar is a surface, not
    /// a command menu, so it doesn't disable workbench shortcuts on its own.
    pub(super) fn handle_keymap_shortcuts(
        &mut self,
        ctx: &egui::Context,
        workflow_modal_open: bool,
        text_menu_open: bool,
    ) -> (bool, Option<String>) {
        let state_id = egui::Id::new(("openide_claimed_shortcuts", ctx.viewport_id()));
        let mut claimed = ctx.data_mut(|data| {
            data.get_temp::<std::collections::HashSet<egui::Key>>(state_id)
                .unwrap_or_default()
        });
        let events = ctx.input(|input| input.events.clone());
        let modifier_id = state_id.with("modifiers");
        let mut native_modifiers = ctx
            .data_mut(|data| data.get_temp::<egui::Modifiers>(modifier_id))
            .unwrap_or_else(|| ctx.input(|input| input.modifiers));
        let mut remaining = Vec::with_capacity(events.len());
        let mut changed = if workflow_modal_open
            || text_menu_open
            || self.view.show_command_center
            || self.view.show_settings
        {
            self.dismiss_editor_review_menu()
        } else {
            false
        };
        let mut search_focus = None;
        // Native events can be batched during a slow frame. Preserve their
        // order and claim each shortcut before the remaining keys reach a PTY.
        for event in events {
            if let egui::Event::ModifiersChanged(modifiers) = &event {
                native_modifiers = *modifiers;
            }
            // egui-winit emits Cut instead of Key(X) even with Shift pressed.
            // Recover only the workbench chord; ordinary Cut stays untouched.
            let native_event = if matches!(event, egui::Event::Cut) {
                let modifiers = native_modifiers;
                if modifiers.shift && !modifiers.alt {
                    egui::Event::Key {
                        key: egui::Key::X,
                        physical_key: Some(egui::Key::X),
                        pressed: true,
                        repeat: claimed.contains(&egui::Key::X),
                        modifiers,
                    }
                } else {
                    event.clone()
                }
            } else {
                event.clone()
            };
            match &native_event {
                egui::Event::WindowFocused(false) => claimed.clear(),
                egui::Event::Key {
                    key,
                    pressed: false,
                    ..
                } if claimed.remove(key) => continue,
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: true,
                    ..
                } if claimed.contains(key) => continue,
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    ..
                } => {
                    claimed.remove(key);
                }
                _ => {}
            }
            let profile = self.view.keymap_profile;
            let blocked = workflow_modal_open
                || text_menu_open
                || self.tree.has_open_popover_except("ide_panel_dismiss")
                || self.vim_owns_native_key(&native_event);
            if !blocked {
                if let Some(action) = self.editor_review_shortcut_action(&native_event) {
                    if let egui::Event::Key { key, .. } = &native_event {
                        claimed.insert(*key);
                    }
                    self.apply_editor_review_action(&action, ctx);
                    changed = true;
                    continue;
                }
            }
            let action = if blocked {
                None
            } else {
                ShortcutAction::ALL.into_iter().find(|action| {
                    self.keymap_action_available(*action) && profile.matches(*action, &native_event)
                })
            };
            if let Some(action) = action {
                if let egui::Event::Key { key, .. } = &native_event {
                    claimed.insert(*key);
                }
                search_focus = self.dispatch_keymap_action(action, ctx).or(search_focus);
                if self.view.show_command_center || self.view.show_settings {
                    self.dismiss_editor_review_menu();
                }
                changed = true;
            } else {
                remaining.push(event);
            }
        }
        ctx.input_mut(|input| input.events = remaining);
        ctx.data_mut(|data| data.insert_temp(state_id, claimed));
        ctx.data_mut(|data| data.insert_temp(modifier_id, native_modifiers));
        if changed {
            ctx.request_repaint();
        }
        (changed, search_focus)
    }

    fn dispatch_keymap_action(
        &mut self,
        action: ShortcutAction,
        ctx: &egui::Context,
    ) -> Option<String> {
        let profile = self.view.keymap_profile;
        use ShortcutAction::*;
        let mut search_focus = None;
        match action {
            CommandPalette => {
                if self.view.show_settings {
                    self.view.show_command_center = false;
                    search_focus = Some(self.settings_search_target().to_owned());
                } else if self.view.show_command_center {
                    self.view.show_command_center = false;
                } else {
                    self.open_command_center();
                    if profile != KeymapProfile::OpenIde {
                        self.view.command_scope = Scope::Commands;
                        self.request_command_search();
                    }
                }
            }
            QuickOpen => {
                self.view.ide.sessions_open = false;
                self.view.ide.compact_panel = None;
                self.open_command_center();
                self.view.command_scope = Scope::Files;
                self.request_command_search();
            }
            ToggleSidebar => self.toggle_left_sidebar(),
            ToggleBottomPanel => {
                self.apply_action(
                    UiAction::Clicked {
                        id: "ide_bottom_toggle".into(),
                    },
                    ctx,
                );
            }
            ToggleTerminal => {
                let focused =
                    self.tree.focused_id() == Some(self.active_terminal_screen_id().as_str());
                if self.view.show_terminal_panel && !focused {
                    self.terminal_focus_pending = true;
                } else if self.view.show_terminal_panel && self.ide_workspace_active() {
                    self.apply_action(
                        UiAction::Clicked {
                            id: "ide_bottom_hide".into(),
                        },
                        ctx,
                    );
                } else {
                    self.apply_action(
                        UiAction::Clicked {
                            id: "terminal_toggle".into(),
                        },
                        ctx,
                    );
                }
            }
            NewFile if self.ide_workspace_active() => {
                self.apply_action(
                    UiAction::Clicked {
                        id: "ide_new_file".into(),
                    },
                    ctx,
                );
            }
            NewTask | NewFile => {
                self.new_session();
                if !self.view.running {
                    self.view.show_settings = false;
                    self.visit_navigation();
                }
            }
            Settings => {
                if !self.view.show_settings {
                    self.apply_action(
                        UiAction::Clicked {
                            id: "toggle_settings_header".into(),
                        },
                        ctx,
                    );
                }
            }
            Files | Search | Git | Extensions | Debug => {
                use crate::ide_workspace::IdeActivity;
                let activity = match action {
                    Files => IdeActivity::Files,
                    Search => IdeActivity::Search,
                    Git => IdeActivity::Git,
                    Extensions => IdeActivity::Extensions,
                    _ => IdeActivity::Debug,
                };
                self.reveal_ide_activity(activity, ctx);
            }
            Problems | Output | DebugConsole => {
                use crate::ide_workspace::IdeBottomPanel;
                let panel = match action {
                    Problems => IdeBottomPanel::Problems,
                    Output => IdeBottomPanel::Output,
                    _ => IdeBottomPanel::DebugConsole,
                };
                self.open_ide_bottom(panel, ctx);
            }
            ModelPicker => {
                let open =
                    !(self.view.composer_model_menu_open || self.view.composer_effort_menu_open);
                self.close_composer_floating_controls();
                self.view.composer_model_menu_open = open;
                self.view.composer_model_list_open = false;
                self.view.composer_model_query.clear();
                self.composer_model_search_focus_pending = false;
                self.view.composer_advanced_open = false;
                self.view.composer_effort_menu_open = false;
                self.view.composer_effort_preview = None;
                self.view.backend_menu_open = false;
            }
            DuplicateLines => {
                self.apply_editor_workbench_action(
                    &UiAction::Clicked {
                        id: "editor_duplicate".into(),
                    },
                    ctx,
                );
            }
        }
        search_focus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::{IdeBottomPanel, WindowMode};

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.ide.mode = WindowMode::Ide;
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        (root, app)
    }

    fn dispatch(
        app: &mut AgentsWindow,
        key: egui::Key,
        shift: bool,
        alt: bool,
        modal: bool,
        menu: bool,
    ) -> (bool, Vec<egui::Event>) {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: !cfg!(target_os = "macos"),
                    mac_cmd: cfg!(target_os = "macos"),
                    command: true,
                    shift,
                    alt,
                },
            }],
            ..Default::default()
        });
        let handled = app.handle_keymap_shortcuts(&ctx, modal, menu).0;
        let remaining = ctx.input(|input| input.events.clone());
        ctx.end_pass().textures_delta.clear();
        (handled, remaining)
    }

    #[test]
    fn ctrl_j_restores_each_bottom_section_without_allocating_a_terminal() {
        let (_root, mut app) = fixture();
        let scope = app.dock_scope();
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            app.view.ide.mode = mode;
            for panel in [
                IdeBottomPanel::Problems,
                IdeBottomPanel::Output,
                IdeBottomPanel::DebugConsole,
            ] {
                if mode == WindowMode::Agents && panel != IdeBottomPanel::Problems {
                    continue;
                }
                app.view.ide.bottom_panel = panel;
                app.view.ide.bottom_open = false;
                let (handled, events) =
                    dispatch(&mut app, egui::Key::J, false, false, false, false);
                assert!(handled && events.is_empty());
                assert!(app.view.ide.bottom_open);
                assert_eq!(app.view.ide.bottom_panel, panel);
                assert!(!app.view.show_terminal_panel);
                assert!(app.dock.active_terminal(&scope).is_none());
                assert!(dispatch(&mut app, egui::Key::J, false, false, false, false).0);
                assert!(!app.view.ide.bottom_open);
                assert_eq!(app.view.ide.bottom_panel, panel);
            }
        }
    }

    #[test]
    fn problems_shortcut_opens_from_each_focused_surface_in_both_windows() {
        use openide_ui::{CodeEditor, Element, Size, UiTree};
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            for surface in ["none", "prompt", "editor", "terminal"] {
                let (root, mut app) = fixture();
                app.view.ide.mode = mode;
                app.view.keymap_profile = KeymapProfile::VsCode;
                let workspace = root.path().join("workspace");
                std::fs::create_dir(&workspace).unwrap();
                app.view.workspace = workspace.to_string_lossy().into_owned();
                app.view.prompt = "Borrador intacto".into();
                let scope = app.dock_scope();
                let mut controller_before = None;
                let (element, focus) = match surface {
                    "prompt" => (
                        Element::text_input("prompt", &app.view.prompt, "Task"),
                        Some("prompt".to_owned()),
                    ),
                    "editor" => {
                        std::fs::write(workspace.join("file.rs"), "fn main() {}\n").unwrap();
                        let tab = app.dock.open(
                            &scope,
                            DockTabKind::FilePreview {
                                path: "file.rs".into(),
                            },
                        );
                        app.dock.set_collapsed(&scope, false);
                        app.sync_dock_view();
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while app
                            .view
                            .code_editor
                            .as_ref()
                            .is_none_or(|view| view.loading)
                        {
                            app.poll_code_editor(&egui::Context::default());
                            assert!(Instant::now() < deadline, "editor fixture did not load");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        let (_, _, controller, snapshot) = app.active_editor().unwrap();
                        controller_before = Some((controller.clone(), snapshot));
                        let id = crate::code_editor_ui::view_id(tab.get());
                        (
                            Element::code_editor(CodeEditor::new(&id, controller)),
                            Some(id),
                        )
                    }
                    "terminal" => {
                        let terminal = app.dock.new_terminal(&scope);
                        // Exercise terminal focus and identity without starting a PTY.
                        app.terminal_controls_host
                            .errors
                            .insert(terminal.get(), "Fixture sin proceso".into());
                        app.sync_dock_view();
                        assert!(app.view.show_terminal_panel);
                        let id = app.active_terminal_screen_id();
                        (Element::text_input(&id, "", "Terminal"), Some(id))
                    }
                    _ => (Element::spacer(), None),
                };
                app.tree = UiTree::new(element);
                app.tree.layout(Size::new(800.0, 500.0));
                if let Some(id) = focus {
                    assert!(app.tree.focus(&id));
                }
                let terminals_before = app
                    .dock
                    .terminal_tabs(&scope)
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>();
                for _ in 0..2 {
                    let (handled, events) =
                        dispatch(&mut app, egui::Key::M, true, false, false, false);
                    assert!(handled && events.is_empty(), "{mode:?} / {surface}");
                    assert!(app.view.ide.bottom_open);
                    assert_eq!(app.view.ide.bottom_panel, IdeBottomPanel::Problems);
                    assert!(!app.view.show_terminal_panel);
                }
                for open in [false, true] {
                    let (handled, events) =
                        dispatch(&mut app, egui::Key::J, false, false, false, false);
                    assert!(handled && events.is_empty());
                    assert_eq!(app.view.ide.bottom_open, open);
                    assert_eq!(app.view.ide.bottom_panel, IdeBottomPanel::Problems);
                    assert!(!app.view.show_terminal_panel);
                }
                assert_eq!(
                    app.dock
                        .terminal_tabs(&scope)
                        .iter()
                        .map(|tab| tab.id)
                        .collect::<Vec<_>>(),
                    terminals_before,
                );
                assert!(app.terminals.is_empty());
                assert_eq!(app.view.prompt, "Borrador intacto");
                if let Some((controller, before)) = controller_before {
                    let after = controller.snapshot().unwrap();
                    assert_eq!(after.document_id(), before.document_id());
                    assert_eq!(after.revision(), before.revision());
                    assert_eq!(after.selection(), before.selection());
                } else {
                    assert!(app.view.code_editor.is_none());
                }
            }
        }
    }

    #[test]
    fn batched_shortcuts_and_held_keys_never_leak_to_the_next_focused_surface() {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        app.view.ide.bottom_panel = IdeBottomPanel::Problems;
        app.view.ide.bottom_open = false;
        let press = |key, repeat| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat,
            modifiers: egui::Modifiers {
                ctrl: !cfg!(target_os = "macos"),
                mac_cmd: cfg!(target_os = "macos"),
                command: true,
                ..Default::default()
            },
        };
        let sidebar = app.view.ide.explorer_open;
        ctx.begin_pass(egui::RawInput {
            events: vec![press(egui::Key::J, false), press(egui::Key::B, false)],
            ..Default::default()
        });
        assert!(app.handle_keymap_shortcuts(&ctx, false, false).0);
        assert!(ctx.input(|input| input.events.is_empty()));
        assert!(app.view.ide.bottom_open);
        assert_ne!(app.view.ide.explorer_open, sidebar);
        ctx.end_pass().textures_delta.clear();
        // Held Ctrl+J must not retoggle, nor become a LF in a newly focused PTY.
        // Simulate a menu opening while the key remains down, too.
        ctx.begin_pass(egui::RawInput {
            events: vec![press(egui::Key::J, true)],
            ..Default::default()
        });
        assert!(!app.handle_keymap_shortcuts(&ctx, true, false).0);
        assert!(ctx.input(|input| input.events.is_empty()));
        assert!(app.view.ide.bottom_open);
        ctx.end_pass().textures_delta.clear();
        let mut release = press(egui::Key::J, false);
        if let egui::Event::Key { pressed, .. } = &mut release {
            *pressed = false;
        }
        ctx.begin_pass(egui::RawInput {
            events: vec![release, press(egui::Key::J, false)],
            ..Default::default()
        });
        assert!(app.handle_keymap_shortcuts(&ctx, false, false).0);
        assert!(ctx.input(|input| input.events.is_empty()));
        assert!(!app.view.ide.bottom_open);
        ctx.end_pass().textures_delta.clear();
    }

    #[test]
    fn model_picker_and_problems_do_not_share_chords_in_modern_profiles() {
        let (_root, mut app) = fixture();
        app.view.selected_backend = "native".into();
        app.view.composer_model_options = vec![("example".into(), "Example".into())];
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            app.view.ide.mode = mode;
            for profile in [KeymapProfile::VsCode, KeymapProfile::Zed] {
                app.view.keymap_profile = profile;
                app.view.composer_model_menu_open = false;
                assert!(dispatch(&mut app, egui::Key::M, true, false, false, false).0);
                assert_eq!(app.view.ide.bottom_panel, IdeBottomPanel::Problems);
                assert!(app.view.ide.bottom_open);
                assert!(!app.view.composer_model_menu_open);
                app.view.ide.bottom_open = false;
                assert!(dispatch(&mut app, egui::Key::M, false, true, false, false).0);
                assert!(app.view.composer_model_menu_open);
                assert!(!app.view.ide.bottom_open);
            }
            app.view.composer_model_menu_open = false;
            app.view.keymap_profile = KeymapProfile::OpenIde;
            assert!(dispatch(&mut app, egui::Key::M, true, false, false, false).0);
            assert!(app.view.composer_model_menu_open);
            assert!(!app.view.ide.bottom_open);
        }
    }

    #[test]
    fn modals_and_text_menus_keep_their_keys_and_do_not_open_panels() {
        let (_root, mut app) = fixture();
        app.view.ide.bottom_open = false;
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            app.view.ide.mode = mode;
            for (modal, menu) in [(true, false), (false, true)] {
                for (key, shift) in [(egui::Key::J, false), (egui::Key::M, true)] {
                    let (handled, remaining) = dispatch(&mut app, key, shift, false, modal, menu);
                    assert!(!handled);
                    assert_eq!(remaining.len(), 1);
                    assert!(!app.view.ide.bottom_open && !app.view.show_terminal_panel);
                }
            }
        }
    }

    #[test]
    fn agents_problems_shortcut_does_not_enable_ide_only_sections_or_escape_overlays() {
        let (_root, mut app) = fixture();
        app.view.ide.mode = WindowMode::Agents;
        app.view.keymap_profile = KeymapProfile::VsCode;
        assert!(app.keymap_action_available(ShortcutAction::Problems));
        for action in [
            ShortcutAction::Files,
            ShortcutAction::Search,
            ShortcutAction::Git,
            ShortcutAction::Extensions,
            ShortcutAction::Debug,
            ShortcutAction::Output,
            ShortcutAction::DebugConsole,
        ] {
            assert!(!app.keymap_action_available(action));
        }
        for (settings, palette, page) in [
            (true, false, MainPage::Workspace),
            (false, true, MainPage::Workspace),
            (false, false, MainPage::Marketplace),
        ] {
            app.view.show_settings = settings;
            app.view.show_command_center = palette;
            app.view.main_page = page;
            let (handled, events) = dispatch(&mut app, egui::Key::M, true, false, false, false);
            assert!(!handled && events.len() == 1);
            assert!(!app.view.ide.bottom_open && !app.view.show_terminal_panel);
        }
    }

    #[test]
    fn native_shift_cut_opens_extensions_and_plain_cut_is_preserved() {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        for shift in [false, true] {
            ctx.begin_pass(egui::RawInput {
                events: vec![
                    egui::Event::ModifiersChanged(egui::Modifiers {
                        ctrl: !cfg!(target_os = "macos"),
                        mac_cmd: cfg!(target_os = "macos"),
                        command: true,
                        shift,
                        ..Default::default()
                    }),
                    egui::Event::Cut,
                    egui::Event::ModifiersChanged(Default::default()),
                ],
                ..Default::default()
            });
            assert_eq!(app.handle_keymap_shortcuts(&ctx, false, false).0, shift);
            assert_eq!(
                ctx.input(|input| !input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Cut))),
                shift
            );
            ctx.end_pass().textures_delta.clear();
        }
        assert_eq!(
            app.view.ide.activity,
            crate::ide_workspace::IdeActivity::Extensions
        );
    }

    #[test]
    fn palette_and_file_picker_use_distinct_scopes_in_both_windows() {
        let (_root, mut app) = fixture();
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            app.view.ide.mode = mode;
            app.view.show_command_center = false;
            assert!(dispatch(&mut app, egui::Key::P, true, false, false, false).0);
            assert!(app.view.show_command_center);
            assert_eq!(app.view.command_scope, Scope::Commands);
            assert!(dispatch(&mut app, egui::Key::P, false, false, false, false).0);
            assert_eq!(app.view.command_scope, Scope::Files);
            assert!(dispatch(&mut app, egui::Key::P, true, false, false, false).0);
            assert!(!app.view.show_command_center);
        }
    }

    #[test]
    fn settings_actions_save_profile_and_vim_independently_and_reject_stale_controls() {
        let (root, mut app) = fixture();
        let choose = UiAction::Clicked {
            id: "settings_keymap:zed".into(),
        };
        assert!(app.apply_keymap_settings_action(&choose));
        assert_eq!(app.view.keymap_profile, KeymapProfile::VsCode);
        app.view.show_settings = true;
        app.view.settings_page = SettingsPage::Shortcuts;
        assert!(app.apply_keymap_settings_action(&choose));
        assert!(app.apply_keymap_settings_action(&UiAction::Clicked {
            id: "settings_vim_toggle".into()
        }));
        let saved =
            crate::settings_preferences::SettingsPreferences::load(&root.path().join("data"));
        assert_eq!(saved.keymap_profile, KeymapProfile::Zed);
        assert!(saved.vim_mode && app.view.vim_mode);
        assert!(app.apply_keymap_settings_action(&UiAction::Clicked {
            id: "settings_keymap:openide".into()
        }));
        assert!(app.view.vim_mode);
        assert_eq!(app.view.keymap_profile, KeymapProfile::OpenIde);
    }

    #[test]
    fn vim_navigation_has_priority_only_in_the_focused_modal_editor() {
        use openide_ui::{CodeEditor, EditorController, Element, Size, UiTree};
        let (_root, mut app) = fixture();
        app.view.vim_mode = true;
        let controller =
            EditorController::new(openide_editor::TextBuffer::new("first\nsecond\n").unwrap());
        app.tree = UiTree::new(Element::code_editor(
            CodeEditor::new("modal_editor", controller.clone()).vim_mode(true),
        ));
        app.tree.layout(Size::new(800.0, 500.0));
        assert!(app.tree.focus("modal_editor"));
        let (handled, events) = dispatch(&mut app, egui::Key::J, false, false, false, false);
        assert!(!handled);
        assert_eq!(events.len(), 1);
        assert!(!app.view.ide.bottom_open && !app.view.show_terminal_panel);
        app.tree.handle_event(UiEvent::Key {
            key: openide_ui::Key::J,
            ctrl: true,
            shift: false,
        });
        assert_eq!(controller.snapshot().unwrap().selection().head, 6);
        app.tree.handle_event(UiEvent::Text("i".into()));
        app.view.ide.bottom_panel = IdeBottomPanel::Problems;
        assert!(dispatch(&mut app, egui::Key::J, false, false, false, false).0);
        assert!(app.view.ide.bottom_open);
    }
}
