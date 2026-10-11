//! Editing an earlier user message in place and resubmitting from it.
//!
//! The view owns no second composer: while a message is edited the one
//! composer is drawn over that message and the bottom of the surface shows a
//! stub. The host keeps the bottom draft in its draft map and restores it.

use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use openide_ui::{
    AlertDialog, Button, ButtonVariant, Card, Color, Component, ControlSize, Element, FontFamily,
    PopoverPlacement, RadiusRole, Spinner, Text, TextRole,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    composer_ui::{build_composer, ComposerProps},
    session_changes::{OpaqueEffects, RevertSkip},
    theme,
    view::ViewState,
};

/// What `start_run` appends to the text the user typed. A prompt holding
/// none of them is that text.
const APPENDED_BLOCKS: [&str; 3] = [
    "\n\nAdjuntos de este proyecto (rutas relativas): ",
    "\n\nImágenes enviadas al modelo en este orden: ",
    "\n\n[Contexto agregado por un hook]\n",
];
const TYPED_DIRECTORY: &str = "typed-prompts";
const MAX_TYPED_ENTRIES: usize = 512;
const MAX_TYPED_BYTES: u64 = 16 * 1024 * 1024;

/// A native turn's opening message as the transcript can offer it.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableTurn {
    /// Which `User` row it is, counting only `User` rows.
    pub user_row: usize,
    /// Sequence of the turn's admission: where a rewind cuts.
    pub sequence: u64,
    /// What the user typed. `None` when the stored prompt carries appended
    /// blocks and the typed text was not recorded: such a message is not
    /// editable, because stripping the blocks would be a guess.
    pub text: Option<String>,
    /// The original carried attachments or images. They are not resent.
    pub drops_attachments: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditingMessage {
    pub user_row: usize,
    pub sequence: u64,
    pub drops_attachments: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RewindConfirm {
    /// User messages after the edited one that the rewind removes.
    pub later_messages: usize,
    pub error: Option<String>,
    /// Paths "Revertir" would restore, as the preflight found them.
    pub restorable: Vec<String>,
    pub skipped: Vec<(String, RevertSkip)>,
    pub opaque: OpaqueEffects,
    /// Why the preflight itself could not be made.
    pub unavailable: Option<String>,
    /// A turn was running when this opened: confirming stops it first, and
    /// the lists above are only what the preflight finds so far.
    pub stops_turn: bool,
    /// Confirmed: waiting for that turn to stop before anything changes.
    pub stopping: bool,
    /// Made again after the turn stopped, and not what the choice was made on.
    pub recomputed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RewindUndoView {
    pub session_id: String,
    /// User messages the rewind removed, the edited one included.
    pub removed_messages: usize,
    pub error: Option<String>,
    /// Files the rewind put back to how they were before the removed turns.
    pub restored_files: usize,
    /// After a revert: paths it left alone. After an undo: restored paths
    /// that changed since and so kept their content.
    pub skipped_files: Vec<String>,
    /// The rewind was undone; the notice only reports `skipped_files`.
    pub undone: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageEditView {
    pub turns: Vec<EditableTurn>,
    pub editing: Option<EditingMessage>,
    pub confirm: Option<RewindConfirm>,
    pub undo: Option<RewindUndoView>,
}

/// The text behind a stored prompt: the recorded one, or the prompt itself
/// when nothing can have been appended to it.
pub(crate) fn typed_text(composed: &str, recorded: Option<&str>) -> Option<String> {
    match recorded {
        Some(typed) => Some(typed.to_owned()),
        None => (!APPENDED_BLOCKS.iter().any(|block| composed.contains(block)))
            .then(|| composed.to_owned()),
    }
}

pub(crate) fn carries_attachments(composed: &str, images: usize) -> bool {
    images > 0 || APPENDED_BLOCKS[..2].iter().any(|block| composed.contains(block))
}

/// Why a click on this turn's message cannot open it for editing, or
/// `None` when it can. A running turn of the session does not prevent it:
/// sending the edit stops that turn first. Nothing is offered while an
/// automation runs it, a permission waits, or another edit is open.
pub(crate) fn blocked_reason(state: &ViewState, turn: &EditableTurn) -> Option<&'static str> {
    if turn.text.is_none() {
        Some("Este mensaje lleva contexto agregado que no se puede separar de lo que escribiste.")
    } else if state.selected_backend != "native" {
        Some("Sólo las tareas nativas permiten editar un mensaje.")
    } else if state.background_run_selected {
        Some("Una automatización está ejecutando esta tarea.")
    } else if !state.pending_permissions.is_empty() {
        Some("Respondé el permiso pendiente antes de editar un mensaje anterior.")
    } else if state.message_edit.editing.is_some() {
        Some("Ya hay un mensaje en edición.")
    } else {
        None
    }
}

/// The turn a click on this row may edit.
pub(crate) fn offered(state: &ViewState, turn: &EditableTurn) -> bool {
    blocked_reason(state, turn).is_none()
}

pub(crate) fn action_id(user_row: usize) -> String {
    format!("message_edit:{user_row}")
}

/// A user message that opens for editing on a click, or Enter with focus.
pub(crate) fn clickable_bubble(card: Element, user_row: usize) -> Element {
    // The fill and radius repeat the card's, so the pinned cut fades into it.
    Element::clickable(action_id(user_row), card)
        .background(theme::card_palette().raised)
        .radius(RadiusRole::Card.px())
}

/// The message being edited: the composer floats over the bubble, which
/// keeps its place in the transcript, and everything else dims behind it.
/// Escape and a click outside report `message_edit_cancel`.
pub(crate) fn editor(state: &ViewState, bubble: Element, width: f32) -> Element {
    let Some(editing) = &state.message_edit.editing else {
        return bubble;
    };
    if state.message_edit.confirm.is_some() {
        // The confirmation is the modal surface now.
        return bubble;
    }
    let mut props = ComposerProps::from_state(state, width, false, None, "Editá el mensaje…", None);
    // This composer sends the edit, also while a turn runs. Everything
    // behind it is veiled, so the stop of that turn comes with it.
    props.running = false;
    let mut rows = vec![build_composer(&props)];
    if state.running {
        rows.push(
            Element::row(vec![
                Text::new(
                    "El turno sigue en curso. Enviar el mensaje editado lo detiene.",
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render()
                .centered_y()
                .flex(1.0),
                crate::composer_ui::stop_button("turn_stop", ControlSize::Dense, "Detener ejecución"),
            ])
            .gap(8.0),
        );
    }
    if editing.drops_attachments {
        rows.push(
            Text::new(
                "Los adjuntos del mensaje original no se reenvían. Volvé a adjuntar lo que necesites.",
                theme::component_palette(),
            )
            .role(TextRole::Small)
            .muted()
            .render(),
        );
    }
    let floating = Element::column(rows)
        .gap(6.0)
        .size(Some(width), None)
        .background(theme::BACKGROUND)
        .radius(RadiusRole::Card.px());
    let veil = theme::BACKGROUND;
    Element::column(vec![
        Element::popover(
            // A scroll area does not place an anchor of no height.
            Element::spacer().size(Some(width), Some(1.0)),
            floating,
            "message_edit_cancel",
            PopoverPlacement::BottomStart,
        )
        .popover_gap(0.0)
        .popover_scrim(Color::rgba(veil.r, veil.g, veil.b, 170))
        .popover_initial_focus("prompt"),
        bubble,
    ])
}

/// What stands where the composer was while a message is edited.
pub(crate) fn bottom_stub(width: f32) -> Element {
    Card::new(
        Element::row(vec![
            Text::new("Editando un mensaje anterior", theme::component_palette())
                .muted()
                .render()
                .flex(1.0),
            Button::new("message_edit_cancel", "Cancelar edición", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
        ])
        .gap(8.0),
        theme::card_palette(),
    )
    .border(false)
    .width(width)
    .render()
}

/// The notice a rewind leaves above the composer of its session.
pub(crate) fn undo_notice(state: &ViewState, width: f32) -> Option<Element> {
    let undo = state.message_edit.undo.as_ref()?;
    if state.selected_session.as_deref() != Some(undo.session_id.as_str())
        || state.message_edit.editing.is_some()
    {
        return None;
    }
    let summary = match undo.removed_messages {
        _ if undo.undone => "Rebobinado deshecho.".to_owned(),
        1 => "Se rebobinó la conversación: se quitó 1 mensaje con su respuesta.".to_owned(),
        count => format!("Se rebobinó la conversación: se quitaron {count} mensajes con sus respuestas."),
    };
    let mut copy = vec![Text::new(summary, theme::component_palette())
        .role(TextRole::Small)
        .render()];
    let mut files = Vec::new();
    if undo.restored_files > 0 && !undo.undone {
        files.push(format!("Se {}.", counted(undo.restored_files, "restauró 1 archivo", "restauraron", "archivos")));
    }
    if !undo.skipped_files.is_empty() {
        files.push(format!(
            "{}: {}.",
            if undo.undone { "No se volvieron a aplicar porque cambiaron después" } else { "Se omitieron" },
            listed(undo.skipped_files.iter().map(String::as_str), 3)
        ));
    }
    if !files.is_empty() {
        copy.push(Text::new(files.join(" "), theme::component_palette()).role(TextRole::Small).muted().render());
    }
    if let Some(error) = &undo.error {
        copy.push(
            Element::text(error)
                .font_size(12.0)
                .foreground(theme::DESTRUCTIVE),
        );
    }
    let busy = state.running || state.background_run_selected;
    let mut row = vec![Element::column(copy).gap(3.0).flex(1.0)];
    if !undo.undone {
        row.push(
            Button::new("message_rewind_undo", "Deshacer rebobinado", theme::component_palette())
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .enabled(!busy)
                .render(),
        );
    }
    row.push(
        Button::new("message_rewind_undo_dismiss", "Descartar", theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Compact)
            .render(),
    );
    Some(
        Card::new(
            Element::row(row).gap(8.0),
            theme::card_palette(),
        )
        .border(false)
        .width(width)
        .render(),
    )
}

/// "Submit from a previous message?" over the session surface.
pub(crate) fn confirm_dialog(base: Element, state: &ViewState, width: f32) -> Element {
    let Some(confirm) = &state.message_edit.confirm else {
        return base;
    };
    let small = |text: String| Text::new(text, theme::component_palette()).role(TextRole::Small).render();
    if confirm.stopping {
        return AlertDialog::new(
            base,
            true,
            "Deteniendo el turno…",
            "El mensaje editado se envía en cuanto el turno se detenga. Si cancelás ahora, el turno queda detenido y la conversación no cambia.",
            "message_edit_stopping",
            "message_edit_confirm_cancel",
            theme::component_palette(),
        )
        .width((width - 24.0).min(480.0))
        .confirm_label("Esperando…")
        .confirm_enabled(false)
        .body(
            Element::row(vec![
                Spinner::new("message_edit_stopping_spinner", theme::FOREGROUND).size(14.0).render().centered_y(),
                small("Deteniendo el turno…".to_owned()).centered_y(),
            ])
            .gap(8.0),
        )
        .render();
    }
    // One sentence and, at most, one caveat: the choice is the two buttons.
    let description = if confirm.stops_turn {
        "Se detiene el turno en curso y se quitan los mensajes posteriores."
    } else {
        "Se quitan los mensajes posteriores a este."
    };
    let files = |count: usize| match count {
        1 => "1 archivo".to_owned(),
        count => format!("{count} archivos"),
    };
    let caveat = if let Some(error) = &confirm.error {
        Some((error.clone(), theme::DESTRUCTIVE))
    } else if confirm.recomputed {
        Some(("La lista de archivos cambió al detener el turno.".to_owned(), theme::WARNING))
    } else if let Some(warning) = opaque_warning(confirm.opaque) {
        Some((warning, theme::WARNING))
    } else if !confirm.skipped.is_empty() && !confirm.stops_turn {
        let mut reasons: Vec<_> = confirm.skipped.iter().map(|(_, reason)| reason.label()).collect();
        reasons.sort();
        reasons.dedup();
        Some((
            format!("{} no se restauran: {}.", files(confirm.skipped.len()), reasons.join("; ")),
            theme::SETTINGS_FOREGROUND_SUBTLE,
        ))
    } else {
        None
    };
    // Without anything to restore there is one way forward: send.
    let can_revert = confirm.stops_turn || !confirm.restorable.is_empty();
    let palette = theme::component_palette();
    let outline = |id: &str, label: &str| {
        Button::new(id, label, palette).variant(ButtonVariant::Outline).size(ControlSize::Regular).render()
    };
    let footer = if can_revert {
        let hint = if confirm.stops_turn {
            "Restaura los archivos que cambió el agente desde este mensaje; se calcula al detener el turno".to_owned()
        } else {
            format!("Restaura {} a como estaban antes de este mensaje", files(confirm.restorable.len()))
        };
        vec![
            outline("message_edit_keep_files", "No revertir"),
            theme::primary("message_edit_revert", "Revertir").size(ControlSize::Regular).tooltip(hint).render(),
        ]
    } else {
        vec![
            outline("message_edit_confirm_cancel", "Cancelar"),
            theme::primary("message_edit_keep_files", "Enviar").size(ControlSize::Regular).render(),
        ]
    };
    let dialog_width = openide_ui::Modal::fit_width(width, 440.0).max(240.0);
    let mut dialog = openide_ui::Modal::new(
        base,
        true,
        "¿Enviar desde un mensaje anterior?",
        description,
        "message_edit_confirm_cancel",
        palette,
    )
    .width(dialog_width)
    .footer(
        openide_ui::ButtonGroup::new(footer)
            .align_end()
            .sizing(crate::ui_sizing::MODAL_FOOTER)
            .render(),
    );
    if let Some((text, ink)) = caveat {
        dialog = dialog.body(
            Element::text(text).font_size(12.0).font_family(FontFamily::Sans).foreground(ink),
        );
    }
    dialog.render()
}

/// "1 archivo" or "restauran 3 archivos": `one` whole, or `many count noun`.
fn counted(count: usize, one: &str, many: &str, noun: &str) -> String {
    match count {
        1 => one.to_owned(),
        _ => format!("{many} {count} {noun}").trim_end().to_owned(),
    }
}

/// Up to `limit` names, then how many more.
fn listed<'a>(names: impl Iterator<Item = &'a str>, limit: usize) -> String {
    let names: Vec<_> = names.collect();
    let mut text = names[..names.len().min(limit)].join(", ");
    if names.len() > limit {
        text.push_str(&format!(" y {} más", names.len() - limit));
    }
    text
}

/// Effects of the removed turns that no choice in the dialog takes back.
pub(crate) fn opaque_warning(opaque: OpaqueEffects) -> Option<String> {
    let parts: Vec<_> = [
        (opaque.commands, "1 comando", "comandos"),
        (opaque.mcp, "1 herramienta MCP", "herramientas MCP"),
        (opaque.other, "1 herramienta más", "herramientas más"),
    ]
    .into_iter()
    .filter(|(count, ..)| *count > 0)
    .map(|(count, one, many)| if count == 1 { one.to_owned() } else { format!("{count} {many}") })
    .collect();
    let (last, rest) = parts.split_last()?;
    let list = if rest.is_empty() { last.clone() } else { format!("{} y {last}", rest.join(", ")) };
    Some(format!("Estos turnos también ejecutaron {list}; sus efectos no se pueden deshacer."))
}

#[derive(Default, Serialize, Deserialize)]
struct TypedFile {
    version: u8,
    /// Oldest first: (sha256 of the composed prompt, typed text).
    entries: Vec<(String, String)>,
}

fn typed_path(data_root: &Path, session_id: &str) -> io::Result<PathBuf> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid session id"));
    }
    Ok(data_root.join(TYPED_DIRECTORY).join(format!("{session_id}.json")))
}

fn prompt_digest(composed: &str) -> String {
    format!("{:x}", Sha256::digest(composed.as_bytes()))
}

fn read_typed(path: &Path) -> io::Result<TypedFile> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_TYPED_BYTES => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "typed prompts too large"))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(TypedFile::default()),
        Err(error) => return Err(error),
    }
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

/// Typed text by digest of the prompt it was composed into. Keyed by
/// content, so a rewind cannot leave an entry pointing at the wrong turn.
pub(crate) fn load_typed(data_root: &Path, session_id: &str) -> HashMap<String, String> {
    typed_path(data_root, session_id)
        .and_then(|path| read_typed(&path))
        .map(|file| file.entries.into_iter().collect())
        .unwrap_or_default()
}

pub(crate) fn recorded_typed<'a>(typed: &'a HashMap<String, String>, composed: &str) -> Option<&'a str> {
    typed.get(&prompt_digest(composed)).map(String::as_str)
}

/// Remember what the user typed for a prompt that had blocks appended.
pub(crate) fn record_typed(
    data_root: &Path,
    session_id: &str,
    composed: &str,
    typed: &str,
) -> io::Result<()> {
    let path = typed_path(data_root, session_id)?;
    let directory = path.parent().expect("typed prompts live in a directory");
    fs::create_dir_all(directory)?;
    let mut file = read_typed(&path).unwrap_or_default();
    let digest = prompt_digest(composed);
    file.version = 1;
    file.entries.retain(|(saved, _)| *saved != digest);
    file.entries.push((digest, typed.to_owned()));
    let excess = file.entries.len().saturating_sub(MAX_TYPED_ENTRIES);
    file.entries.drain(..excess);
    let temporary = directory.join(format!(".{session_id}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut out = fs::File::create(&temporary)?;
        out.write_all(&serde_json::to_vec(&file).map_err(io::Error::other)?)?;
        out.sync_all()?;
        fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn remove_typed(data_root: &Path, session_id: &str) {
    if let Ok(path) = typed_path(data_root, session_id) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_is_recovered_only_when_it_is_certain() {
        assert_eq!(typed_text("Revisá esto", None).as_deref(), Some("Revisá esto"));
        for block in APPENDED_BLOCKS {
            let composed = format!("Revisá esto{block}…");
            assert_eq!(typed_text(&composed, None), None, "{block:?} may have been appended");
            assert_eq!(typed_text(&composed, Some("Revisá esto")).as_deref(), Some("Revisá esto"));
        }
        assert!(carries_attachments("a\n\nAdjuntos de este proyecto (rutas relativas): []", 0));
        assert!(carries_attachments("a", 1));
        assert!(!carries_attachments("a\n\n[Contexto agregado por un hook]\nb", 0));
    }

    #[test]
    fn typed_prompts_round_trip_are_bounded_and_leave_with_their_session() {
        let root = tempfile::tempdir().unwrap();
        assert!(load_typed(root.path(), "session").is_empty());
        record_typed(root.path(), "session", "composed one", "one").unwrap();
        record_typed(root.path(), "session", "composed two", "two").unwrap();
        record_typed(root.path(), "session", "composed one", "one again").unwrap();
        let typed = load_typed(root.path(), "session");
        assert_eq!(recorded_typed(&typed, "composed one"), Some("one again"));
        assert_eq!(recorded_typed(&typed, "composed two"), Some("two"));
        assert_eq!(recorded_typed(&typed, "other"), None);
        assert!(load_typed(root.path(), "another").is_empty());
        for index in 0..MAX_TYPED_ENTRIES + 3 {
            record_typed(root.path(), "session", &format!("composed {index}"), "x").unwrap();
        }
        assert_eq!(load_typed(root.path(), "session").len(), MAX_TYPED_ENTRIES);
        assert!(record_typed(root.path(), "../escape", "a", "b").is_err());
        remove_typed(root.path(), "session");
        assert!(load_typed(root.path(), "session").is_empty());
    }
}
