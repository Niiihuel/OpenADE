//! External agents over the Agent Client Protocol in the window: which ones
//! this machine runs, their tasks and their turns.

use super::*;
use crate::acp_run::{AcpEntry, AcpSource, AgentStart, AcpRunHandle, TurnRequest};
use std::sync::mpsc::{Receiver, TryRecvError};

/// Where openADE puts the agents it downloads, under the data folder.
const INSTALLS: &str = "acp-agents";

/// What `receiver` has sent, if anything yet; it is let go once it answers
/// or its sender is gone.
fn answer<T>(receiver: &mut Option<Receiver<T>>, ctx: &egui::Context) -> Option<T> {
    match receiver.as_ref()?.try_recv() {
        Ok(value) => {
            *receiver = None;
            Some(value)
        }
        Err(TryRecvError::Empty) => {
            ctx.request_repaint_after(Duration::from_millis(250));
            None
        }
        Err(TryRecvError::Disconnected) => {
            *receiver = None;
            None
        }
    }
}

impl AgentsWindow {
    /// Reads, off the UI thread, the registry's agents and how each runs
    /// here. The index comes from a day-old copy or the network.
    pub(super) fn refresh_acp_agents(&mut self) {
        let root = self.data_root.clone();
        let search_path = self.agent_search_path();
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new().name("acp-agents".into()).spawn(move || {
            let found = openide_acp::registry::load(&root).map(|registry| {
                crate::acp_run::catalog(&registry, search_path.as_deref(), &root.join(INSTALLS), &crate::acp_run::packages_on(&root))
            });
            let _ = sender.send(found);
        });
        if started.is_ok() {
            self.acp_offers_loading = Some(receiver);
        }
    }

    pub(super) fn poll_acp_agents(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        match answer(&mut self.acp_offers_loading, ctx) {
            Some(Ok(catalog)) => {
                let offers: Vec<_> = catalog.iter().filter_map(AcpEntry::offer).collect();
                changed = catalog != self.view.acp_catalog || self.view.acp_catalog_error.is_some();
                self.view.acp_catalog = catalog;
                self.view.acp_catalog_error = None;
                if offers != self.view.acp_agents {
                    self.view.acp_agents = offers;
                    changed = true;
                    // A task reopened at launch waited for its agent to be found.
                    self.warm_acp_agent();
                }
            }
            // Without the network or a copy of the index, the list stays as it was.
            Some(Err(error)) => {
                self.view.acp_catalog_error = Some(error);
                changed = true;
            }
            None => {}
        }
        if let Some((session, started)) = answer(&mut self.acp_warming, ctx) {
            if self.view.selected_session.as_deref() == Some(session.as_str()) && !self.view.running {
                match started {
                    Ok(()) => self.refresh_composer_selection(),
                    Err(error) => self.show_composer_error(error),
                }
                changed = true;
            }
        }
        if let Some((id, installed)) = answer(&mut self.acp_installing, ctx) {
            self.view.acp_installing = None;
            let name = self.acp_agent_name(&id);
            match installed {
                Ok(()) => self.set_settings_notice(format!("{name} quedó instalado y aparece en Nueva tarea con"), false),
                Err(error) => self.set_settings_notice(format!("No se pudo instalar {name}: {error}"), true),
            }
            self.refresh_acp_agents();
            changed = true;
        }
        changed
    }

    fn acp_agent_name(&self, id: &str) -> String {
        self.view.acp_catalog.iter().find(|entry| entry.id == id).map_or_else(|| id.to_owned(), |entry| entry.name.clone())
    }

    /// Downloads the vendor's build of a registry agent, off the UI thread.
    pub(super) fn install_acp_agent(&mut self, id: &str) {
        if self.view.acp_installing.is_some() {
            return;
        }
        let Some(AcpSource::Download { target, dir }) =
            self.view.acp_catalog.iter().find(|entry| entry.id == id).map(|entry| entry.source.clone())
        else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let id = id.to_owned();
        let installing = id.clone();
        let started = std::thread::Builder::new().name("acp-install".into()).spawn(move || {
            let _ = sender.send((id, openide_acp::registry::install(&target, &dir).map(drop)));
        });
        if started.is_ok() {
            self.acp_installing = Some(receiver);
            self.view.acp_installing = Some(installing);
        }
    }

    /// Deletes what openADE downloaded for an agent; the user's own installs
    /// are never touched.
    pub(super) fn remove_acp_agent(&mut self, id: &str) {
        let downloaded = self.view.acp_catalog.iter().any(|entry| entry.id == id && entry.source == AcpSource::Downloaded);
        if !downloaded || !openide_acp::registry::plain_name(id) {
            return;
        }
        let name = self.acp_agent_name(id);
        match std::fs::remove_dir_all(self.data_root.join(INSTALLS).join(id)) {
            Ok(()) => self.set_settings_notice(format!("Se quitó {name}"), false),
            Err(error) => self.set_settings_notice(format!("No se pudo quitar {name}: {error}"), true),
        }
        self.refresh_acp_agents();
    }

    /// Offers, or stops offering, an agent that `npx` or `uvx` runs.
    pub(super) fn toggle_acp_package(&mut self, id: &str) {
        let Some(on) = self.view.acp_catalog.iter().find(|entry| entry.id == id).and_then(|entry| match entry.source {
            AcpSource::Package { on, .. } => Some(on),
            _ => None,
        }) else {
            return;
        };
        match crate::acp_run::set_package_on(&self.data_root, id, !on) {
            Ok(()) => self.refresh_acp_agents(),
            Err(error) => self.set_settings_notice(format!("No se pudo guardar el cambio: {error}"), true),
        }
    }

    /// Starts the open task's agent ahead of its first turn, so the composer
    /// can offer its models. The idle agents of other tasks stop.
    pub(super) fn warm_acp_agent(&mut self) {
        let Some(agent_id) = crate::acp_run::agent_id(&self.view.selected_backend) else {
            return;
        };
        let Some(session_id) = self.view.selected_session.clone() else {
            return;
        };
        let Some(offer) = self.view.acp_agents.iter().find(|offer| offer.id == agent_id).cloned() else {
            return;
        };
        // Only the selected task can run a turn, so the others are idle.
        self.acp_slots.retain(|id, _| *id == session_id);
        let slot = self.acp_slots.entry(session_id.clone()).or_default().clone();
        if slot.agent().is_some() {
            return;
        }
        let start = AgentStart {
            session_id: session_id.clone(),
            workspace: PathBuf::from(&self.view.workspace),
            command: offer.command,
            agent_name: offer.name,
            data_root: self.data_root.clone(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new().name("acp-start".into()).spawn(move || {
            let _ = sender.send((session_id, slot.ensure(&start, None).map(drop)));
        });
        if started.is_ok() {
            self.acp_warming = Some(receiver);
        }
    }

    /// The composer's models and efforts are the ACP agent's own options.
    /// Returns false when the open task is not an ACP agent's.
    pub(super) fn refresh_acp_composer_options(&mut self) -> bool {
        if crate::acp_run::agent_id(&self.view.selected_backend).is_none() {
            return false;
        }
        let config = self
            .view
            .selected_session
            .as_ref()
            .and_then(|session| self.acp_slots.get(session))
            .map(|slot| slot.config())
            .unwrap_or_default();
        // A saved choice the agent does not offer gives way to its own.
        let offered = |category: &str, chosen: &mut String| {
            let Some((current, values)) = crate::acp_run::choices(&config, category) else {
                return Vec::new();
            };
            if !values.iter().any(|(value, _)| value == chosen) {
                *chosen = current;
            }
            values
        };
        self.view.composer_model_options = offered("model", &mut self.view.composer_model);
        self.view.composer_effort_options = offered("thought_level", &mut self.view.composer_effort);
        true
    }

    /// Picks an ACP agent's model, or its effort, for the next turns of this
    /// agent. None when the open task is not an ACP agent's.
    pub(super) fn select_acp_choice(&mut self, effort: bool, value: &str) -> Option<bool> {
        crate::acp_run::agent_id(&self.view.selected_backend)?;
        let options = if effort { &self.view.composer_effort_options } else { &self.view.composer_model_options };
        if !options.iter().any(|(known, _)| known == value) {
            return Some(false);
        }
        if effort {
            self.view.composer_effort = value.to_owned();
        } else {
            self.view.composer_model = value.to_owned();
        }
        self.remember_composer_selection(true);
        self.view.composer_model_menu_open = false;
        self.view.composer_model_list_open = false;
        self.view.composer_model_query.clear();
        self.composer_model_search_focus_pending = false;
        self.view.composer_advanced_open = false;
        self.view.composer_effort_menu_open = false;
        self.view.composer_effort_preview = None;
        Some(true)
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
        let attachments = self.view.composer_attachments.clone();
        if prompt.is_empty() && attachments.is_empty() {
            return;
        }
        if self.attachment_pastes_pending.contains(&self.draft_key()) {
            self.view.status = "Espera a que se preparen los adjuntos".to_owned();
            return;
        }
        if self.persistence_error.is_some() {
            self.show_persistence_error();
            return;
        }
        let Some(offer) = self.view.acp_agents.iter().find(|offer| offer.id == agent_id).cloned() else {
            self.show_composer_error(format!("{agent_id} no está instalado en este equipo o todavía se está buscando"));
            return;
        };
        let Some(session_id) = self.view.selected_session.clone() else {
            self.show_composer_error("Abre la tarea del agente antes de escribirle");
            return;
        };
        let workspace = PathBuf::from(&self.view.workspace);
        // shortcut: images are read even when they go as paths, so one over
        // 8 MiB is refused for an agent that would only open it itself.
        let payload = match prepare_native_attachments_with_image_support(&workspace, &attachments, true) {
            Ok(payload) => payload,
            Err(error) => {
                self.show_composer_error(error);
                return;
            }
        };
        let slot = self.acp_slots.entry(session_id.clone()).or_default().clone();
        // Files go as paths the agent opens itself. So do images, unless the
        // agent has said it takes them inline.
        let (text, images) = if slot.agent().is_some_and(|agent| agent.images) {
            (native_prompt_with_images(&prompt, &payload.path_attachments, &payload.image_attachments), payload.images)
        } else {
            (composer_prompt_with_attachments(&prompt, &attachments), Vec::new())
        };
        self.view.composer_error = None;
        self.stop_dictation();
        let request = TurnRequest {
            start: AgentStart {
                session_id,
                workspace,
                command: offer.command,
                agent_name: offer.name.clone(),
                data_root: self.data_root.clone(),
            },
            sequence_start: self.last_sequence,
            prompt: text,
            images,
            choices: vec![("model", self.view.composer_model.clone()), ("thought_level", self.view.composer_effort.clone())],
        };
        self.active_run = Some(ActiveRun::Acp(AcpRunHandle::start(request, slot)));
        self.run_has_output = false;
        let draft = self.draft_key();
        self.pending_prompt = Some(prompt);
        self.pending_attachments = attachments;
        self.view.prompt.clear();
        self.view.composer_attachments.clear();
        self.clear_draft_key(&draft);
        self.draft_dirty_at = None;
        self.tree.set_input_value("prompt", "");
        self.tree.scroll_to_end("transcript");
        self.view.running = true;
        self.view.status = format!("Ejecutando {}...", offer.name);
        self.refresh_sessions();
    }
}
