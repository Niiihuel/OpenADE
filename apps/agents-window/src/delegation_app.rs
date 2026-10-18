//! Tasks a CLI hands to another agent through the bridge's `delegate_task`.
//! Each is started as an ordinary task of the same project, the way the
//! composer starts one, then left running off screen like any task the person
//! switches away from; its turn's final answer goes back to the CLI.

use super::*;
use external_bridge_app::DelegateCall;

/// More would be a CLI fanning out faster than anyone can follow.
const MAX_DELEGATIONS: usize = 4;

impl AgentsWindow {
    pub(super) fn serve_delegate_calls(&mut self) -> bool {
        let calls = std::mem::take(
            &mut *self.external_bridge.delegate_calls.lock().unwrap_or_else(|e| e.into_inner()),
        );
        let served = !calls.is_empty();
        for call in calls {
            match self.start_delegated(&call) {
                Ok(session) => {
                    self.delegations.insert(session, call.reply);
                }
                Err(error) => {
                    let _ = call.reply.send(Err(error));
                }
            }
        }
        served
    }

    /// The agents a task can be handed to, as `delegate_task` names them.
    fn delegation_agents(&self) -> Vec<String> {
        let mut agents = vec!["openade".to_owned()];
        agents.extend(self.view.acp_agents.iter().map(|offer| offer.id.clone()));
        agents.extend(
            self.profiles
                .iter()
                .filter(|profile| profile.id != "native" && profile.detect_executable().is_some())
                .map(|profile| profile.id.clone()),
        );
        agents
    }

    /// Start `call` as a task and come back to what the person had open.
    /// Returns the new task's id.
    fn start_delegated(&mut self, call: &DelegateCall) -> Result<String, String> {
        if call.workspace != Path::new(&self.view.workspace) {
            return Err("Abrí ese proyecto en openADE para delegarle tareas".into());
        }
        if self.delegations.len() >= MAX_DELEGATIONS {
            return Err(format!("Ya hay {MAX_DELEGATIONS} tareas delegadas en curso"));
        }
        let agents = self.delegation_agents();
        if !agents.contains(&call.agent) {
            return Err(format!("Agente desconocido «{}». Disponibles: {}", call.agent, agents.join(", ")));
        }
        let previous = self.view.selected_session.clone();
        // A draft has no task to come back to: its text is put back by hand.
        let draft = (self.view.prompt.clone(), self.view.composer_attachments.clone());
        let acp = self.view.acp_agents.iter().any(|offer| offer.id == call.agent);
        if acp {
            self.start_acp_task(&call.agent);
        } else {
            self.new_session();
            if call.agent != "openade" {
                self.view.selected_backend = call.agent.clone();
            }
        }
        let opened = if acp { self.view.selected_session != previous } else { self.view.selected_session.is_none() };
        let started = opened && {
            self.view.prompt = call.prompt.clone();
            self.view.composer_attachments.clear();
            self.start_run();
            self.active_run.is_some()
        };
        let session = self.view.selected_session.clone().filter(|_| started);
        let refusal = self.view.status.clone();
        match &previous {
            Some(id) => self.open_session(id),
            None => {
                self.new_session();
                (self.view.prompt, self.view.composer_attachments) = draft;
                self.save_draft();
            }
        }
        session.ok_or_else(|| {
            if refusal.is_empty() { "openADE no pudo iniciar la tarea".into() } else { refusal }
        })
    }

    /// Send `prompt` as a turn of the existing task `session`, which is idle,
    /// and leave the person where they were; off screen it goes on parked.
    pub(super) fn start_turn_off_screen(
        &mut self,
        session: &str,
        prompt: &str,
        attachments: Vec<ComposerAttachmentView>,
    ) -> Result<(), String> {
        let previous = self.view.selected_session.clone();
        let draft = (self.view.prompt.clone(), self.view.composer_attachments.clone());
        let here = previous.as_deref() == Some(session);
        if !here {
            self.open_session(session);
            if self.view.selected_session.as_deref() != Some(session) {
                return Err(self.view.status.clone());
            }
        }
        self.view.prompt = prompt.to_owned();
        self.view.composer_attachments = attachments;
        self.start_run();
        let started = self.active_run.is_some();
        let refusal = self.view.status.clone();
        match (&previous, here) {
            (_, true) => {
                (self.view.prompt, self.view.composer_attachments) = draft;
                self.tree.set_input_value("prompt", &self.view.prompt);
            }
            (Some(id), false) => self.open_session(id),
            (None, false) => {
                self.new_session();
                (self.view.prompt, self.view.composer_attachments) = draft;
                self.save_draft();
            }
        }
        if started { Ok(()) } else { Err(refusal) }
    }

    /// Give the CLI waiting on `session` its answer once the turn ends.
    pub(super) fn answer_delegation(&mut self, session: &str, kind: &AgentEventKind) {
        let answer = match kind {
            AgentEventKind::TurnCompleted { response, .. } => Ok(response.clone()),
            AgentEventKind::TurnFailed { error } => Err(format!("La tarea delegada falló: {error}")),
            AgentEventKind::TurnCancelled => Err("La tarea delegada se canceló".into()),
            AgentEventKind::TurnPaused { .. } => {
                Err("La tarea delegada quedó en pausa; la persona puede retomarla en openADE".into())
            }
            _ => return,
        };
        if let Some(reply) = self.delegations.remove(session) {
            let _ = reply.send(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(agent: &str, workspace: &Path) -> (DelegateCall, std::sync::mpsc::Receiver<Result<String, String>>) {
        let (reply, answer) = std::sync::mpsc::channel();
        (
            DelegateCall { agent: agent.into(), prompt: "Revisá los tests".into(), workspace: workspace.into(), reply },
            answer,
        )
    }

    #[test]
    fn unknown_agents_other_projects_and_too_many_calls_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = temp.path().to_string_lossy().into_owned();
        let (other, _) = call("openade", &temp.path().join("elsewhere"));
        assert!(app.start_delegated(&other).unwrap_err().contains("Abrí ese proyecto"));
        let (unknown, _) = call("nadie", temp.path());
        let error = app.start_delegated(&unknown).unwrap_err();
        assert!(error.contains("Agente desconocido «nadie»") && error.contains("openade"), "{error}");
        for index in 0..MAX_DELEGATIONS {
            app.delegations.insert(format!("s{index}"), std::sync::mpsc::channel().0);
        }
        let (busy, _) = call("openade", temp.path());
        assert!(app.start_delegated(&busy).unwrap_err().contains("en curso"));
    }

    #[test]
    fn the_end_of_a_delegated_turn_answers_its_caller_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        let (reply, answer) = std::sync::mpsc::channel();
        app.delegations.insert("task".into(), reply);
        app.answer_delegation("task", &AgentEventKind::TurnStarted { prompt: "x".into() });
        assert!(answer.try_recv().is_err());
        app.answer_delegation("task", &AgentEventKind::TurnCompleted { response: "Listo".into(), steps: 1 });
        assert_eq!(answer.try_recv().unwrap(), Ok("Listo".into()));
        assert!(app.delegations.is_empty());
    }
}
