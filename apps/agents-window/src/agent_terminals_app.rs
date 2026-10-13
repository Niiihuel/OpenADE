//! Commands the native agent starts, as terminal tabs of their conversation.
//!
//! The process stays with the agent's command manager; a tab is the user's
//! window on it. What the user types there goes straight to the process: it
//! is their own terminal, so it passes neither the manual-input policy of
//! "Operaciones" nor the command hooks. The agent's `write_stdin` is unchanged.
//!
//! A command that turns out to be a server (it prints an address of this
//! machine, and that address answers) gets a live preview in the
//! conversation's browser tab, when the user's setting says so.
use super::*;
use crate::native_command_sessions::StartedCommand;
use crate::preview::PreviewOpening;
use crate::terminal_dock_ui::AgentTerminalView;
use openide_harness::PersistentCommandExit;

pub(super) struct AgentTerminal {
    /// The native session whose agent started it; its tab shows with it.
    owner: String,
    /// That conversation's tabs: its terminal, and the browser of a preview.
    scope: DockScope,
    /// The manager that runs it: another one means it was revoked.
    generation: String,
    command_id: String,
    /// The command line, as the tab's title.
    command: String,
    read_only: bool,
    ended: Option<String>,
    /// The address its server printed, once something answered there.
    preview: Option<String>,
    started: Instant,
}

/// A server says where it listens within its first minute, and the check of
/// that address takes a few seconds more.
const PREVIEW_WAIT: Duration = Duration::from_secs(70);

fn ended_label(exit: &PersistentCommandExit, revoked: bool) -> String {
    match exit {
        PersistentCommandExit::Exited { code: Some(code) } => {
            format!("Proceso finalizado · código {code}")
        }
        PersistentCommandExit::Exited { code: None } => "Proceso finalizado por una señal".into(),
        PersistentCommandExit::Terminated if revoked => {
            "Detenido · se revocaron los comandos de esta conversación".into()
        }
        PersistentCommandExit::Terminated => "Detenido".into(),
        PersistentCommandExit::TimedOut => "Detenido · alcanzó su tiempo límite".into(),
        PersistentCommandExit::WaitFailed(error) => {
            format!("No se pudo observar el final del proceso: {error}")
        }
    }
}

impl AgentsWindow {
    pub(super) fn agent_terminal_title(&self, id: u64) -> Option<&str> {
        self.agent_terminals
            .get(&id)
            .map(|agent| agent.command.as_str())
    }

    pub(super) fn is_agent_terminal(&self, id: u64) -> bool {
        self.agent_terminals.contains_key(&id)
    }

    /// A running agent command, whose tab closes only after asking.
    pub(super) fn agent_terminal_running(&self, id: u64) -> bool {
        self.agent_terminals
            .get(&id)
            .is_some_and(|agent| agent.ended.is_none())
            && self.terminals.contains_key(&id)
    }

    pub(super) fn forget_closed_agent_terminals(&mut self) {
        self.agent_terminals
            .retain(|id, _| self.dock.contains_tab(*id));
    }

    /// Give each command an agent just started its tab, and note the ones
    /// that ended. Runs every frame; both checks are a channel and a lock.
    pub(super) fn follow_agent_terminals(&mut self) -> bool {
        let mut changed = false;
        for started in self.native_command_sessions.take_started() {
            changed |= self.open_agent_terminal(started);
        }
        let mut answered = Vec::new();
        for (id, agent) in &mut self.agent_terminals {
            let Some(session) = self.terminals.get(id).filter(|_| agent.ended.is_none()) else {
                continue;
            };
            if agent.preview.is_none() {
                // Reported once per command, so it never opens a second time.
                if let Some(url) = session.agent_preview() {
                    agent.preview = Some(url.clone());
                    answered.push((agent.owner.clone(), agent.scope.clone(), url));
                    changed = true;
                }
            }
            if let Some(exit) = session.agent_exit() {
                let revoked = self
                    .native_command_sessions
                    .manager(&agent.owner)
                    .is_none_or(|manager| manager.generation() != agent.generation);
                agent.ended = Some(ended_label(&exit, revoked));
                changed = true;
            }
        }
        for (owner, scope, url) in answered {
            if self.settings_preferences.preview_opening == PreviewOpening::Auto {
                self.open_preview(&scope, &url);
                let task = self.session_title(&owner);
                self.view.notifications.push(
                    crate::notifications::Severity::Info,
                    format!("Vista previa abierta: {url}"),
                    task,
                    Some(crate::notifications::Target::Session(owner)),
                );
            }
        }
        changed |= self.follow_browser_previews();
        if changed {
            self.refresh_agent_terminal_views();
        }
        changed
    }

    /// A command may still announce a server. Its terminal can be hidden,
    /// and then nothing repaints the window: it keeps looking by itself.
    pub(super) fn awaiting_agent_preview(&self) -> bool {
        self.settings_preferences.preview_opening != PreviewOpening::Never
            && self.agent_terminals.values().any(|agent| {
                agent.ended.is_none()
                    && agent.preview.is_none()
                    && agent.started.elapsed() < PREVIEW_WAIT
            })
    }

    fn session_title(&self, session: &str) -> String {
        self.view
            .sessions
            .iter()
            .find(|listed| listed.id == session)
            .map_or_else(|| "esta conversación".to_owned(), |listed| listed.title.clone())
    }

    /// Open or reuse a conversation's "Navegador" tab on the address of a
    /// server of this machine, and of no other.
    fn open_preview(&mut self, scope: &DockScope, url: &str) -> bool {
        let Some(url) = crate::preview::loopback_url(url) else {
            return false;
        };
        let tab = self.open_tab_beside_typing(scope, DockTabKind::Browser);
        // The tab's context may still be starting, or not be started until
        // the tab is shown: the address waits and opens once it is ready.
        self.browser_restart_urls.insert(tab.get(), url);
        if *scope == self.dock_scope() {
            self.sync_dock_view();
        }
        true
    }

    /// Open or reuse a tab nobody asked for by hand. The keyboard stays where
    /// it is: the tab comes forward only when that does not replace what the
    /// user is typing in, and is otherwise left among the tabs. The caller
    /// syncs the view when `scope` is the one shown.
    pub(super) fn open_tab_beside_typing(&mut self, scope: &DockScope, kind: DockTabKind) -> DockTabId {
        let shown = *scope == self.dock_scope();
        let previous = self.dock.active_tab(scope).map(|tab| tab.id);
        let collapsed = self.dock.is_collapsed(scope);
        let tab = self.dock.open(scope, kind);
        self.dock.set_collapsed(scope, false);
        // The composer is beside the dock; any other field or editor may be
        // the tab this one would cover.
        let typing = shown && self.typing_in().is_some_and(|field| field != "prompt");
        if typing {
            if let Some(previous) = previous {
                self.dock.activate(scope, previous);
            }
            self.dock.set_collapsed(scope, collapsed);
        }
        tab
    }

    /// The text field or editor that holds the keyboard, if one does.
    fn typing_in(&self) -> Option<&str> {
        use openide_ui::HitKind::{CodeEditor, TextArea, TextInput};
        let focused = self.tree.focused_id()?;
        self.tree
            .frame()
            .regions
            .iter()
            .any(|region| {
                region.id == focused && matches!(region.kind, CodeEditor | TextArea | TextInput)
            })
            .then_some(focused)
    }

    /// The agent's `open_preview`: tell each browser which address it opens
    /// by default, and show the tab of one it just opened.
    fn follow_browser_previews(&mut self) -> bool {
        let mut reveal = Vec::new();
        for (id, runtime) in &self.browsers {
            // The newest command of that conversation that serves.
            let serving = self
                .agent_terminals
                .iter()
                .filter(|(_, agent)| {
                    agent.ended.is_none() && agent.owner == runtime.scope().session_id
                })
                .max_by_key(|(tab, _)| **tab)
                .and_then(|(_, agent)| agent.preview.clone());
            runtime.set_preview_url(serving);
            if runtime.take_reveal() {
                reveal.push(*id);
            }
        }
        let mut changed = false;
        for id in reveal {
            let Some(tab) = self.dock.tab(id).cloned() else {
                continue;
            };
            let scope = DockScope::new(tab.workspace_id, tab.owner);
            self.dock.activate(&scope, tab.id);
            self.dock.set_collapsed(&scope, false);
            if scope == self.dock_scope() {
                self.sync_dock_view();
                changed = true;
            }
        }
        changed
    }

    fn open_agent_terminal(&mut self, started: StartedCommand) -> bool {
        let owner = started.owner;
        let workspace = if self.view.selected_session.as_deref() == Some(owner.as_str()) {
            self.view.workspace.clone()
        } else {
            let stored = self
                .store
                .as_ref()
                .and_then(|store| store.load(&owner).ok().flatten());
            match stored {
                Some(stored) => stored.session.workspace.to_string_lossy().into_owned(),
                None => return false,
            }
        };
        let scope = DockScope::for_owner(Path::new(&workspace), &owner);
        let previous = self.dock.active_terminal(&scope).map(|tab| tab.id);
        let collapsed = self.dock.is_terminal_collapsed(&scope);
        // A new tab comes to the front of its conversation's panel and opens it.
        let id = self.dock.new_terminal(&scope).get();
        let command: String = started
            .argv
            .join(" ")
            .chars()
            .map(|character| if character.is_control() { ' ' } else { character })
            .take(80)
            .collect();
        match started.session {
            Ok(session) => {
                self.terminals.insert(id, session);
            }
            Err(error) => {
                self.terminal_controls_host
                    .errors
                    .insert(id, format!("No se pudo mostrar el comando: {error}"));
            }
        }
        self.agent_terminals.insert(
            id,
            AgentTerminal {
                owner,
                scope: scope.clone(),
                generation: started.generation,
                command_id: started.id,
                command,
                read_only: started.read_only,
                ended: None,
                preview: None,
                started: Instant::now(),
            },
        );
        if scope != self.dock_scope() {
            // Nobody is typing there: it shows when that conversation opens.
            return false;
        }
        // Unless that replaces what the user is typing in: a terminal's
        // screen, or another section of the IDE's bottom panel. Then the tab
        // is only listed, and the panel stays as it was.
        // Nor does it bring back the panel a maximized dock hid.
        let busy = self.view.ide.bottom_open
            || self.dock.is_maximized(&scope)
            || self
                .tree
                .focused_id()
                .is_some_and(|focused| focused.starts_with("terminal_"));
        if busy {
            if let Some(previous) = previous {
                self.dock.activate(&scope, previous);
            }
            self.dock.set_terminal_collapsed(&scope, collapsed);
        }
        self.sync_dock_view();
        true
    }

    pub(super) fn refresh_agent_terminal_views(&mut self) {
        let agents: Vec<_> = self
            .view
            .terminal_tabs
            .iter()
            .filter_map(|tab| {
                let agent = self.agent_terminals.get(&tab.id)?;
                Some(AgentTerminalView {
                    id: tab.id,
                    command_id: agent.command_id.clone(),
                    task: self.session_title(&agent.owner),
                    read_only: agent.read_only,
                    ended: agent.ended.clone(),
                    // Offered while it serves, unless the user wants none.
                    preview: agent.preview.clone().filter(|_| {
                        agent.ended.is_none()
                            && self.settings_preferences.preview_opening != PreviewOpening::Never
                    }),
                })
            })
            .collect();
        let controls = &mut self.view.terminal_controls;
        if controls.closing.is_some_and(|id| {
            !agents
                .iter()
                .any(|agent| agent.id == id && agent.ended.is_none())
        }) {
            controls.closing = None;
        }
        controls.agents = agents;
    }

    /// The user confirmed closing a running command's tab: stop its process
    /// tree, then close the tab. Their own act on their tab, like its keys.
    fn stop_agent_terminal(&mut self, id: u64) {
        if self.view.terminal_controls.closing != Some(id) {
            return;
        }
        self.view.terminal_controls.closing = None;
        if let Some(agent) = self.agent_terminals.remove(&id) {
            if let Some(manager) = self
                .native_command_sessions
                .manager(&agent.owner)
                .filter(|manager| manager.generation() == agent.generation)
            {
                // Unknown to its manager: it already exited and was retired.
                let _ = manager.stop(&agent.command_id);
            }
        }
        let scope = self.dock_scope();
        if let Some(tab) = DockTabId::from_raw(id) {
            self.dock.close(&scope, tab);
        }
        self.close_terminal_controls();
        self.sync_dock_view();
    }

    /// "Ver en terminal": the user asked for it, so it takes the keyboard.
    fn show_agent_terminal(&mut self, command_id: &str) {
        let Some(tab) = self
            .view
            .terminal_controls
            .agents
            .iter()
            .find(|agent| agent.command_id == command_id)
            .and_then(|agent| DockTabId::from_raw(agent.id))
        else {
            return;
        };
        let scope = self.dock_scope();
        self.dock.set_terminal_collapsed(&scope, false);
        self.dock.activate(&scope, tab);
        self.sync_dock_view();
        self.terminal_focus_pending = true;
    }

    pub(super) fn apply_agent_terminal_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(tab) = id.strip_prefix("agent_terminal_stop:") {
            if let Ok(tab) = tab.parse() {
                self.stop_agent_terminal(tab);
            }
        } else if id == "agent_terminal_stop_cancel" {
            self.view.terminal_controls.closing = None;
        } else if let Some(command) = id.strip_prefix("agent_terminal_show:") {
            self.show_agent_terminal(command);
        } else if let Some(command) = id.strip_prefix("agent_preview_open:") {
            // The user asked: the address is the one recorded on the command.
            let found = self
                .agent_terminals
                .values()
                .find(|agent| agent.command_id == command && agent.scope == self.dock_scope())
                .and_then(|agent| Some((agent.scope.clone(), agent.preview.clone()?)));
            if let Some((scope, url)) = found {
                self.open_preview(&scope, &url);
            }
        } else {
            return false;
        }
        true
    }
}

#[cfg(test)]
#[path = "agent_terminals_app_tests.rs"]
mod tests;
