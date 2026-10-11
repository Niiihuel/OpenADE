//! Continuing a paused conversation on another model.
//!
//! The paused turn is closed with `TurnSuperseded` and a new turn starts on
//! the chosen route through the ordinary new-turn path. Paused subagents move
//! with it, leaves first; nothing that finished is touched or run again.

use std::collections::{BTreeMap, VecDeque};

use openide_core::{AgentEvent, AgentEventKind, TurnMode, UserImage};
use openide_ui::{AlertDialog, Component, Element, Icon, Notice, Text, TextRole};

use crate::{theme, view::ViewState};

/// Why the switch cannot go ahead as it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SwitchBlocker {
    /// The message that would be sent again carries images the model cannot see.
    Images { count: usize },
    /// The route cannot take this turn at all; the text says why.
    Refused(String),
}

/// A switch waiting for the person's confirmation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SwitchConfirm {
    pub session_id: String,
    pub route_id: String,
    pub model_name: String,
    /// Paused agents that move to the model, this one included.
    pub moving: usize,
    /// Agents of the tree that keep working on their own model.
    pub working: usize,
    /// The session is a subagent: only it and its own subagents move.
    pub child: bool,
    pub notices: Vec<String>,
    pub blocker: Option<SwitchBlocker>,
}

/// A switch that closed the old turn and has not started the new one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnfinishedSwitch {
    pub provider_id: String,
    pub model_id: String,
    pub next_turn_id: String,
    pub host_prompt: bool,
    pub error: Option<String>,
}

/// The turn the host starts to continue a superseded one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HostTurn {
    pub turn_id: String,
    pub prompt: String,
    pub images: Vec<UserImage>,
    pub mode: TurnMode,
}

/// One thing left to do for a switch in progress.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SwitchStep {
    ResumeChild(String),
    StartRoot { session_id: String, drop_images: bool },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SwitchView {
    pub confirm: Option<SwitchConfirm>,
    pub unfinished: BTreeMap<String, UnfinishedSwitch>,
    /// Model a fallback proposal names, by paused session.
    pub proposals: BTreeMap<String, (String, String)>,
    /// Settings: ask before summarising for a newly chosen model.
    pub ask_before_summary: bool,
}

pub(crate) type SwitchQueue = VecDeque<SwitchStep>;

/// The switch the journal left open, if its continuation was never admitted.
pub(crate) fn unfinished_switch(events: &[AgentEvent]) -> Option<UnfinishedSwitch> {
    events
        .iter()
        .rev()
        .find(|event| {
            !matches!(
                event.kind,
                AgentEventKind::HostNotice { .. } | AgentEventKind::CheckpointCommitted { .. }
            )
        })
        .and_then(|event| match &event.kind {
            AgentEventKind::TurnSuperseded {
                provider_id,
                model_id,
                next_turn_id,
                host_prompt,
            } => Some(UnfinishedSwitch {
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                next_turn_id: next_turn_id.clone(),
                host_prompt: *host_prompt,
                error: None,
            }),
            _ => None,
        })
}

/// What the turn that continues `events`' superseded turn opens with: the
/// host's continuation, or that turn's own prompt and images when it never
/// got a response.
pub(crate) fn host_turn(events: &[AgentEvent], switch: &UnfinishedSwitch) -> Option<HostTurn> {
    let (prompt, images, mode) = events.iter().rev().find_map(|event| match &event.kind {
        AgentEventKind::TurnAdmitted { prompt, mode, .. } => Some((prompt.clone(), Vec::new(), *mode)),
        AgentEventKind::TurnAdmittedWithImages { prompt, mode, images, .. } => {
            Some((prompt.clone(), images.clone(), *mode))
        }
        _ => None,
    })?;
    Some(if switch.host_prompt {
        HostTurn {
            turn_id: switch.next_turn_id.clone(),
            prompt: openide_harness::MODEL_SWITCH_CONTINUATION.to_owned(),
            images: Vec::new(),
            mode,
        }
    } else {
        HostTurn {
            turn_id: switch.next_turn_id.clone(),
            prompt,
            images,
            mode,
        }
    })
}

impl SwitchConfirm {
    fn lines(&self) -> Vec<String> {
        let model = &self.model_name;
        let mut lines = vec![match (self.child, self.moving) {
            (true, 1) => format!("Este subagente continúa en {model} desde donde quedó."),
            (true, moving) => format!("Este subagente y {} más pasan a {model}.", moving - 1),
            (false, 1) => format!("La conversación continúa en {model} desde donde quedó."),
            (false, moving) => format!("{moving} agentes en pausa pasan a {model}."),
        }];
        if self.working > 0 {
            lines.push(match self.working {
                1 => "1 agente sigue trabajando en su modelo actual.".to_owned(),
                working => format!("{working} agentes siguen trabajando en su modelo actual."),
            });
        }
        lines.push("Las herramientas que ya terminaron no se repiten.".to_owned());
        lines.extend(self.notices.iter().cloned());
        lines
    }
}

/// The confirmation, or the blocker that has to be resolved first.
pub(crate) fn confirm_dialog(base: Element, state: &ViewState, width: f32) -> Element {
    let Some(confirm) = &state.model_switch.confirm else {
        return base;
    };
    let model = &confirm.model_name;
    let small = |text: &str| Text::new(text, theme::component_palette()).role(TextRole::Small).render();
    let dialog = match &confirm.blocker {
        Some(SwitchBlocker::Images { count }) => AlertDialog::new(
            base,
            true,
            format!("{model} no puede ver imágenes"),
            match count {
                1 => "El mensaje que se envía de nuevo lleva 1 imagen.".to_owned(),
                count => format!("El mensaje que se envía de nuevo lleva {count} imágenes."),
            },
            "model_switch_without_images",
            "model_switch_other",
            theme::component_palette(),
        )
        .confirm_label("Enviar sin imágenes")
        .cancel_label("Elegir otro modelo"),
        Some(SwitchBlocker::Refused(reason)) => AlertDialog::new(
            base,
            true,
            format!("No se puede continuar en {model}"),
            reason.clone(),
            "model_switch_refused",
            "model_switch_other",
            theme::component_palette(),
        )
        .confirm_label("Continuar")
        .confirm_enabled(false)
        .cancel_label("Elegir otro modelo"),
        None => AlertDialog::new(
            base,
            true,
            format!("Continuar en {model}"),
            "El turno en pausa se cierra y sigue en el modelo elegido.",
            "model_switch_confirm",
            "model_switch_cancel",
            theme::component_palette(),
        )
        .confirm_label("Continuar")
        .body(Element::column(confirm.lines().iter().map(|line| small(line)).collect()).gap(4.0)),
    };
    dialog.width((width - 24.0).min(480.0)).render()
}

impl SwitchView {
    /// Shown in place of the pause card while a switch is half done.
    pub fn unfinished_card(&self, session_id: &str) -> Option<Element> {
        let switch = self.unfinished.get(session_id)?;
        let mut lines = vec!["El turno anterior ya se cerró; falta iniciar el que lo continúa.".to_owned()];
        lines.extend(switch.error.clone());
        Some(
            Notice::new(
                format!("El cambio a {} no terminó", switch.model_id),
                theme::component_palette(),
            )
            .icon(Icon::Refresh)
            .description(lines.join("\n"))
            .framed()
            .stacked()
            .action(
                theme::primary(
                    format!("model_switch_finish:{session_id}"),
                    format!("Continuar en {}", switch.model_id),
                )
                .size(openide_ui::ControlSize::Compact)
                .render(),
            )
            .render(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, kind: AgentEventKind) -> AgentEvent {
        AgentEvent::new("s", sequence, kind)
    }

    #[test]
    fn the_continuation_is_the_hosts_text_or_the_unanswered_prompt_itself() {
        let superseded = |host_prompt| AgentEventKind::TurnSuperseded {
            provider_id: "p".into(),
            model_id: "m2".into(),
            next_turn_id: "next".into(),
            host_prompt,
        };
        let admitted = AgentEventKind::TurnAdmitted {
            turn_id: "t".into(),
            prompt: "Hacé esto".into(),
            model: "m1".into(),
            mode: TurnMode::Plan,
        };
        let mut events = vec![event(1, admitted), event(2, superseded(false))];
        let switch = unfinished_switch(&events).unwrap();
        let turn = host_turn(&events, &switch).unwrap();
        assert_eq!((turn.prompt.as_str(), turn.turn_id.as_str(), turn.mode), ("Hacé esto", "next", TurnMode::Plan));
        events[1] = event(2, superseded(true));
        // A notice after it does not hide the open switch.
        events.push(event(3, AgentEventKind::HostNotice { code: "x".into(), detail: "y".into() }));
        let switch = unfinished_switch(&events).unwrap();
        assert_eq!(host_turn(&events, &switch).unwrap().prompt, openide_harness::MODEL_SWITCH_CONTINUATION);
        // Once the next turn is admitted nothing is left open.
        events.push(event(4, AgentEventKind::TurnAdmitted {
            turn_id: "next".into(),
            prompt: "x".into(),
            model: "m2".into(),
            mode: TurnMode::Build,
        }));
        assert!(unfinished_switch(&events).is_none());
    }

    #[test]
    fn the_confirmation_says_who_moves_and_who_keeps_working() {
        let confirm = SwitchConfirm {
            session_id: "s".into(),
            route_id: "r".into(),
            model_name: "Luna".into(),
            moving: 3,
            working: 1,
            child: false,
            notices: vec!["aviso".into()],
            blocker: None,
        };
        assert_eq!(
            confirm.lines(),
            [
                "3 agentes en pausa pasan a Luna.",
                "1 agente sigue trabajando en su modelo actual.",
                "Las herramientas que ya terminaron no se repiten.",
                "aviso",
            ]
        );
    }
}
