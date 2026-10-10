//! Explicit terminal actions and launch identity. Menu projection performs no
//! executable discovery; profiles are resolved only on an explicit menu action.
use super::*;
use crate::terminal_dock_ui::{TerminalProfileView, TerminalRenameView};

#[derive(Default)]
pub(super) struct TerminalControlsHost {
    profiles: HashMap<u64, TerminalShell>,
    names: HashMap<u64, String>,
    splits: HashMap<DockScope, [u64; 2]>,
    pub(super) errors: HashMap<u64, String>,
    menu_scope: Option<DockScope>,
    rename_scope: Option<DockScope>,
}

fn shell_label(shell: &TerminalShell) -> String {
    match shell {
        TerminalShell::System => "Shell del sistema".into(),
        TerminalShell::PowerShell => "PowerShell".into(),
        TerminalShell::PowerShellCore => "PowerShell 7".into(),
        TerminalShell::CommandPrompt => "Símbolo del sistema".into(),
        TerminalShell::Bash => "Bash".into(),
        TerminalShell::Zsh => "Zsh".into(),
        TerminalShell::Fish => "Fish".into(),
        TerminalShell::Custom(path) => path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned(),
    }
}

fn named_profile(id: &str, configured: &TerminalShell) -> Option<TerminalShell> {
    Some(match id {
        "default" => configured.clone(),
        "system" => TerminalShell::System,
        "bash" => TerminalShell::Bash,
        "zsh" => TerminalShell::Zsh,
        "fish" => TerminalShell::Fish,
        "powershell" if cfg!(windows) => TerminalShell::PowerShell,
        "pwsh" => TerminalShell::PowerShellCore,
        "cmd" if cfg!(windows) => TerminalShell::CommandPrompt,
        _ => return None,
    })
}

impl AgentsWindow {
    pub(super) fn terminal_profile_for(&mut self, id: u64) -> TerminalShell {
        self.terminal_controls_host
            .profiles
            .entry(id)
            .or_insert_with(|| self.settings_preferences.terminal_shell.clone())
            .clone()
    }

    pub(super) fn terminal_title_for(&self, id: u64, index: usize) -> String {
        if let Some(name) = self.terminal_controls_host.names.get(&id) {
            return name.clone();
        }
        if let Some(session) = self.terminals.get(&id) {
            return format!("{} {}", session.shell_label(), index + 1);
        }
        let label = self
            .terminal_controls_host
            .profiles
            .get(&id)
            .filter(|profile| **profile != TerminalShell::System)
            .map(shell_label)
            .unwrap_or_else(|| "Terminal".to_owned());
        format!("{label} {}", index + 1)
    }

    pub(super) fn refresh_terminal_titles(&mut self) {
        for index in 0..self.view.terminal_tabs.len() {
            let id = self.view.terminal_tabs[index].id;
            self.view.terminal_tabs[index].title = self.terminal_title_for(id, index);
        }
    }

    pub(super) fn sync_terminal_controls_view(&mut self) {
        self.terminal_controls_host
            .profiles
            .retain(|id, _| self.dock.contains_tab(*id));
        self.terminal_controls_host
            .names
            .retain(|id, _| self.dock.contains_tab(*id));
        self.terminal_controls_host
            .errors
            .retain(|id, _| self.dock.contains_tab(*id));
        // Draft promotion preserves DockTabIds. Follow that verified ownership
        // transfer, not merely the newly selected task name.
        let current_scope = self.dock_scope();
        let draft_scope = DockScope::for_owner(Path::new(&self.view.workspace), "");
        if current_scope != draft_scope {
            if let Some(pair) = self
                .terminal_controls_host
                .splits
                .get(&draft_scope)
                .copied()
            {
                let tabs = self.dock.terminal_tabs(&current_scope);
                if pair
                    .iter()
                    .all(|id| tabs.iter().any(|tab| tab.id.get() == *id))
                {
                    self.terminal_controls_host.splits.remove(&draft_scope);
                    self.terminal_controls_host
                        .splits
                        .entry(current_scope)
                        .or_insert(pair);
                }
            }
        }
        self.terminal_controls_host.splits.retain(|scope, pair| {
            let tabs = self.dock.terminal_tabs(scope);
            pair.iter()
                .all(|id| tabs.iter().any(|tab| tab.id.get() == *id))
        });
        self.refresh_terminal_panes();
        self.view.terminal_controls.sessions_open = false;
        self.view.terminal_controls.profiles_open = false;
        self.terminal_controls_host.menu_scope = None;
        let scope = self.dock_scope();
        if self.terminal_controls_host.rename_scope.as_ref() != Some(&scope)
            || self
                .view
                .terminal_controls
                .rename
                .as_ref()
                .is_some_and(|rename| {
                    !self
                        .view
                        .terminal_tabs
                        .iter()
                        .any(|tab| tab.id == rename.id)
                })
        {
            self.view.terminal_controls.rename = None;
            self.terminal_controls_host.rename_scope = None;
        }
        self.refresh_terminal_titles();
    }

    pub(super) fn close_terminal_controls(&mut self) {
        self.view.terminal_menu_open = false;
        self.view.terminal_controls.sessions_open = false;
        self.view.terminal_controls.profiles_open = false;
        self.view.terminal_controls.rename = None;
        self.terminal_controls_host.menu_scope = None;
        self.terminal_controls_host.rename_scope = None;
    }

    pub(super) fn new_terminal_with_profile(&mut self, profile: TerminalShell) -> u64 {
        self.new_terminal_for(profile, None)
    }

    /// A terminal that runs an agent CLI instead of a shell. The CLI is
    /// recorded before the terminal is shown, because showing it starts it.
    pub(super) fn new_agent_terminal(
        &mut self,
        agent: (&'static crate::agent_cli_definitions::AgentCliDefinition, bool),
    ) -> u64 {
        self.new_terminal_for(TerminalShell::System, Some(agent))
    }

    fn new_terminal_for(
        &mut self,
        profile: TerminalShell,
        agent: Option<(&'static crate::agent_cli_definitions::AgentCliDefinition, bool)>,
    ) -> u64 {
        let scope = self.dock_scope();
        let id = self.dock.new_terminal(&scope).get();
        if let Some(agent) = agent {
            self.mark_agent_terminal(id, agent);
        }
        self.terminal_controls_host.profiles.insert(id, profile);
        self.close_terminal_controls();
        self.sync_dock_view();
        self.terminal_focus_pending = true;
        id
    }

    pub(super) fn terminal_id_in_scope(&self, id: u64) -> bool {
        self.dock
            .terminal_tabs(&self.dock_scope())
            .iter()
            .any(|tab| tab.id.get() == id)
    }

    pub(super) fn visible_terminal_ids(&self) -> Vec<u64> {
        let Some(active) = self.active_terminal_id() else {
            return Vec::new();
        };
        if let Some(pair) = self.terminal_controls_host.splits.get(&self.dock_scope()) {
            if pair.contains(&active) && pair.iter().all(|id| self.terminal_id_in_scope(*id)) {
                return pair.to_vec();
            }
        }
        vec![active]
    }

    pub(super) fn terminal_screen_id(&self, id: u64) -> String {
        crate::terminal_dock_ui::screen_id(id, self.visible_terminal_ids().len() == 2)
    }

    pub(super) fn active_terminal_screen_id(&self) -> String {
        self.active_terminal_id()
            .map(|id| self.terminal_screen_id(id))
            .unwrap_or_else(|| "terminal_screen".into())
    }

    pub(super) fn refresh_terminal_panes(&mut self) {
        let ids = self.visible_terminal_ids();
        self.view.terminal_controls.panes = if ids.len() == 2 {
            ids.into_iter()
                .map(|id| crate::terminal_dock_ui::TerminalPaneView {
                    id,
                    snapshot: self.terminals.get(&id).map(TerminalSession::snapshot),
                    error: self.terminal_controls_host.errors.get(&id).cloned(),
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    /// Pointer focus changes only the active member; it never reorders panes,
    /// restarts the PTY, or rebuilds unrelated editor/browser view state.
    pub(super) fn activate_terminal_pane(&mut self, id: u64) -> bool {
        if !self.visible_terminal_ids().contains(&id) || !self.terminal_id_in_scope(id) {
            return false;
        }
        if self.active_terminal_id() != Some(id) {
            if let Some(previous) = self.active_terminal_id() {
                if let Some(pane) = self
                    .view
                    .terminal_controls
                    .panes
                    .iter_mut()
                    .find(|pane| pane.id == previous)
                {
                    pane.snapshot = self.view.terminal_snapshot.clone();
                    pane.error = self.view.terminal_error.clone();
                }
                if let Some(session) = self.terminals.get_mut(&previous) {
                    session.end_selection();
                }
            }
            self.dock
                .activate(&self.dock_scope(), DockTabId::from_raw(id).unwrap());
            for tab in &mut self.view.terminal_tabs {
                tab.active = tab.id == id;
            }
            self.view.terminal_snapshot = self.terminals.get(&id).map(TerminalSession::snapshot);
            self.view.terminal_error = self.terminal_controls_host.errors.get(&id).cloned();
            self.close_terminal_controls();
        }
        let screen = self.terminal_screen_id(id);
        self.tree.focus(&screen);
        true
    }

    fn split_terminal(&mut self) {
        if !self.view.show_terminal_panel
            || self.view.show_settings
            || self.view.show_command_center
            || self.view.main_page != MainPage::Workspace
        {
            return;
        }
        let Some(first) = self.active_terminal_id() else {
            return;
        };
        if self.visible_terminal_ids().len() == 2 {
            return;
        }
        let scope = self.dock_scope();
        let profile = self.terminal_profile_for(first);
        let second = self.dock.new_terminal(&scope).get();
        self.terminal_controls_host.profiles.insert(second, profile);
        self.terminal_controls_host
            .splits
            .insert(scope, [first, second]);
        self.close_terminal_controls();
        self.sync_dock_view();
        self.terminal_focus_pending = true;
    }

    fn available_terminal_profiles(&self) -> Vec<TerminalProfileView> {
        let configured = &self.settings_preferences.terminal_shell;
        [
            "default",
            "system",
            "bash",
            "zsh",
            "fish",
            "powershell",
            "pwsh",
            "cmd",
        ]
        .into_iter()
        .filter_map(|id| {
            let profile = named_profile(id, configured)?;
            if id != "default" && &profile == configured {
                return None;
            }
            // PATH/custom path validation is confined to this explicit menu
            // request. Never advertise an executable we could not resolve.
            profile.command().ok()?;
            Some(TerminalProfileView {
                id: id.into(),
                label: shell_label(&profile),
                is_default: id == "default",
            })
        })
        .chain(
            crate::agent_cli_definitions::definitions()
                .iter()
                .filter(|agent| self.resolve_agent_cli(agent).is_some())
                .flat_map(|agent| agent.profiles())
                .map(|(id, label)| TerminalProfileView {
                    id,
                    label,
                    is_default: false,
                }),
        )
        .collect()
    }

    fn commit_terminal_name(&mut self, id: u64) {
        let scope = self.dock_scope();
        if self.terminal_controls_host.rename_scope.as_ref() != Some(&scope)
            || !self.terminal_id_in_scope(id)
        {
            return;
        }
        let Some(rename) = self
            .view
            .terminal_controls
            .rename
            .as_mut()
            .filter(|rename| rename.id == id)
        else {
            return;
        };
        let name = rename.name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            rename.error = Some("Usa entre 1 y 80 caracteres, sin saltos de línea.".into());
            return;
        }
        self.terminal_controls_host
            .names
            .insert(id, name.to_owned());
        self.close_terminal_controls();
        self.refresh_terminal_titles();
        self.terminal_focus_pending = true;
    }

    fn handle_terminal_tab_action(&mut self, action: &str) {
        let Some((kind, raw)) = action.split_once(':') else {
            return;
        };
        let Some(target) = raw.parse::<u64>().ok().and_then(DockTabId::from_raw) else {
            return;
        };
        let terminal_popover = self.view.terminal_menu_open
            || self.view.terminal_controls.sessions_open
            || self.view.terminal_controls.profiles_open
            || self.view.terminal_controls.rename.is_some();
        if !self.terminal_id_in_scope(target.get())
            || self.view.show_settings
            || self.view.show_command_center
            || self.view.main_page != MainPage::Workspace
            || self.view.composer_full_access_confirm.is_some()
            || self.tree.has_open_popover() && !terminal_popover
        {
            return;
        }
        let retain_tab_focus = kind != "terminal_tab_menu"
            && self.tree.has_keyboard_focus()
            && self.tree.focused_id() == Some(action)
            && !self.tree.has_open_popover();
        let retain_session_list =
            kind == "terminal_close" && self.view.terminal_controls.sessions_open;
        let scope = self.dock_scope();
        if kind == "terminal_close" {
            let split_neighbor = self
                .terminal_controls_host
                .splits
                .get(&scope)
                .filter(|pair| {
                    self.active_terminal_id() == Some(target.get()) && pair.contains(&target.get())
                })
                .and_then(|pair| pair.iter().copied().find(|id| *id != target.get()));
            self.dock.close(&scope, target);
            if let Some(neighbor) = split_neighbor.and_then(DockTabId::from_raw) {
                self.dock.activate(&scope, neighbor);
            }
        } else {
            self.dock.activate(&scope, target);
        }
        self.close_terminal_controls();
        self.sync_dock_view();
        if retain_session_list && self.view.show_terminal_panel {
            // Keep managing the remaining sessions, but return keyboard focus
            // to a selection row rather than the next destructive action.
            self.view.terminal_controls.sessions_open = true;
            self.terminal_controls_host.menu_scope = Some(scope);
            self.terminal_focus_pending = false;
            if let Some(active) = self.active_terminal_id() {
                self.tree.focus(&format!("terminal_tab_menu:{active}"));
            }
            return;
        }
        self.terminal_focus_pending = self.view.show_terminal_panel && !retain_tab_focus;
        if !self.view.show_terminal_panel {
            if self.ide_workspace_active() {
                // The agent can be hidden in the IDE. Return to the visible
                // document, or its persistent file-search control, without
                // leaving a prompt request that steals focus when chat opens.
                self.prompt_focus_pending = false;
                let source_visible = self.view.side_tab == SideTab::FilePreview
                    && !self.view.ide.extensions_open
                    && self
                        .view
                        .document_preview
                        .as_ref()
                        .is_none_or(|preview| preview.is_source_visible());
                let editor = source_visible
                    .then(|| self.view.code_editor.as_ref())
                    .flatten()
                    .map(|editor| crate::code_editor_ui::view_id(editor.tab_id));
                if let Some(editor) = editor {
                    self.editor_services.focus_next = (!self.tree.focus(&editor)).then_some(editor);
                } else {
                    self.editor_services.focus_next = None;
                    self.tree.focus("ide_titlebar_quick_open");
                }
            } else {
                self.prompt_focus_pending = true;
                self.focus_prompt_if_pending();
            }
        } else if retain_tab_focus {
            if let Some(active) = self.active_terminal_id() {
                self.tree.focus(&format!("terminal_tab:{active}"));
            }
        }
    }

    pub(super) fn handle_terminal_controls_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        if let UiAction::InputChanged { id, value } = action {
            let Some(id) = id
                .strip_prefix("terminal_rename_input:")
                .and_then(|raw| raw.parse::<u64>().ok())
            else {
                return false;
            };
            if self.terminal_controls_host.rename_scope.as_ref() == Some(&self.dock_scope()) {
                if let Some(rename) = self
                    .view
                    .terminal_controls
                    .rename
                    .as_mut()
                    .filter(|rename| rename.id == id)
                {
                    rename.name = value.chars().take(256).collect();
                    rename.error = None;
                }
            }
            return true;
        }
        if let UiAction::Submitted { id, .. } = action {
            let Some(id) = id
                .strip_prefix("terminal_rename_input:")
                .and_then(|raw| raw.parse::<u64>().ok())
            else {
                return false;
            };
            self.commit_terminal_name(id);
            return true;
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        match id.as_str() {
            _ if id.starts_with("terminal_tab:")
                || id.starts_with("terminal_tab_menu:")
                || id.starts_with("terminal_close:") =>
            {
                self.handle_terminal_tab_action(id);
            }
            "terminal_split" => self.split_terminal(),
            "terminal_unsplit" => {
                self.terminal_controls_host
                    .splits
                    .remove(&self.dock_scope());
                self.close_terminal_controls();
                self.sync_dock_view();
                self.terminal_focus_pending = true;
            }
            _ if id.starts_with("terminal_restart:") || id.starts_with("terminal_bottom:") => {
                let Some(target) = id
                    .split_once(':')
                    .and_then(|(_, id)| id.parse::<u64>().ok())
                else {
                    return true;
                };
                if self.activate_terminal_pane(target) {
                    let action = if id.starts_with("terminal_restart:") {
                        "terminal_restart"
                    } else {
                        "terminal_bottom"
                    };
                    self.apply_action(UiAction::Clicked { id: action.into() }, ctx);
                }
            }
            "terminal_sessions_toggle" | "terminal_profiles_toggle" | "terminal_menu_toggle" => {
                if !self.view.show_terminal_panel
                    || self.view.show_settings
                    || self.view.show_command_center
                    || self.view.main_page != MainPage::Workspace
                    || self.active_terminal_id().is_none()
                {
                    return true;
                }
                let opening = match id.as_str() {
                    "terminal_sessions_toggle" => !self.view.terminal_controls.sessions_open,
                    "terminal_profiles_toggle" => !self.view.terminal_controls.profiles_open,
                    _ => !self.view.terminal_menu_open,
                };
                self.close_terminal_controls();
                self.terminal_focus_pending = false;
                if opening {
                    self.terminal_controls_host.menu_scope = Some(self.dock_scope());
                    match id.as_str() {
                        "terminal_sessions_toggle" => {
                            self.refresh_terminal_titles();
                            self.view.terminal_controls.sessions_open = true;
                        }
                        "terminal_profiles_toggle" => {
                            self.view.terminal_controls.profiles =
                                self.available_terminal_profiles();
                            self.view.terminal_controls.profiles_open = true;
                        }
                        _ => self.view.terminal_menu_open = true,
                    }
                }
            }
            "terminal_sessions_dismiss"
            | "terminal_profiles_dismiss"
            | "terminal_rename_dismiss"
            | "terminal_menu_dismiss" => self.close_terminal_controls(),
            "terminal_settings" => {
                self.close_terminal_controls();
                if !self.view.show_settings {
                    self.apply_action(
                        UiAction::Clicked {
                            id: "toggle_settings".into(),
                        },
                        ctx,
                    );
                }
                self.apply_action(
                    UiAction::Clicked {
                        id: "settings_page:terminal".into(),
                    },
                    ctx,
                );
            }
            _ if id.starts_with("terminal_new_profile:") => {
                let profile_id = id.trim_start_matches("terminal_new_profile:");
                if !self.view.terminal_controls.profiles_open
                    || self.terminal_controls_host.menu_scope.as_ref() != Some(&self.dock_scope())
                    || !self
                        .view
                        .terminal_controls
                        .profiles
                        .iter()
                        .any(|profile| profile.id == profile_id)
                {
                    return true;
                }
                if let Some(agent) = crate::agent_cli_definitions::profile(profile_id) {
                    self.new_agent_terminal(agent);
                } else if let Some(profile) =
                    named_profile(profile_id, &self.settings_preferences.terminal_shell)
                {
                    self.new_terminal_with_profile(profile);
                }
            }
            _ if id.starts_with("terminal_rename:") => {
                let Some(id) = id
                    .trim_start_matches("terminal_rename:")
                    .parse::<u64>()
                    .ok()
                else {
                    return true;
                };
                if !self.terminal_id_in_scope(id) {
                    return true;
                }
                let Some(tab) = self
                    .view
                    .terminal_tabs
                    .iter()
                    .find(|tab| tab.id == id)
                    .cloned()
                else {
                    return true;
                };
                self.close_terminal_controls();
                self.terminal_focus_pending = false;
                self.terminal_controls_host.rename_scope = Some(self.dock_scope());
                self.view.terminal_controls.rename = Some(TerminalRenameView {
                    id,
                    name: tab.title.clone(),
                    error: None,
                });
                self.tree
                    .set_input_value(&format!("terminal_rename_input:{id}"), &tab.title);
            }
            _ if id.starts_with("terminal_rename_confirm:") => {
                if let Ok(id) = id
                    .trim_start_matches("terminal_rename_confirm:")
                    .parse::<u64>()
                {
                    self.commit_terminal_name(id);
                }
            }
            _ if id.starts_with("terminal_paste:") => {
                let id = id.trim_start_matches("terminal_paste:").parse::<u64>().ok();
                if id != self.active_terminal_id()
                    || id.is_none()
                    || !self.terminal_panel_interactive()
                    || !self
                        .view
                        .terminal_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.running)
                {
                    return true;
                }
                self.close_terminal_controls();
                self.terminal_focus_pending = true;
                let screen = self.active_terminal_screen_id();
                if self.tree.focus(&screen) {
                    self.request_text_paste(screen, ctx);
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
#[path = "terminal_controls_app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "terminal_split_app_tests.rs"]
mod split_tests;
