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
    PopoverPlacement, RadiusRole, Text, TextRole,
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

/// The turn a click on this row may edit. Nothing is offered while the
/// session works, waits for a permission, or another edit is open.
pub(crate) fn offered(state: &ViewState, turn: &EditableTurn) -> bool {
    turn.text.is_some()
        && state.selected_backend == "native"
        && !state.running
        && !state.background_run_selected
        && state.pending_permissions.is_empty()
        && state.message_edit.editing.is_none()
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
    let mut rows = vec![build_composer(&ComposerProps::from_state(
        state,
        width,
        false,
        None,
        "Editá el mensaje…",
        None,
    ))];
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
    let removed = match confirm.later_messages {
        0 => "Se quitará de la conversación la respuesta a este mensaje.".to_owned(),
        1 => "Se quitarán de la conversación la respuesta a este mensaje y el mensaje posterior con su respuesta.".to_owned(),
        count => format!("Se quitarán de la conversación la respuesta a este mensaje y los {count} mensajes posteriores con sus respuestas."),
    };
    let small = |text: String| Text::new(text, theme::component_palette()).role(TextRole::Small).render();
    let mut body = Vec::new();
    if !confirm.restorable.is_empty() {
        body.push(small(format!(
            "Se {}:",
            counted(confirm.restorable.len(), "restaura 1 archivo", "restauran", "archivos")
        )));
        body.push(path_list(confirm.restorable.iter().map(String::as_str)));
    } else {
        body.push(small(
            match (&confirm.unavailable, confirm.skipped.is_empty()) {
                (Some(reason), _) => format!("No se pudo comprobar qué archivos restaurar: {reason}"),
                (None, true) => "Estos turnos no dejaron cambios de archivos registrados: no hay nada que revertir.".to_owned(),
                (None, false) => "Ningún archivo se puede restaurar con seguridad.".to_owned(),
            },
        ));
    }
    if !confirm.skipped.is_empty() {
        body.push(small(format!("Se {}:", counted(confirm.skipped.len(), "omite 1", "omiten", ""))));
        let mut reasons: Vec<_> = confirm.skipped.iter().map(|(_, reason)| *reason).collect();
        reasons.sort();
        reasons.dedup();
        for reason in reasons {
            let paths = confirm.skipped.iter().filter(|(_, other)| *other == reason).map(|(path, _)| path.as_str());
            body.push(
                Text::new(format!("{}: {}", reason.label(), listed(paths, 3)), theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render(),
            );
        }
    }
    if let Some(warning) = opaque_warning(confirm.opaque) {
        body.push(Element::text(warning).font_size(12.0).font_family(FontFamily::Sans).foreground(theme::WARNING));
    }
    if let Some(error) = &confirm.error {
        body.push(
            Element::text(error)
                .font_size(12.0)
                .foreground(theme::DESTRUCTIVE),
        );
    }
    AlertDialog::new(
        base,
        true,
        "¿Enviar desde un mensaje anterior?",
        format!("{removed} «No revertir» deja los archivos como están. Podés deshacer el rebobinado después."),
        "message_edit_revert",
        "message_edit_confirm_cancel",
        theme::component_palette(),
    )
    .width((width - 24.0).min(480.0))
    .confirm_label("Revertir")
    .confirm_enabled(!confirm.restorable.is_empty())
    .alternative("message_edit_keep_files", "No revertir")
    .body(Element::column(body).gap(6.0))
    .render()
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

fn path_list<'a>(paths: impl Iterator<Item = &'a str>) -> Element {
    let paths: Vec<_> = paths.collect();
    let mut rows: Vec<_> = paths
        .iter()
        .take(6)
        .map(|path| Element::text(*path).font_size(12.0).font_family(FontFamily::Monospace))
        .collect();
    if paths.len() > 6 {
        rows.push(
            Text::new(format!("y {} más", paths.len() - 6), theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
    }
    Element::column(rows).gap(2.0)
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
