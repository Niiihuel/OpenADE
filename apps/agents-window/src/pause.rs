//! Provider retries and limit pauses as the window shows them.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use openide_core::TurnPauseClass;
use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, Element, Icon, Notice, Text, TextRole,
};

use crate::theme;

pub(crate) fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// A provider request waiting to be sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelRetry {
    pub attempt: u32,
    pub max_attempts: u32,
    pub until_ms: u64,
}

/// A turn stopped at a model boundary for a known cause, read from replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PausedTurn {
    pub class: TurnPauseClass,
    pub limit_key: String,
    pub resets_at_ms: Option<u64>,
    /// The provider's raw text: shown only behind "Detalle".
    pub detail: String,
    pub provider_id: String,
    /// The session that spawned this one, when it is a subagent.
    pub parent: Option<String>,
}

impl PausedTurn {
    pub fn from_replay(
        replay: &openide_core::NativeReplay,
        session: &openide_core::Session,
    ) -> Option<Self> {
        let pause = replay.pause.as_ref()?;
        // Only the two clean model boundaries are a pause. Anything else is
        // a recovery a person has to look at.
        matches!(
            replay.recovery,
            openide_core::ReplayRecovery::ReadyForModel { .. }
                | openide_core::ReplayRecovery::ModelInFlight { .. }
        )
        .then(|| Self {
            class: pause.class,
            limit_key: pause.limit_key.clone(),
            resets_at_ms: pause.resets_at_ms,
            detail: pause.detail.clone(),
            provider_id: replay
                .history_model_route
                .as_ref()
                .map(|route| route.provider_id.clone())
                .unwrap_or_default(),
            parent: session
                .subagent
                .as_ref()
                .map(|link| link.parent_session_id.clone()),
        })
    }

    fn provider(&self) -> &str {
        match self.provider_id.as_str() {
            "codex" => "ChatGPT",
            "openai" => "OpenAI",
            "anthropic" => "Anthropic",
            "openrouter" => "OpenRouter",
            "zai" => "Z.ai",
            "minimax" => "MiniMax",
            "deepseek" => "DeepSeek",
            "groq" => "Groq",
            "" => "el proveedor",
            other => other,
        }
    }

    /// The one line of the card and the composer. Never the provider's text.
    pub fn headline(&self) -> String {
        let provider = self.provider();
        match self.class {
            TurnPauseClass::LimitReached => format!("Límite de uso alcanzado · {provider}"),
            TurnPauseClass::OutOfCredits => format!("Sin crédito en {provider}"),
            TurnPauseClass::Auth => format!("Reconectá {provider}"),
            TurnPauseClass::Transient => format!("{provider} no responde"),
            TurnPauseClass::WaitingOnChild => "Esperando a un subagente en pausa".into(),
        }
    }

    pub fn account_url(&self) -> Option<&'static str> {
        (self.class == TurnPauseClass::OutOfCredits)
            .then(|| crate::provider_usage::account_url(&self.provider_id))
            .flatten()
    }
}

/// `03:10` in the machine's time zone.
pub(crate) fn clock_label(at_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms as i64)
        .map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// `2 h 14 min`, `14 min`, `45 s`.
pub(crate) fn remaining_label(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3599 => format!("{} min", seconds.div_ceil(60)),
        _ => format!("{} h {} min", seconds / 3600, seconds % 3600 / 60),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PauseView {
    /// Wall clock of the last frame that had a countdown to show.
    pub clock_ms: u64,
    pub retry: Option<ModelRetry>,
    /// Paused turns by session, rebuilt from the journals.
    pub sessions: BTreeMap<String, PausedTurn>,
    /// The session whose provider text is unfolded.
    pub detail_open: Option<String>,
    /// The paused session whose "Cambiar de modelo" note is showing.
    pub model_note: Option<String>,
}

impl PauseView {
    /// Whether a countdown is visible, so the frame loop keeps the clock.
    pub fn counting(&self) -> bool {
        self.retry.is_some() || self.sessions.values().any(|pause| pause.resets_at_ms.is_some())
    }

    /// When the limit renews, and how long that is from now.
    pub fn reset_line(&self, pause: &PausedTurn) -> Option<String> {
        let at = pause.resets_at_ms?;
        Some(match self.seconds_until(at) {
            0 => format!("se renovó a las {}", clock_label(at)),
            left => format!("se renueva a las {} · faltan {}", clock_label(at), remaining_label(left)),
        })
    }

    /// Paused turns that share this one's provider limit, itself included.
    pub fn sharing_limit(&self, pause: &PausedTurn) -> usize {
        self.sessions
            .values()
            .filter(|other| !pause.limit_key.is_empty() && other.limit_key == pause.limit_key)
            .count()
            .max(1)
    }

    /// The pause card at the end of a paused turn's transcript.
    pub fn card(&self, session_id: &str, pause: &PausedTurn, compact: bool) -> Element {
        let mut lines = Vec::new();
        lines.extend(self.reset_line(pause));
        match pause.class {
            TurnPauseClass::LimitReached if pause.resets_at_ms.is_none() => {
                lines.push("El proveedor no informó cuándo se renueva.".into())
            }
            TurnPauseClass::OutOfCredits => {
                lines.push("Continuá cuando la cuenta vuelva a tener crédito.".into())
            }
            TurnPauseClass::Auth => {
                lines.push("Volvé a conectar la cuenta en Ajustes y continuá.".into())
            }
            TurnPauseClass::Transient => {
                lines.push("Se agotaron los reintentos. El turno quedó intacto.".into())
            }
            TurnPauseClass::WaitingOnChild => {
                lines.push("Continuará cuando el subagente retome su trabajo.".into())
            }
            TurnPauseClass::LimitReached => {}
        }
        let sharing = self.sharing_limit(pause);
        if sharing > 1 {
            lines.push(format!("{sharing} agentes en pausa por este límite."));
        }
        if self.model_note.as_deref() == Some(session_id) {
            lines.push(
                "Para usar otro modelo, cancelá este turno o dejá que continúe: un turno en curso no cambia de modelo."
                    .into(),
            );
        }
        let subtle = |id: String, label: &str| {
            Button::new(id, label, theme::action_palette(theme::HOVER, theme::FOREGROUND))
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Compact)
                .render()
        };
        let mut actions = vec![
            theme::primary(format!("pause_continue:{session_id}"), "Continuar ahora")
                .size(ControlSize::Compact)
                .render(),
            subtle(format!("pause_model:{session_id}"), "Cambiar de modelo"),
        ];
        if pause.account_url().is_some() {
            actions.push(subtle(format!("pause_account:{session_id}"), "Abrir cuenta"));
        }
        actions.push(
            Button::new(
                format!("pause_cancel:{session_id}"),
                "Cancelar",
                theme::action_palette(theme::HOVER, theme::DESTRUCTIVE),
            )
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Compact)
            .render(),
        );
        let detail_open = self.detail_open.as_deref() == Some(session_id);
        actions.push(subtle(
            format!("pause_detail:{session_id}"),
            if detail_open { "Ocultar detalle" } else { "Detalle" },
        ));
        // A phone-wide transcript cannot hold five buttons on one line.
        let actions = if compact {
            let rest = actions.split_off(2);
            Element::column(vec![Element::row(actions).gap(6.0), Element::row(rest).gap(6.0)]).gap(6.0)
        } else {
            Element::row(actions).gap(6.0)
        };
        let mut body = vec![actions];
        if detail_open {
            body.push(
                Text::new(&pause.detail, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .mono()
                    .render(),
            );
        }
        Notice::new(pause.headline(), theme::component_palette())
            .icon(if pause.resets_at_ms.is_some() { Icon::Clock } else { Icon::Refresh })
            .description(lines.join("\n"))
            .framed()
            .stacked()
            .action(Element::column(body).gap(8.0))
            .render()
    }

    fn seconds_until(&self, at_ms: u64) -> u64 {
        at_ms.saturating_sub(self.clock_ms).div_ceil(1000)
    }

    pub fn retry_label(&self) -> Option<String> {
        self.retry.map(|retry| {
            format!(
                "Reintentando en {}s ({}/{})",
                self.seconds_until(retry.until_ms),
                retry.attempt,
                retry.max_attempts
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_core::{
        AgentEvent, AgentEventKind, AgentMessage, CheckpointCompatibility, SessionStore, ToolCall,
        TurnMode,
    };

    fn replay(store: &SessionStore, id: &str) -> openide_core::NativeReplay {
        let stored = store.load(id).unwrap().unwrap();
        openide_core::replay_native(
            id,
            &stored.events,
            None,
            &CheckpointCompatibility {
                provider: "fixture".into(),
                model: "m".into(),
                context_format_key: "v1".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn a_journal_reloaded_after_a_restart_keeps_its_pause_and_an_uncertain_tool_is_never_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let session = store.create(dir.path(), "native").unwrap();
        let admitted = store
            .admit_native_turn(&session.id, "turn-1", "Hola", "m", TurnMode::Build)
            .unwrap();
        let mut sequence = admitted.sequence;
        let mut append = |store: &SessionStore, kind| {
            sequence += 1;
            store
                .append_native_event(&AgentEvent::new(&session.id, sequence, kind))
                .unwrap();
        };
        append(&store, AgentEventKind::TurnStarted { prompt: "Hola".into() });
        append(&store, AgentEventKind::ModelRequestStarted { step: 1, model: "m".into() });
        // A journal written before pauses existed: a plain interrupted turn.
        assert!(PausedTurn::from_replay(&replay(&store, &session.id), &session).is_none());
        append(
            &store,
            AgentEventKind::TurnPaused {
                class: TurnPauseClass::LimitReached,
                limit_key: "k".into(),
                resets_at_ms: Some(7_000),
                step: 1,
                detail: "HTTP 429: raw".into(),
            },
        );
        // A new process opens the same directory and reads the same pause.
        let reopened = SessionStore::open(dir.path()).unwrap();
        let pause = PausedTurn::from_replay(&replay(&reopened, &session.id), &session).unwrap();
        assert_eq!((pause.class, pause.resets_at_ms, pause.parent.as_deref()), (TurnPauseClass::LimitReached, Some(7_000), None));
        assert_eq!(pause.headline(), "Límite de uso alcanzado · el proveedor");
        assert!(!pause.headline().contains("HTTP"));
        // The response commits and a tool starts without finishing: that
        // is a recovery for a person, with no pause mark left.
        let call = ToolCall { id: "c".into(), name: "write".into(), arguments: serde_json::json!({}) };
        append(
            &reopened,
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("", vec![call.clone()]),
                response_id: None,
                usage: None,
            },
        );
        append(&reopened, AgentEventKind::ToolStarted { tool_call: call });
        let state = replay(&reopened, &session.id);
        assert!(matches!(state.recovery, openide_core::ReplayRecovery::ToolOutcomeUncertain { .. }));
        assert!(PausedTurn::from_replay(&state, &session).is_none());
    }

    #[test]
    fn countdown_labels_read_in_plain_units() {
        assert_eq!(remaining_label(45), "45 s");
        assert_eq!(remaining_label(61), "2 min");
        assert_eq!(remaining_label(8_040), "2 h 14 min");
        let view = PauseView { clock_ms: 10_000, ..Default::default() };
        assert_eq!(view.seconds_until(10_001), 1);
        assert_eq!(view.seconds_until(9_000), 0);
    }
}
