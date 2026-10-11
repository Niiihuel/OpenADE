//! Answering a question the agent asked. The answer is recorded in the
//! session's decision journal first and the request is resolved after, the
//! order a permission decision follows, so a crash between the two leaves an
//! answer the recovered turn takes instead of asking again.
use super::*;
use crate::question_ui::{self, QuestionDraft};

impl AgentsWindow {
    fn pending_question(&self, request_id: &str) -> Option<Vec<question_ui::Question>> {
        self.view
            .pending_permissions
            .iter()
            .find(|permission| {
                permission.request_id == request_id && permission.tool_name == question_ui::TOOL
            })
            .map(|permission| {
                question_ui::parse(
                    &serde_json::from_str(&permission.arguments).unwrap_or(serde_json::Value::Null),
                )
            })
    }

    pub(super) fn apply_question_action(&mut self, action: &UiAction, ctx: &egui::Context) -> bool {
        match action {
            UiAction::InputChanged { id, value } => {
                let Some((request_id, question)) = id
                    .strip_prefix("question_text:")
                    .and_then(|rest| rest.rsplit_once(':'))
                    .and_then(|(request, question)| Some((request, question.parse().ok()?)))
                else {
                    return false;
                };
                if let Some(questions) = self.pending_question(request_id) {
                    self.view
                        .question_drafts
                        .entry(request_id.to_owned())
                        .or_default()
                        .write(questions.len(), question, value);
                }
            }
            UiAction::Clicked { id } if id.starts_with("mode_proposal_") => {
                let Some((verb, request_id)) = id["mode_proposal_".len()..].split_once(':') else {
                    return false;
                };
                self.decide_mode_proposal(request_id, verb == "accept");
            }
            UiAction::Clicked { id } => {
                let Some(rest) = id.strip_prefix("question_") else {
                    return false;
                };
                let Some((verb, target)) = rest.split_once(':') else {
                    return false;
                };
                let request_id = target.split(':').next().unwrap_or_default().to_owned();
                let Some(questions) = self.pending_question(&request_id) else {
                    return true;
                };
                let draft: &mut QuestionDraft =
                    self.view.question_drafts.entry(request_id.clone()).or_default();
                match verb {
                    "pick" => {
                        let mut parts = target.split(':').skip(1).map(str::parse::<usize>);
                        if let (Some(Ok(question)), Some(Ok(option))) = (parts.next(), parts.next())
                        {
                            draft.pick(&questions, question, option);
                        }
                    }
                    "next" => draft.turn(questions.len(), true),
                    "back" => draft.turn(questions.len(), false),
                    "send" => {
                        if draft.any_answer(questions.len()) {
                            let answer = question_ui::answer_json(&questions, draft);
                            self.answer_question(&request_id, Some(answer));
                        }
                    }
                    "skip" => self.answer_question(&request_id, None),
                    _ => return false,
                }
            }
            _ => return false,
        }
        ctx.request_repaint();
        true
    }

    /// `answer` is the user's answer; `None` is the user skipping the question.
    fn answer_question(&mut self, request_id: &str, answer: Option<String>) {
        let (Some(session_id), Some(store)) =
            (self.view.selected_session.clone(), self.store.clone())
        else {
            self.view.status = "El historial no está disponible".to_owned();
            return;
        };
        let recorded = match &answer {
            Some(answer) => store.record_user_answer(&session_id, request_id, answer),
            None => {
                store.record_permission_decision(&session_id, request_id, PermissionDecision::Deny)
            }
        };
        if let Err(error) = recorded {
            self.persistence_error = Some(error.to_string());
            self.show_persistence_error();
            return;
        }
        let choice = if answer.is_some() {
            PermissionChoice::Allow
        } else {
            PermissionChoice::Deny
        };
        let resolved = self
            .active_run
            .as_ref()
            .is_some_and(|run| run.resolve_permission(request_id, choice))
            || self
                .active_resume
                .as_ref()
                .is_some_and(|resume| resume.resolve_permission(request_id, choice));
        self.view.status = if !resolved {
            "La pregunta ya no está activa".to_owned()
        } else if answer.is_some() {
            "Respuesta enviada".to_owned()
        } else {
            "Pregunta omitida".to_owned()
        };
        self.view
            .pending_permissions
            .retain(|permission| permission.request_id != request_id);
        self.view.question_drafts.remove(request_id);
    }

    /// The user's decision on a proposed mode. Accepting only remembers the
    /// mode for this session: it is set by `select_composer_mode`, the path
    /// the composer's own control uses, when the turn is over. Nothing here
    /// touches permissions.
    fn decide_mode_proposal(&mut self, request_id: &str, accept: bool) {
        let Some(mode) = self
            .view
            .pending_permissions
            .iter()
            .find(|permission| {
                permission.request_id == request_id
                    && permission.tool_name == crate::mode_proposal_ui::TOOL
            })
            .and_then(|permission| {
                crate::mode_proposal_ui::proposed(
                    &serde_json::from_str(&permission.arguments).unwrap_or(serde_json::Value::Null),
                )
            })
        else {
            return;
        };
        let session_id = self.view.selected_session.clone();
        self.answer_permission(request_id, accept);
        self.view
            .pending_permissions
            .retain(|permission| permission.request_id != request_id);
        self.view.status = match (accept, mode) {
            (true, TurnMode::Plan) => "Modo Plan desde tu próximo mensaje",
            (true, TurnMode::Build) => "Modo Agente desde tu próximo mensaje",
            (false, _) => "Propuesta de modo rechazada",
        }
        .to_owned();
        if let (true, Some(session_id)) = (accept, session_id) {
            self.accepted_turn_mode = Some((session_id, mode));
        }
    }

    /// Sets an accepted mode once the composer accepts a mode change again,
    /// which is when no turn is running. A different session drops it.
    pub(super) fn apply_accepted_turn_mode(&mut self) -> bool {
        let Some((session_id, mode)) = self.accepted_turn_mode.clone() else {
            return false;
        };
        if self.view.selected_session.as_deref() != Some(session_id.as_str()) {
            self.accepted_turn_mode = None;
            return false;
        }
        if !self.composer_policy_editable() {
            return false;
        }
        self.accepted_turn_mode = None;
        self.select_composer_mode(mode);
        true
    }

    /// With the focus on the card: a number picks that option and moves to
    /// the confirming button, the arrows walk the options and the questions.
    /// Enter then confirms through the focused button. Typing in the answer
    /// field is left alone.
    pub(super) fn handle_question_keys(&mut self, ctx: &egui::Context) -> bool {
        let Some(focused) = self.tree.focused_id().map(str::to_owned) else {
            return false;
        };
        let Some(rest) = focused
            .strip_prefix("question_")
            .filter(|rest| !rest.starts_with("text:"))
        else {
            return false;
        };
        let Some(request_id) = rest
            .split_once(':')
            .map(|(_, target)| target.split(':').next().unwrap_or_default().to_owned())
        else {
            return false;
        };
        let Some(questions) = self.pending_question(&request_id) else {
            return false;
        };
        let page = self
            .view
            .question_drafts
            .get(&request_id)
            .map_or(0, |draft| draft.page)
            .min(questions.len().saturating_sub(1));
        let options = questions[page].options.len();
        let focused_option = focused
            .strip_prefix(&format!("question_pick:{request_id}:{page}:"))
            .and_then(|option| option.parse::<usize>().ok());
        let mut keys = Vec::new();
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let known = modifiers.is_none()
                    && matches!(
                        key,
                        egui::Key::Num1
                            | egui::Key::Num2
                            | egui::Key::Num3
                            | egui::Key::Num4
                            | egui::Key::ArrowUp
                            | egui::Key::ArrowDown
                            | egui::Key::ArrowLeft
                            | egui::Key::ArrowRight
                    );
                if known {
                    keys.push(*key);
                }
                !known
            });
        });
        let last = page + 1 == questions.len();
        let confirm = question_ui::action(&request_id, if last { "send" } else { "next" });
        for key in &keys {
            let number = match key {
                egui::Key::Num1 => Some(0),
                egui::Key::Num2 => Some(1),
                egui::Key::Num3 => Some(2),
                egui::Key::Num4 => Some(3),
                _ => None,
            };
            let (click, focus) = match (key, number) {
                (_, Some(option)) if option < options => (
                    Some(question_ui::pick_action(&request_id, page, option)),
                    // Several choices stay on the options; one moves on.
                    (!questions[page].multi_select).then(|| confirm.clone()),
                ),
                (egui::Key::ArrowDown | egui::Key::ArrowUp, _) => {
                    let next = match (focused_option, key) {
                        (Some(option), egui::Key::ArrowDown) => (option + 1).min(options - 1),
                        (Some(option), _) => option.saturating_sub(1),
                        (None, egui::Key::ArrowDown) => 0,
                        (None, _) => options - 1,
                    };
                    (None, Some(question_ui::pick_action(&request_id, page, next)))
                }
                (egui::Key::ArrowRight, _) if !last => {
                    (Some(question_ui::action(&request_id, "next")), None)
                }
                (egui::Key::ArrowLeft, _) if page > 0 => {
                    (Some(question_ui::action(&request_id, "back")), None)
                }
                _ => (None, None),
            };
            if let Some(id) = click {
                self.apply_question_action(&UiAction::Clicked { id }, ctx);
            }
            if let Some(id) = focus {
                self.question_focus_pending = Some(id);
            }
        }
        !keys.is_empty()
    }

    /// Focus asked for by a key, applied once the rebuilt card has the target.
    pub(super) fn apply_question_focus(&mut self) {
        if let Some(id) = self.question_focus_pending.take() {
            if !self.tree.focus(&id) {
                self.question_focus_pending = Some(id);
            }
        }
    }
}

#[cfg(test)]
#[path = "question_app_tests.rs"]
mod tests;
