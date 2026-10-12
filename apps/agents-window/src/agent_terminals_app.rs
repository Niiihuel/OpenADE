//! Commands the native agent starts, as terminal tabs of their conversation.
//!
//! The process stays with the agent's command manager; a tab is the user's
//! window on it. What the user types there goes straight to the process: it
//! is their own terminal, so it passes neither the manual-input policy of
//! "Operaciones" nor the command hooks. The agent's `write_stdin` is unchanged.
use super::*;
use crate::terminal_dock_ui::AgentTerminalView;
use crate::native_command_sessions::StartedCommand;
use openide_harness::PersistentCommandExit;

pub(super) struct AgentTerminal {
    /// The native session whose agent started it; its tab shows with it.
    owner: String,
    /// The manager that runs it: another one means it was revoked.
    generation: String,
    command_id: String,
    /// The command line, as the tab's title.
    command: String,
    read_only: bool,
    ended: Option<String>,
}

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
        for (id, agent) in &mut self.agent_terminals {
            if agent.ended.is_some() {
                continue;
            }
            let Some(exit) = self.terminals.get(id).and_then(TerminalSession::agent_exit) else {
                continue;
            };
            let revoked = self
                .native_command_sessions
                .manager(&agent.owner)
                .is_none_or(|manager| manager.generation() != agent.generation);
            agent.ended = Some(ended_label(&exit, revoked));
            changed = true;
        }
        if changed {
            self.refresh_agent_terminal_views();
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
                generation: started.generation,
                command_id: started.id,
                command,
                read_only: started.read_only,
                ended: None,
            },
        );
        if scope != self.dock_scope() {
            // Nobody is typing there: it shows when that conversation opens.
            return false;
        }
        // Unless that replaces what the user is typing in: a terminal's
        // screen, or another section of the IDE's bottom panel. Then the tab
        // is only listed, and the panel stays as it was.
        let busy = self.view.ide.bottom_open
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
                    task: self
                        .view
                        .sessions
                        .iter()
                        .find(|session| session.id == agent.owner)
                        .map_or_else(|| "esta conversación".to_owned(), |session| session.title.clone()),
                    read_only: agent.read_only,
                    ended: agent.ended.clone(),
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
        } else {
            return false;
        }
        true
    }
}

#[cfg(test)]
#[path = "agent_terminals_app_tests.rs"]
mod tests;
