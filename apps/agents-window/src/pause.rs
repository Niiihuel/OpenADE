//! Provider retries and limit pauses as the window shows them.

use std::collections::{BTreeMap, BTreeSet};
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
    pub paused_at_ms: u64,
    /// Pauses of this turn so far, this one included.
    pub pauses_in_turn: u32,
}

/// What the timer may do with a paused turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoResume {
    /// Continue by itself at this time.
    At(u64),
    /// The reset passed long ago, with the app closed or elsewhere: ask.
    Ask,
    /// Only a person continues it.
    Manual,
}

/// A turn continued automatically twice that pauses a third time stops
/// re-arming: something other than the clock is wrong.
const MAX_AUTO_REARMS: u32 = 2;
const MAX_AUTO_WAIT_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_LATE_MS: u64 = 30 * 60 * 1000;

/// Whether and when a paused turn continues by itself. Every doubt is Manual.
pub(crate) fn auto_resume(
    session_id: &str,
    pause: &PausedTurn,
    enabled: bool,
    cancelled: bool,
    now_ms: u64,
) -> AutoResume {
    let Some(resets_at) = pause.resets_at_ms else {
        return AutoResume::Manual;
    };
    if !enabled
        || cancelled
        // Credits, credentials and a silent provider do not fix themselves.
        || !matches!(pause.class, TurnPauseClass::LimitReached | TurnPauseClass::WaitingOnChild)
        || pause.pauses_in_turn > 1 + MAX_AUTO_REARMS
        || resets_at.saturating_sub(pause.paused_at_ms) > MAX_AUTO_WAIT_MS
    {
        return AutoResume::Manual;
    }
    if now_ms > resets_at.saturating_add(MAX_LATE_MS) {
        return AutoResume::Ask;
    }
    // A few seconds past the reset, spread so sessions do not fire together.
    let spread = session_id.bytes().fold(0u64, |sum, byte| sum.wrapping_mul(31).wrapping_add(u64::from(byte)));
    AutoResume::At(resets_at.saturating_add(5_000 + spread % 15_000))
}

/// `root` and its paused descendants in the order they must continue:
/// leaves first, then the parents waiting on them.
pub(crate) fn resume_order(sessions: &BTreeMap<String, PausedTurn>, root: &str) -> Vec<String> {
    let mut order = vec![root.to_owned()];
    let mut cursor = 0;
    // Bounded by the map: each session is appended once.
    while cursor < order.len() && order.len() <= sessions.len() {
        let parent = order[cursor].clone();
        order.extend(
            sessions
                .iter()
                .filter(|(id, pause)| pause.parent.as_deref() == Some(&parent) && !order.contains(id))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>(),
        );
        cursor += 1;
    }
    order.reverse();
    order
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
            paused_at_ms: pause.paused_at_ms,
            pauses_in_turn: pause.pauses_in_turn,
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
    /// Settings: continue by itself when the limit renews.
    pub auto_enabled: bool,
    /// Sessions whose automatic continuation the user cancelled with Esc.
    pub auto_cancelled: BTreeSet<String>,
    /// The pause (by its time) each session was last continued automatically
    /// for. One attempt per pause: a resume that fails without pausing anew
    /// must not be retried by the clock.
    pub auto_attempted: BTreeMap<String, u64>,
    /// `composer_line` of the selected session, kept for the composer to borrow.
    pub composer: Option<String>,
}

impl PauseView {
    /// Whether a countdown is visible, so the frame loop keeps the clock.
    pub fn counting(&self) -> bool {
        self.retry.is_some() || self.sessions.values().any(|pause| pause.resets_at_ms.is_some())
    }

    pub fn plan(&self, session_id: &str, pause: &PausedTurn) -> AutoResume {
        auto_resume(
            session_id,
            pause,
            self.auto_enabled,
            self.auto_cancelled.contains(session_id)
                || self.auto_attempted.get(session_id) == Some(&pause.paused_at_ms),
            self.clock_ms,
        )
    }

    /// When the limit renews, how long that is from now, and whether the
    /// turn continues by itself then.
    pub fn reset_line(&self, session_id: &str, pause: &PausedTurn) -> Option<String> {
        let at = pause.resets_at_ms?;
        let left = self.seconds_until(at);
        Some(match self.plan(session_id, pause) {
            AutoResume::At(_) if left > 0 => format!(
                "continúa automáticamente a las {} · faltan {}",
                clock_label(at),
                remaining_label(left)
            ),
            AutoResume::At(_) => "el límite se renovó · continuando".into(),
            AutoResume::Ask => format!("se renovó a las {} · continuá cuando quieras", clock_label(at)),
            AutoResume::Manual if self.auto_cancelled.contains(session_id) => format!(
                "se renueva a las {} · reanudación automática cancelada",
                clock_label(at)
            ),
            AutoResume::Manual if left > 0 => {
                format!("se renueva a las {} · faltan {}", clock_label(at), remaining_label(left))
            }
            AutoResume::Manual => format!("se renovó a las {}", clock_label(at)),
        })
    }

    /// The same state in one line, for the composer of the paused session.
    pub fn composer_line(&self, selected: Option<&str>) -> Option<String> {
        let session_id = selected?;
        let pause = self.sessions.get(session_id)?;
        // Short: the card above carries the cause in full.
        let mut line = match self.reset_line(session_id, pause) {
            Some(reset) => format!("En pausa · {reset}"),
            None => format!("En pausa · {}", pause.headline()),
        };
        if matches!(self.plan(session_id, pause), AutoResume::At(_)) {
            line.push_str(" · Esc cancela la reanudación automática");
        }
        Some(line)
    }

    /// `en pausa · 03:10` for the sidebar's task row.
    pub fn row_label(&self, session_id: &str) -> Option<String> {
        let pause = self.sessions.get(session_id)?;
        Some(match pause.resets_at_ms {
            Some(at) => format!("en pausa · {}", clock_label(at)),
            None => "en pausa".into(),
        })
    }

    /// `2 en pausa · 1 trabajando` for a tree whose agents are not all held.
    pub fn tree_line(&self, root: &str, working: usize) -> Option<String> {
        let paused = resume_order(&self.sessions, root).len();
        (paused > 1 || working > 0).then(|| {
            let paused = format!("{paused} en pausa");
            if working > 0 { format!("{paused} · {working} trabajando") } else { paused }
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
    /// `working` counts this session's subagents that are still running: a
    /// node the closed limit does not hold keeps going.
    pub fn card(&self, session_id: &str, pause: &PausedTurn, working: usize, compact: bool) -> Element {
        let mut lines = Vec::new();
        lines.extend(self.reset_line(session_id, pause));
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
        if let Some(tree) = self.tree_line(session_id, working) {
            lines.push(tree);
        } else if sharing > 1 {
            lines.push(format!("{sharing} agentes en pausa por este límite."));
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
        if pause.class == TurnPauseClass::Auth {
            actions.push(subtle(format!("pause_reconnect:{session_id}"), "Reconectar"));
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
        assert_eq!(pause.pauses_in_turn, 1);
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

    fn limit(paused_at_ms: u64, resets_at_ms: Option<u64>) -> PausedTurn {
        PausedTurn {
            class: TurnPauseClass::LimitReached,
            limit_key: "k".into(),
            resets_at_ms,
            detail: String::new(),
            provider_id: "codex".into(),
            parent: None,
            paused_at_ms,
            pauses_in_turn: 1,
        }
    }

    const HOUR: u64 = 3_600_000;

    #[test]
    fn auto_resume_waits_for_the_reset_and_gives_every_doubt_to_a_person() {
        let pause = limit(1_000 * HOUR, Some(1_002 * HOUR));
        let reset = 1_002 * HOUR;
        let plan = |pause: &PausedTurn, now| auto_resume("session", pause, true, false, now);
        // Armed a few seconds past the reset, never before it.
        let AutoResume::At(due) = plan(&pause, 1_000 * HOUR) else { panic!("armed") };
        assert!((reset + 5_000..reset + 20_000).contains(&due));
        // The app starts with the reset already in the past: a short delay
        // still continues, a long one asks instead.
        assert_eq!(plan(&pause, reset + 20 * 60_000), AutoResume::At(due));
        assert_eq!(plan(&pause, reset + 31 * 60_000), AutoResume::Ask);
        // Off in Settings, or cancelled with Esc.
        assert_eq!(auto_resume("session", &pause, false, false, reset), AutoResume::Manual);
        assert_eq!(auto_resume("session", &pause, true, true, reset), AutoResume::Manual);
        // More than a day away is a manual pause, also once that day passed.
        let far = limit(1_000 * HOUR, Some(1_025 * HOUR));
        assert_eq!(plan(&far, 1_000 * HOUR), AutoResume::Manual);
        assert_eq!(plan(&far, 1_025 * HOUR + 1), AutoResume::Manual);
        // Two automatic re-arms at most: the fourth pause of a turn stays put.
        let mut again = pause.clone();
        again.pauses_in_turn = 3;
        assert!(matches!(plan(&again, reset), AutoResume::At(_)));
        again.pauses_in_turn = 4;
        assert_eq!(plan(&again, reset), AutoResume::Manual);
        // No reset time, or a cause the clock does not fix: never.
        assert_eq!(plan(&limit(0, None), reset), AutoResume::Manual);
        for class in [TurnPauseClass::OutOfCredits, TurnPauseClass::Auth, TurnPauseClass::Transient] {
            let mut other = pause.clone();
            other.class = class;
            assert_eq!(plan(&other, reset), AutoResume::Manual, "{class:?}");
        }
        let mut waiting = pause.clone();
        waiting.class = TurnPauseClass::WaitingOnChild;
        assert!(matches!(plan(&waiting, reset), AutoResume::At(_)));
    }

    #[test]
    fn a_paused_tree_resumes_leaves_first_and_reads_as_one_group() {
        let child = |parent: &str| PausedTurn { parent: Some(parent.into()), ..limit(0, Some(HOUR)) };
        let mut view = PauseView { clock_ms: 0, auto_enabled: true, ..Default::default() };
        view.sessions.insert("root".into(), limit(0, Some(HOUR)));
        view.sessions.insert("a".into(), child("root"));
        view.sessions.insert("b".into(), child("root"));
        view.sessions.insert("a1".into(), child("a"));
        view.sessions.insert("other".into(), limit(0, None));
        assert_eq!(resume_order(&view.sessions, "root"), ["a1", "b", "a", "root"]);
        assert_eq!(resume_order(&view.sessions, "a"), ["a1", "a"]);
        assert_eq!(resume_order(&view.sessions, "other"), ["other"]);
        assert_eq!(view.sharing_limit(&view.sessions["root"]), 5);
        assert_eq!(view.tree_line("root", 1).as_deref(), Some("4 en pausa · 1 trabajando"));
        assert_eq!(view.tree_line("other", 0), None);
        assert!(view.row_label("root").unwrap().starts_with("en pausa · "));
        assert_eq!(view.row_label("other").as_deref(), Some("en pausa"));
        // Esc cancels the automatic continuation, never the pause.
        let line = view.composer_line(Some("root")).unwrap();
        assert!(line.contains("continúa automáticamente") && line.contains("Esc cancela"), "{line}");
        view.auto_cancelled.insert("root".into());
        let line = view.composer_line(Some("root")).unwrap();
        assert!(line.contains("reanudación automática cancelada") && !line.contains("Esc"), "{line}");
        // One automatic attempt per pause: only a new pause arms the clock again.
        view.auto_cancelled.clear();
        view.clock_ms = HOUR + 60_000;
        assert!(matches!(view.plan("root", &view.sessions["root"]), AutoResume::At(_)));
        view.auto_attempted.insert("root".into(), 0);
        assert_eq!(view.plan("root", &view.sessions["root"]), AutoResume::Manual);
        let mut again = view.sessions["root"].clone();
        again.paused_at_ms = 2 * HOUR;
        again.resets_at_ms = Some(3 * HOUR);
        again.pauses_in_turn = 2;
        assert!(matches!(view.plan("root", &again), AutoResume::At(_)));
        assert!(view.sessions.contains_key("root"));
        assert_eq!(view.composer_line(Some("none")), None);
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
