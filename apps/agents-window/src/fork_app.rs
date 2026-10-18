//! A task forked from a turn: a new task of any agent in the same project,
//! carrying the conversation up to that turn as text (`handoff_text`). The
//! native agent and ACP agents get it written in their composer, for the
//! person to add what to do and send; a CLI gets it typed into its prompt
//! once it has started, without Enter.

use super::*;

/// A CLI that writes nothing for this long has drawn its prompt.
const CLI_READY_QUIET: Duration = Duration::from_millis(1500);

impl AgentsWindow {
    pub(super) fn fork_task(&mut self, end: usize, target: &str) {
        let title = self.view.selected_session_title.clone().unwrap_or_default();
        let items = &self.view.transcript[..end.min(self.view.transcript.len())];
        let text = crate::message_actions::handoff_text(items, &title);
        let before = self.view.selected_session.clone();
        if let Some(agent) = target.strip_prefix("cli:") {
            self.start_cli_task(agent);
            if let Some(session) = self.view.selected_session.clone().filter(|session| Some(session) != before.as_ref()) {
                self.cli_handoffs.insert(session, (text, Instant::now()));
                self.view.status = "La CLI recibe el contexto cuando termine de iniciar".into();
            }
            return;
        }
        match target.strip_prefix("acp:") {
            Some(agent) => self.start_acp_task(agent),
            None => self.new_session(),
        }
        let opened = match target {
            "native" => self.view.selected_session.is_none(),
            _ => self.view.selected_session.is_some() && self.view.selected_session != before,
        };
        if opened {
            self.view.prompt = text;
            self.tree.set_input_value("prompt", &self.view.prompt);
            self.save_draft();
            self.prompt_focus_pending = true;
            self.view.status = "El contexto de la tarea está en el mensaje: agregá qué hacer y envialo".into();
        }
    }

    /// Type each waiting context into its CLI once the CLI is ready.
    pub(super) fn poll_cli_handoffs(&mut self, ctx: &egui::Context) -> bool {
        if self.cli_handoffs.is_empty() {
            return false;
        }
        let sessions: Vec<String> = self.cli_handoffs.keys().cloned().collect();
        let mut changed = false;
        for session in sessions {
            let workspace = self.view.sessions.iter().find(|candidate| candidate.id == session).map(|candidate| candidate.project.clone());
            let scope = DockScope::for_owner(Path::new(&workspace.unwrap_or_else(|| self.view.workspace.clone())), &session);
            let terminal = self.dock.terminal_tabs(&scope).first().map(|tab| tab.id.get());
            let Some(terminal) = terminal.and_then(|id| self.terminals.get_mut(&id)) else {
                // Its terminal has not started yet, or was closed for good.
                if self.cli_handoffs[&session].1.elapsed() > Duration::from_secs(60) {
                    self.cli_handoffs.remove(&session);
                }
                continue;
            };
            if terminal.quiet_for().is_some_and(|quiet| quiet >= CLI_READY_QUIET) {
                if let Some((text, _)) = self.cli_handoffs.remove(&session) {
                    if let Err(error) = terminal.paste(&text) {
                        self.view.status = format!("No se pudo pasar el contexto a la CLI: {error}");
                    }
                    changed = true;
                }
            }
        }
        if !self.cli_handoffs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(300));
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::TranscriptItem;

    #[test]
    fn a_fork_to_the_native_agent_opens_a_draft_holding_the_conversation() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = temp.path().to_string_lossy().into_owned();
        app.view.selected_session = Some("source".into());
        app.view.selected_session_title = Some("Login".into());
        app.view.transcript = vec![
            TranscriptItem::User { content: "Arreglá el login".into(), images: Vec::new() },
            TranscriptItem::Assistant { content: "Listo, cambié auth.rs".into(), streaming: false },
            TranscriptItem::User { content: "Ahora los tests".into(), images: Vec::new() },
            TranscriptItem::Assistant { content: "No llegues acá".into(), streaming: false },
        ];
        app.fork_task(2, "native");
        assert!(app.view.selected_session.is_none());
        let prompt = &app.view.prompt;
        assert!(prompt.starts_with("[Contexto de la tarea «Login» de openADE."), "{prompt}");
        assert!(prompt.contains("Usuario:\nArreglá el login") && prompt.contains("Asistente:\nListo, cambié auth.rs"), "{prompt}");
        assert!(!prompt.contains("Ahora los tests") && !prompt.contains("No llegues"), "only up to the turn: {prompt}");
        assert!(prompt.ends_with("Lo que sigue es el pedido nuevo.]\n\n"));
    }

    #[test]
    fn a_long_conversation_keeps_its_first_message_and_its_end() {
        let mut items = vec![TranscriptItem::User { content: "Primer pedido".into(), images: Vec::new() }];
        for index in 0..200 {
            items.push(TranscriptItem::Assistant { content: format!("respuesta {index} {}", "x".repeat(200)), streaming: false });
        }
        let text = crate::message_actions::handoff_text(&items, "Larga");
        assert!(text.len() < 17_000, "{}", text.len());
        assert!(text.contains("Primer pedido") && text.contains("respuesta 199") && !text.contains("respuesta 0 "));
        assert!(text.contains("sin "), "says what it left out");
    }
}
