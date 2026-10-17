//! External agents over the Agent Client Protocol in the window: which ones
//! this machine runs, their tasks and their turns.

use super::*;
use crate::acp_run::{AcpOffer, AcpRunHandle, TurnRequest};

impl AgentsWindow {
    /// Looks, off the UI thread, for the registry's agents installed here.
    /// The index comes from a day-old copy or the network.
    pub(super) fn refresh_acp_agents(&mut self) {
        let root = self.data_root.clone();
        let search_path = self.agent_search_path();
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new().name("acp-agents".into()).spawn(move || {
            let found = openide_acp::registry::load(&root)
                .map(|registry| crate::acp_run::offers(&registry, search_path.as_deref(), &root.join("acp-agents")));
            let _ = sender.send(found);
        });
        if started.is_ok() {
            self.acp_offers_loading = Some(receiver);
        }
    }

    pub(super) fn poll_acp_agents(&mut self, ctx: &egui::Context) -> bool {
        let Some(receiver) = &self.acp_offers_loading else {
            return false;
        };
        match receiver.try_recv() {
            Ok(found) => {
                self.acp_offers_loading = None;
                // Without the network or a copy of the index, the list stays as it was.
                if let Ok(offers) = found {
                    if offers != self.view.acp_agents {
                        self.view.acp_agents = offers;
                        return true;
                    }
                }
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(250));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.acp_offers_loading = None;
                false
            }
        }
    }

    /// Creates a task whose agent runs over ACP, in the ordinary conversation.
    pub(super) fn start_acp_task(&mut self, agent_id: &str) {
        let Some(offer) = self.view.acp_agents.iter().find(|offer| offer.id == agent_id).cloned() else {
            self.view.status = format!("{agent_id} no está instalado en este equipo");
            return;
        };
        if self.view.running {
            self.view.status = "Detén el agente antes de cambiar de sesión".to_owned();
            return;
        }
        let Some(store) = &self.store else {
            self.view.status = "No se pudo abrir el historial de tareas".to_owned();
            return;
        };
        let created = store
            .create(PathBuf::from(&self.view.workspace), format!("{}{}", crate::acp_run::ACP_BACKEND, offer.id))
            .and_then(|session| store.rename(&session.id, Some(&offer.name)));
        match created {
            Ok(session) => {
                self.refresh_sessions();
                self.open_session(&session.id);
            }
            Err(error) => self.view.status = format!("No se pudo crear la tarea: {error}"),
        }
    }

    /// Sends the composer's message to the session's ACP agent, starting the
    /// agent first when this is its first turn since the window opened.
    pub(super) fn start_acp_run(&mut self) {
        let Some(agent_id) = crate::acp_run::agent_id(&self.view.selected_backend).map(str::to_owned) else {
            return;
        };
        if self.view.running || self.active_run.is_some() {
            self.view.status = "Espera a que el agente termine este turno".to_owned();
            return;
        }
        let prompt = self.view.prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        if !self.view.composer_attachments.is_empty() {
            self.show_composer_error("Los adjuntos todavía no llegan a los agentes ACP; envía solo el texto.");
            return;
        }
        if self.persistence_error.is_some() {
            self.show_persistence_error();
            return;
        }
        let Some(offer): Option<AcpOffer> = self.view.acp_agents.iter().find(|offer| offer.id == agent_id).cloned() else {
            self.show_composer_error(format!("{agent_id} no está instalado en este equipo o todavía se está buscando"));
            return;
        };
        let Some(session_id) = self.view.selected_session.clone() else {
            self.show_composer_error("Abre la tarea del agente antes de escribirle");
            return;
        };
        self.view.composer_error = None;
        self.stop_dictation();
        let slot = self.acp_slots.entry(session_id.clone()).or_default().clone();
        let request = TurnRequest {
            session_id,
            sequence_start: self.last_sequence,
            workspace: PathBuf::from(&self.view.workspace),
            prompt: prompt.clone(),
            command: offer.command,
            agent_name: offer.name.clone(),
            data_root: self.data_root.clone(),
        };
        self.active_run = Some(ActiveRun::Acp(AcpRunHandle::start(request, slot)));
        self.run_has_output = false;
        let draft = self.draft_key();
        self.pending_prompt = Some(prompt);
        self.pending_attachments = Vec::new();
        self.view.prompt.clear();
        self.clear_draft_key(&draft);
        self.draft_dirty_at = None;
        self.tree.set_input_value("prompt", "");
        self.tree.scroll_to_end("transcript");
        self.view.running = true;
        self.view.status = format!("Ejecutando {}...", offer.name);
        self.refresh_sessions();
    }
}
