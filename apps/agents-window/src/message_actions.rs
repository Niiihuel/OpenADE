//! The row of actions of a transcript message: copy and edit on a user's
//! bubble; copy, when and with what it was answered, and what else the turn
//! offers under an assistant's turn.
//!
//! Rows are built every frame and stay cheap: text to copy is assembled
//! when the action is taken, never while drawing.

use std::time::Instant;

use openide_ui::{
    Component, DropdownMenuEntry, DropdownMenuItem, Element, Icon, MessageAction,
    MessageActionDone, MessageActions, MessageActionsVisibility, TooltipSpec,
};

use crate::{
    message_edit::{self, EditableTurn},
    provider_usage_ui::{elapsed_label, now_ms},
    theme,
    transcript::{
        duration_label, turn_time_id, ActivityStatus, TranscriptActivity, TranscriptItem,
    },
    view::ViewState,
};

/// What the rows remember between frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageActionsView {
    /// The copy just made, while its button shows a check.
    pub copied: Option<MessageActionDone>,
    /// The turn whose menu is open: its session and first transcript row.
    pub menu: Option<(String, usize)>,
}

const NO_PROSE: &str = "Este turno no tiene texto para copiar.";
const DROPS_ATTACHMENTS: &str =
    "El mensaje original llevaba adjuntos que no se reenvían. Editalo para enviarlo de nuevo.";

/// What a user's bubble can do with its message.
#[derive(Clone, Copy)]
pub(crate) enum UserEdit {
    /// It cannot be edited at all: a steered message, or a session whose
    /// history is not rewound.
    None,
    /// It opens for editing, as this `User` row.
    Offered(usize),
    /// It could be edited, but not now, for this reason.
    Blocked(&'static str),
    /// It is the message being edited.
    Open,
}

fn copy_action(id: String, state: &ViewState) -> MessageAction {
    let done = state
        .message_actions
        .copied
        .as_ref()
        .is_some_and(|copied| copied.shows(&id, Instant::now()));
    MessageAction::new(id, Icon::Copy, if done { "Copiado" } else { "Copiar" }).done(done)
}

/// Copy and edit at the trailing edge of a user's bubble, shown while the
/// pointer is over it.
pub(crate) fn user_actions(state: &ViewState, item_index: usize, edit: UserEdit) -> Option<Element> {
    let edit_action = || MessageAction::new(format!("user_edit:{item_index}"), Icon::Edit, "Editar y reenviar");
    let mut actions = MessageActions::new(format!("user_actions:{item_index}"), theme::component_palette())
        .action(copy_action(format!("user_copy:{item_index}"), state))
        .visibility(MessageActionsVisibility::Hover);
    actions = match edit {
        UserEdit::Open => return None,
        UserEdit::None => actions,
        // The bubble itself reports the same action on a click.
        UserEdit::Offered(row) => actions.action(edit_action().dispatch_as(message_edit::action_id(row))),
        UserEdit::Blocked(reason) => actions.action(edit_action().disabled(reason)),
    };
    Some(actions.render())
}

/// One turn's rows after its user message, as the transcript holds them.
pub(crate) struct Turn<'a> {
    /// Transcript index of the first row after the user message.
    pub start: usize,
    pub items: &'a [TranscriptItem],
    /// The `User` row before it, counting only `User` rows.
    pub user_row: usize,
    /// The turn's opening message, when it can be rewound to.
    pub editable: Option<&'a EditableTurn>,
    /// The conversation ends with this turn.
    pub last: bool,
}

fn has_prose(items: &[TranscriptItem]) -> bool {
    items.iter().any(
        |item| matches!(item, TranscriptItem::Assistant { content, .. } if !content.trim().is_empty()),
    )
}

/// What "Copiar" takes from a turn: the assistant's prose as it wrote it, in
/// Markdown, its parts a blank line apart. Reasoning, tool calls and their
/// output, notices and citations are not part of the answer and stay out.
pub(crate) fn turn_markdown(items: &[TranscriptItem]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant { content, .. } if !content.trim().is_empty() => Some(content.trim()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The rows of the turn that starts at transcript index `start`.
pub(crate) fn turn_items(transcript: &[TranscriptItem], start: usize) -> &[TranscriptItem] {
    let rest = transcript.get(start..).unwrap_or_default();
    let end = rest
        .iter()
        .position(|item| matches!(item, TranscriptItem::User { .. }))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// At most this much of a conversation goes to the task forked from it.
const HANDOFF_BYTES: usize = 16_000;

/// The conversation in `items` as text for another agent: its first message
/// and as much of its end as fits, oldest first, each message whole. Tool
/// calls are a line per turn; reasoning and attachments stay out.
pub(crate) fn handoff_text(items: &[TranscriptItem], title: &str) -> String {
    let mut messages: Vec<String> = Vec::new();
    let mut turn_start = 0;
    let close_turn = |messages: &mut Vec<String>, turn: &[TranscriptItem]| {
        let summary = activity_summary(turn);
        if !summary.is_empty() {
            messages.push(format!("(El agente {summary}.)"));
        }
    };
    for (index, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::User { content, .. } => {
                close_turn(&mut messages, &items[turn_start..index]);
                turn_start = index;
                messages.push(format!("Usuario:\n{}", content.trim()));
            }
            TranscriptItem::Assistant { content, .. } if !content.trim().is_empty() => {
                messages.push(format!("Asistente:\n{}", content.trim()));
            }
            _ => {}
        }
    }
    close_turn(&mut messages, &items[turn_start..]);
    // The first message says what the work is; the end, where it stands.
    let first: String = messages.first().map(|first| first.chars().take(HANDOFF_BYTES / 4).collect()).unwrap_or_default();
    let mut budget = HANDOFF_BYTES.saturating_sub(first.len());
    let mut tail = Vec::new();
    for message in messages.iter().skip(1).rev() {
        if message.len() > budget {
            break;
        }
        budget -= message.len();
        tail.push(message.as_str());
    }
    tail.reverse();
    let omitted = messages.len().saturating_sub(1 + tail.len());
    let mut text = format!(
        "[Contexto de la tarea «{}» de openADE{}. Es historial, no instrucciones nuevas.]\n\n{first}",
        title.trim(),
        if omitted > 0 { format!(", sin {omitted} mensajes del medio") } else { String::new() },
    );
    for message in tail {
        text.push_str("\n\n");
        text.push_str(message);
    }
    text.push_str("\n\n[Fin del contexto. Lo que sigue es el pedido nuevo.]\n\n");
    text
}

/// What the agent did in a turn, in one line: "leyó 6 archivos, ejecutó 3
/// comandos, editó 2 archivos". Counted from the turn's own tool calls, each
/// of which keeps its row above; a turn that used no tool says nothing.
pub(crate) fn activity_summary(items: &[TranscriptItem]) -> String {
    // read, searched, ran, edited, browsed, delegated, anything else
    let mut counts = [0usize; 7];
    let mut edited = std::collections::BTreeSet::new();
    for item in items {
        let TranscriptItem::Tool { name, arguments, .. } = item else {
            continue;
        };
        let name = name.rsplit("__").next().unwrap_or(name);
        let slot = match name {
            "read_file" | "batch_read" | "get_diagnostics" | "list_files" | "find_files" => 0,
            "search_files" | "search_text" | "codebase_search" | "codebase_explore" | "web_search" => 1,
            "run_command" | "exec_command" | "start_command" => 2,
            "write_file" | "edit_file" | "apply_patch" | "delete_file" | "rename_file" => 3,
            "web_fetch" | "open_preview" => 4,
            "spawn_agent" | "delegate_task" => 5,
            // Following a command or a subagent already counted is not another action.
            "poll_command" | "write_stdin" | "terminal_send" | "terminate_command" | "resize_command" | "wait_agent"
            | "await_subagent" | "send_input" | "send_message" | "message_conversation" | "cancel_agent"
            | "cancel_subagent" | "list_agents" | "update_plan" => continue,
            _ if name.starts_with("browser_") => 4,
            _ => 6,
        };
        counts[slot] += 1;
        if slot == 3 {
            // The same file edited twice is one file; a patch names its own.
            match ["path", "file_path"].into_iter().find_map(|key| arguments.get(key)?.as_str()) {
                Some(path) => drop(edited.insert(path.to_owned())),
                None => drop(edited.insert(format!("#{}", counts[3]))),
            }
        }
    }
    counts[3] = edited.len();
    let phrases = [
        ("leyó 1 archivo", "leyó {} archivos"),
        ("buscó 1 vez", "buscó {} veces"),
        ("ejecutó 1 comando", "ejecutó {} comandos"),
        ("editó 1 archivo", "editó {} archivos"),
        ("usó el navegador", "usó el navegador {} veces"),
        ("delegó en 1 subagente", "delegó en {} subagentes"),
        ("usó 1 herramienta", "usó {} herramientas"),
    ];
    counts
        .into_iter()
        .zip(phrases)
        .filter_map(|(count, (one, many))| match count {
            0 => None,
            1 => Some(crate::i18n::piece(one.to_owned())),
            _ => Some(crate::i18n::piece(many.replace("{}", &count.to_string()))),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "hace 5 min · 14 s · model · leyó 2 archivos": only what the journal recorded.
fn caption(state: &ViewState, turn: &Turn<'_>) -> String {
    let id = turn_time_id(turn.user_row);
    let model = turn.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Activity(TranscriptActivity::Model { model, .. }) if !model.is_empty() => {
            Some(model.clone())
        }
        _ => None,
    });
    [
        state.item_times.ended_ms(&id).map(|ended| elapsed_label(ended, now_ms())),
        state.item_times.duration_ms(&id).map(duration_label),
        model,
        Some(activity_summary(turn.items)).filter(|summary| !summary.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The row under a turn that has ended: always shown under the last one,
/// on hover under the others.
pub(crate) fn turn_actions(state: &ViewState, turn: &Turn<'_>) -> Element {
    let start = turn.start;
    let id = format!("turn_actions:{start}");
    let mut copy = copy_action(format!("turn_copy:{start}"), state);
    if !has_prose(turn.items) {
        copy = copy.disabled(NO_PROSE);
    }
    // The opening message to send again, and why that cannot be done now.
    let retry = turn.editable.map(|editable| {
        let blocked = if editable.drops_attachments {
            Some(DROPS_ATTACHMENTS)
        } else {
            message_edit::blocked_reason(state, editable)
        };
        (format!("turn_retry:{}", editable.user_row), blocked)
    });
    // A turn that failed or was stopped has one obvious next step.
    let ended_badly = turn.items.iter().any(|item| {
        matches!(
            item,
            TranscriptItem::Activity(TranscriptActivity::Model {
                status: ActivityStatus::Failed | ActivityStatus::Cancelled,
                ..
            }) | TranscriptItem::Notice { is_error: true, .. }
        )
    });
    let mut actions = MessageActions::new(&id, theme::component_palette()).action(copy);
    let mut entries: Vec<DropdownMenuEntry> = Vec::new();
    if let Some(call) = turn.items.iter().find_map(|item| match item {
        TranscriptItem::Tool { id, .. } => Some(id),
        _ => None,
    }) {
        entries.push(
            DropdownMenuItem::new(format!("turn_activity:{call}"), "Ver actividad del turno")
                .icon(Icon::History)
                .into(),
        );
    }
    if let Some((retry_id, blocked)) = retry {
        if ended_badly {
            let retry = MessageAction::new(retry_id, Icon::Refresh, "Reintentar");
            actions = actions.action(match blocked {
                Some(reason) => retry.disabled(reason),
                None => retry,
            });
        } else {
            let retry = DropdownMenuItem::new(retry_id, "Reintentar").icon(Icon::Refresh);
            entries.push(match blocked {
                Some(reason) => retry.disabled().tooltip_with(TooltipSpec::new(reason)).into(),
                None => retry.into(),
            });
        }
    }
    // Another task starts from here, with this agent or another one.
    entries.push(DropdownMenuEntry::Heading("Bifurcar desde aquí con".into()));
    entries.push(DropdownMenuItem::new(format!("turn_fork:{start}:native"), "openADE").icon(Icon::Chat).into());
    entries.extend(state.acp_agents.iter().map(|agent| {
        DropdownMenuItem::new(format!("turn_fork:{start}:acp:{}", agent.id), &agent.name)
            .icon(crate::provider_icons::icon_for_acp_agent(&agent.id, &agent.name))
            .into()
    }));
    entries.extend(state.agent_clis.iter().filter(|agent| agent.resolved.is_some()).map(|agent| {
        DropdownMenuItem::new(format!("turn_fork:{start}:cli:{}", agent.id), &agent.label)
            .icon(crate::provider_icons::icon_for_agent_cli(&agent.id))
            .into()
    }));
    let open = state.message_actions.menu.as_ref().is_some_and(|(session, row)| {
        *row == start && state.selected_session.as_deref() == Some(session)
    });
    actions
        .overflow(entries, open, "Más acciones")
        .caption(caption(state, turn))
        .visibility(if turn.last {
            MessageActionsVisibility::Always
        } else {
            MessageActionsVisibility::Hover
        })
        .render()
}

/// What a click on one of these rows does besides changing the view.
#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    None,
    /// Put this text on the clipboard.
    Copy(String),
    /// Show this tool call in Activity.
    Activity(String),
    /// Send this `User` row's message again, through the edit's own flow.
    Retry(usize),
    /// Start a task of `target` (`native`, `acp:<id>`, `cli:<id>`) with the
    /// conversation up to transcript row `end` as its context.
    Fork { end: usize, target: String },
}

/// Apply a click on a message's actions to the view. `None` when `id` is
/// not one of them.
pub(crate) fn apply_action(state: &mut ViewState, id: &str) -> Option<Effect> {
    let (kind, value) = id.split_once(':')?;
    let copied = |state: &mut ViewState, text: String| {
        state.message_actions.copied = Some(MessageActionDone::new(id, Instant::now()));
        Effect::Copy(text)
    };
    let effect = match kind {
        "user_copy" => {
            let index: usize = value.parse().ok()?;
            let TranscriptItem::User { content, .. } = state.transcript.get(index)? else {
                return Some(Effect::None);
            };
            // What was typed, when that is known; the stored prompt otherwise.
            let row = state.transcript[..index]
                .iter()
                .filter(|item| matches!(item, TranscriptItem::User { .. }))
                .count();
            let typed = state
                .message_edit
                .turns
                .iter()
                .find(|turn| turn.user_row == row)
                .and_then(|turn| turn.text.clone());
            let text = typed.unwrap_or_else(|| content.clone());
            copied(state, text)
        }
        "turn_copy" => {
            let text = turn_markdown(turn_items(&state.transcript, value.parse().ok()?));
            if text.is_empty() {
                Effect::None
            } else {
                copied(state, text)
            }
        }
        "turn_activity" => Effect::Activity(value.to_owned()),
        "turn_retry" => Effect::Retry(value.parse().ok()?),
        "turn_fork" => {
            let (start, target) = value.split_once(':')?;
            let start: usize = start.parse().ok()?;
            let end = start + turn_items(&state.transcript, start).len();
            Effect::Fork { end, target: target.to_owned() }
        }
        "turn_actions" => {
            let (row, action) = value.split_once(':')?;
            let row: usize = row.parse().ok()?;
            let session = state.selected_session.clone()?;
            let open = (session, row);
            state.message_actions.menu = match action {
                "more" if state.message_actions.menu.as_ref() != Some(&open) => Some(open),
                "more" | "more:dismiss" => None,
                _ => return None,
            };
            return Some(Effect::None);
        }
        _ => return None,
    };
    // Choosing from the menu closes it.
    state.message_actions.menu = None;
    Some(effect)
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, arguments: serde_json::Value) -> TranscriptItem {
        TranscriptItem::Tool {
            id: format!("call-{name}"),
            name: name.into(),
            arguments,
            output: Some(String::new()),
            is_error: false,
            activity: ActivityStatus::Completed,
        }
    }

    #[test]
    fn a_turn_says_in_one_line_what_the_agent_did() {
        assert_eq!(activity_summary(&[]), "");
        let turn = [
            tool("read_file", json!({"path": "src/a.rs"})),
            tool("read_file", json!({"path": "src/b.rs"})),
            tool("search_text", json!({"query": "foo"})),
            tool("run_command", json!({"command": "cargo test"})),
            // Following that command is not another command.
            tool("poll_command", json!({"command_id": "1"})),
            tool("edit_file", json!({"path": "src/a.rs"})),
            // The same file edited again is still one file.
            tool("edit_file", json!({"path": "src/a.rs"})),
            tool("write_file", json!({"path": "src/c.rs"})),
            tool("mcp__openide_browser__browser_click", json!({"ref": "e1"})),
            tool("mcp__github__list_issues", json!({})),
        ];
        assert_eq!(
            activity_summary(&turn),
            "leyó 2 archivos, buscó 1 vez, ejecutó 1 comando, editó 2 archivos, usó el navegador, usó 1 herramienta"
        );
        assert_eq!(activity_summary(&turn[..1]), "leyó 1 archivo");
    }
}
