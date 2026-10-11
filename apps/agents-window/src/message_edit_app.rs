//! Host side of editing an earlier message: entering and leaving the edit,
//! the rewind of the journal with its sidecars, and its undo.

use super::*;
use crate::message_edit::{
    self, EditableTurn, EditingMessage, RewindConfirm, RewindUndoView,
};
use crate::session_changes::{RevertFile, RevertPreflight};
use openide_core::{NativeRewind, NativeRewindRefusal, StoreError};

/// What an undo needs beyond the view's notice.
pub(super) struct RewindUndo {
    rewind: NativeRewind,
    /// Tool captures of the removed calls, taken out of their store.
    snapshots: Vec<(String, Vec<u8>)>,
    /// Files "Revertir" wrote back, with the state an undo returns them to.
    files: Vec<RevertFile>,
    workspace: PathBuf,
}

pub(super) struct MessageEditHost {
    /// False for a subagent's session and for an automation's: their
    /// journals are never rewound, so no message is offered.
    pub(super) allowed: bool,
    /// Typed prompts of one session, by digest of the composed prompt.
    typed: Option<(String, HashMap<String, String>)>,
    undo: Option<RewindUndo>,
    /// A confirmed edit waiting for the running turn to stop.
    pending_stop: Option<PendingStop>,
}

/// The edit confirmed while a turn ran. Nothing is rewound, and no file is
/// touched, until that turn has ended and the journal is settled.
struct PendingStop {
    session_id: String,
    sequence: u64,
    revert: bool,
    /// The provisional lists the choice was made on.
    shown: (Vec<String>, Vec<(String, crate::session_changes::RevertSkip)>),
}

impl Default for MessageEditHost {
    fn default() -> Self {
        Self { allowed: true, typed: None, undo: None, pending_stop: None }
    }
}

fn refusal_text(error: &StoreError) -> String {
    match error {
        StoreError::NativeSessionBusy(_) => {
            "La conversación está en uso. Esperá a que termine el agente.".into()
        }
        StoreError::NativeRewindRefused(reason) => match reason {
            NativeRewindRefusal::NotATurnStart => "Este mensaje ya no es el inicio de un turno.",
            NativeRewindRefusal::UnfinishedTurn => {
                "El último turno no terminó. Resolvé su recuperación antes de rebobinar."
            }
            NativeRewindRefusal::PendingSteers => "Hay un mensaje en cola para el turno actual.",
            NativeRewindRefusal::PendingPermission => "Hay un permiso pendiente de respuesta.",
            NativeRewindRefusal::PendingCommand => "Hay una operación de comando pendiente.",
            NativeRewindRefusal::SubagentSession => "La conversación de un subagente no se rebobina.",
            NativeRewindRefusal::AutomationSession => {
                "La conversación de una automatización no se rebobina."
            }
            NativeRewindRefusal::SpawnedSubagents => {
                "Los turnos que se quitarían usaron subagentes; sus conversaciones no se pueden quitar con ellos."
            }
            NativeRewindRefusal::JournalChanged => {
                "La conversación cambió después del rebobinado; ya no se puede deshacer."
            }
            NativeRewindRefusal::TailUnavailable => {
                "Los mensajes quitados ya no están disponibles."
            }
        }
        .into(),
        other => format!("No se pudo rebobinar la conversación: {other}"),
    }
}

impl AgentsWindow {
    /// Publish which user messages can be edited. Cheap unless a turn was
    /// added: the projection only ever appends turns between reloads.
    pub(super) fn sync_message_edit_turns(&mut self) {
        let turns = self.projection.user_turns();
        if !self.message_edit.allowed || self.view.selected_backend != "native" {
            self.view.message_edit.turns.clear();
            return;
        }
        if turns.len() == self.view.message_edit.turns.len() {
            return;
        }
        let Some(session_id) = self.view.selected_session.clone() else {
            self.view.message_edit.turns.clear();
            return;
        };
        if self.message_edit.typed.as_ref().map(|(id, _)| id) != Some(&session_id) {
            self.message_edit.typed = Some((
                session_id.clone(),
                message_edit::load_typed(&self.data_root, &session_id),
            ));
        }
        let typed = &self.message_edit.typed.as_ref().expect("typed prompts were loaded").1;
        let users: Vec<_> = self
            .view
            .transcript
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::User { content, images } => Some((content, images.len())),
                _ => None,
            })
            .collect();
        self.view.message_edit.turns = turns
            .iter()
            .filter_map(|turn| {
                let (content, images) = users.get(turn.user_row)?;
                Some(EditableTurn {
                    user_row: turn.user_row,
                    sequence: turn.sequence,
                    text: message_edit::typed_text(
                        content,
                        message_edit::recorded_typed(typed, content),
                    ),
                    drops_attachments: message_edit::carries_attachments(content, *images),
                })
            })
            .collect();
    }

    /// `start_run` composed `prompt` from `typed` by appending blocks.
    pub(super) fn record_typed_prompt(&mut self, session_id: &str, prompt: &str, typed: &str) {
        // Losing this only makes the message non-editable later.
        let _ = message_edit::record_typed(&self.data_root, session_id, prompt, typed);
        self.message_edit.typed = None;
    }

    /// Why the selected session cannot be rewound right now.
    fn message_edit_blocked(&self) -> Option<&'static str> {
        if !self.view.pending_permissions.is_empty() {
            return Some("Respondé el permiso pendiente antes de rebobinar.");
        }
        self.branch_change_blocked_reason()
            .map(|_| "Esperá a que termine el agente para editar un mensaje anterior.")
    }

    /// Why this message cannot be opened for editing. The selected session's
    /// own running turn does not prevent it: sending the edit stops that
    /// turn first. Anything else at work does.
    fn message_edit_open_blocked(&self) -> Option<&'static str> {
        if !self.view.pending_permissions.is_empty() {
            return Some("Respondé el permiso pendiente antes de editar un mensaje anterior.");
        }
        self.agent_busy_reason(false)
            .map(|_| "Esperá a que termine el agente para editar un mensaje anterior.")
    }

    /// Whether `start_run` would stop before sending, for reasons it can
    /// only report after the rewind has already happened.
    fn message_edit_send_blocked(&self) -> Option<&'static str> {
        if self.plugin_removal_pending {
            return Some("Esperá a que termine la desinstalación del plugin.");
        }
        if self.view.composer_branches.switching() {
            return Some("Esperá a que termine el cambio de rama.");
        }
        if self.attachment_pastes_pending.contains(&self.draft_key()) {
            return Some("Esperá a que se preparen los adjuntos.");
        }
        if self.view.composer_pending.is_some() {
            return Some("Revisá la selección pendiente del compositor antes de enviar.");
        }
        if self.persistence_error.is_some() {
            return Some("Hay un error de guardado sin resolver.");
        }
        if !Path::new(self.view.workspace.trim()).is_dir() {
            return Some("El directorio de trabajo no existe.");
        }
        if !self.native_settings().has_explicit_configuration(&self.data_root) {
            return Some("No hay modelos disponibles. Conectá un proveedor en Configuración.");
        }
        None
    }

    /// Route the clicks of this feature. True when `id` was one of them.
    pub(super) fn message_edit_action(&mut self, id: &str) -> bool {
        if let Some(row) = id.strip_prefix("message_edit:") {
            if let Ok(row) = row.parse() {
                self.begin_message_edit(row);
            }
            return true;
        }
        match id {
            "message_edit_cancel" => self.cancel_message_edit(),
            // Also cancels an edit waiting for its turn to stop: the turn
            // stays stopped and nothing else changes.
            "message_edit_confirm_cancel" => {
                self.view.message_edit.confirm = None;
                self.message_edit.pending_stop = None;
            }
            "message_edit_keep_files" => self.confirm_message_edit(false),
            "message_edit_revert" => self.confirm_message_edit(true),
            "message_rewind_undo" => self.undo_message_rewind(),
            "message_rewind_undo_dismiss" => {
                self.message_edit.undo = None;
                self.view.message_edit.undo = None;
            }
            _ => return false,
        }
        true
    }

    fn begin_message_edit(&mut self, user_row: usize) {
        let Some(turn) = self
            .view
            .message_edit
            .turns
            .iter()
            .find(|turn| turn.user_row == user_row)
            .filter(|turn| message_edit::offered(&self.view, turn))
            .cloned()
        else {
            return;
        };
        if let Some(reason) = self.message_edit_open_blocked() {
            self.view.status = reason.to_owned();
            return;
        }
        let Some(text) = turn.text else { return };
        // The bottom draft goes to the draft map now and stays there:
        // `save_draft` is skipped while the edit is open.
        self.save_draft();
        self.close_composer_floating_controls();
        self.stop_dictation();
        self.view.message_edit.confirm = None;
        self.view.message_edit.editing = Some(EditingMessage {
            user_row,
            sequence: turn.sequence,
            drops_attachments: turn.drops_attachments,
        });
        self.view.composer_error = None;
        self.view.composer_attachments.clear();
        self.view.prompt = text.clone();
        self.tree.set_input_value("prompt", text);
    }

    /// Leave the edit and bring the bottom draft back exactly.
    pub(super) fn cancel_message_edit(&mut self) {
        if self.view.message_edit.editing.take().is_none() {
            return;
        }
        self.view.message_edit.confirm = None;
        self.message_edit.pending_stop = None;
        self.close_composer_floating_controls();
        self.view.composer_error = None;
        self.restore_draft();
    }

    /// Sending from the edit asks first: it removes history.
    pub(super) fn request_message_edit_submit(&mut self) {
        let Some(editing) = self.view.message_edit.editing.as_ref() else {
            return;
        };
        if self.view.prompt.trim().is_empty() && self.view.composer_attachments.is_empty() {
            return;
        }
        let mut confirm = self.rewind_confirm(editing.sequence);
        // With a turn running the lists are provisional: that turn may still
        // change files before it stops.
        confirm.stops_turn = self.view.running;
        self.view.message_edit.confirm = Some(confirm);
    }

    /// The confirmation for a rewind to `sequence`, with the preflight as
    /// the disk and the journal are right now.
    fn rewind_confirm(&self, sequence: u64) -> RewindConfirm {
        let later_messages = self
            .view
            .message_edit
            .turns
            .iter()
            .filter(|turn| turn.sequence > sequence)
            .count();
        let mut confirm = RewindConfirm { later_messages, ..Default::default() };
        match self.revert_preflight(sequence) {
            Ok(preflight) => show_preflight(&mut confirm, &preflight),
            Err(error) => confirm.unavailable = Some(error),
        }
        confirm
    }

    /// Confirmed while a turn ran: ask that turn to stop and wait for it in
    /// `poll_message_edit_stop`. Nothing is rewound or written here.
    fn stop_then_confirm_message_edit(&mut self, revert: bool) {
        let (Some(editing), Some(session_id)) = (
            self.view.message_edit.editing.clone(),
            self.view.selected_session.clone(),
        ) else {
            return;
        };
        // Refuse before stopping anything when the edit could not be sent.
        let blocked = self
            .message_edit_open_blocked()
            .or_else(|| self.message_edit_send_blocked());
        let Some(confirm) = self.view.message_edit.confirm.as_mut() else {
            return;
        };
        if let Some(reason) = blocked {
            confirm.error = Some(reason.to_owned());
            return;
        }
        confirm.error = None;
        confirm.stopping = true;
        self.message_edit.pending_stop = Some(PendingStop {
            session_id,
            sequence: editing.sequence,
            revert,
            shown: (confirm.restorable.clone(), confirm.skipped.clone()),
        });
        self.cancel_selected_run();
        // The turn may have ended while the dialog was open.
        self.poll_message_edit_stop();
    }

    /// Carry on with an edit confirmed while a turn ran, once that turn has
    /// ended. Called every frame; it waits on the run's own events and has
    /// no timeout. True when it changed the view.
    pub(super) fn poll_message_edit_stop(&mut self) -> bool {
        let Some(pending) = self.message_edit.pending_stop.as_ref() else {
            return false;
        };
        let current = self.view.selected_session.as_deref() == Some(pending.session_id.as_str())
            && self.view.message_edit.editing.as_ref().is_some_and(|editing| editing.sequence == pending.sequence)
            && self.view.message_edit.confirm.as_ref().is_some_and(|confirm| confirm.stopping);
        if !current {
            self.message_edit.pending_stop = None;
            return true;
        }
        if self.view.running || self.active_run.is_some() {
            return false;
        }
        let pending = self.message_edit.pending_stop.take().expect("checked above");
        // The run ended. The journal must be settled too: a turn left
        // unfinished needs its recovery, which is the user's to resolve.
        let unsettled = match self.store.as_ref().map(|store| store.active_native_turn_id(&pending.session_id)) {
            Some(Ok(None)) => self
                .message_edit_blocked()
                .or_else(|| self.message_edit_send_blocked())
                .map(str::to_owned),
            Some(Ok(Some(_))) => Some("El turno no terminó de detenerse y necesita recuperación.".to_owned()),
            Some(Err(error)) => Some(format!("No se pudo comprobar que el turno se detuvo: {error}.")),
            None => Some("El historial no está disponible.".to_owned()),
        };
        if let Some(reason) = unsettled {
            // Abandoned: the typed text stays in the edit composer.
            self.view.message_edit.confirm = None;
            self.show_composer_error(format!(
                "{reason} El mensaje editado no se envió y la conversación no cambió."
            ));
            return true;
        }
        // What the stopped turn changed is part of what a revert restores:
        // the lists are made again, and a revert chosen on other lists is
        // asked again before any file is touched.
        let mut confirm = self.rewind_confirm(pending.sequence);
        let changed = (&confirm.restorable, &confirm.skipped) != (&pending.shown.0, &pending.shown.1);
        confirm.recomputed = changed;
        let ask_again = pending.revert && (changed || confirm.restorable.is_empty());
        self.view.message_edit.confirm = Some(confirm);
        if !ask_again {
            self.confirm_message_edit(pending.revert);
        }
        true
    }

    /// What "Revertir" would do with the files right now. Reads only.
    fn revert_preflight(&self, before_sequence: u64) -> Result<RevertPreflight, String> {
        let store = self.store.as_ref().ok_or("El historial no está disponible")?;
        let session_id = self.view.selected_session.as_deref().ok_or("No hay conversación")?;
        let workspace = Path::new(self.view.workspace.trim());
        crate::session_changes::revert_preflight(
            store.storage_root(),
            session_id,
            workspace,
            before_sequence,
            &self.code_editor.unsaved_paths(workspace),
        )
        .map_err(|error| error.to_string())
    }

    /// Restore the files the removed turns changed, then rewind. The
    /// preflight is made again here: the disk may have changed while the
    /// dialog was open, and only what is restorable now is written. Either
    /// both happen or the files are put back and the journal is untouched.
    fn revert_and_rewind(
        &mut self,
        session_id: &str,
        before_sequence: u64,
    ) -> Result<(RewindUndo, Vec<String>), String> {
        let workspace = PathBuf::from(self.view.workspace.trim());
        // A running command may be writing the same files.
        if self.native_command_sessions.has_running_in_worktree(&workspace)? {
            return Err("Esperá a que terminen los comandos del proyecto antes de revertir.".into());
        }
        let storage = self.store.as_ref().ok_or("El historial no está disponible")?.storage_root().to_owned();
        let preflight = self.revert_preflight(before_sequence)?;
        if let Some(confirm) = &mut self.view.message_edit.confirm {
            show_preflight(confirm, &preflight);
        }
        if preflight.restorable.is_empty() {
            return Err("Ya no hay archivos que se puedan restaurar.".into());
        }
        let put_back = |left: Vec<String>| match left.is_empty() {
            true => "No se cambió ningún archivo ni la conversación.".to_owned(),
            false => format!("No se pudieron devolver a su estado: {}.", left.join(", ")),
        };
        crate::session_changes::revert_files(&storage, &workspace, &preflight.restorable)
            .map_err(|(error, left)| format!("No se pudo restaurar {error}. {}", put_back(left)))?;
        let mut undo = self.rewind_selected_native(session_id, before_sequence).map_err(|error| {
            let left = crate::session_changes::reapply_reverted_files(&storage, &workspace, &preflight.restorable);
            format!("{error} {}", put_back(left))
        })?;
        undo.files = preflight.restorable;
        undo.workspace = workspace;
        Ok((undo, preflight.skipped.into_iter().map(|(path, _)| path).collect()))
    }

    /// Open documents and the file tree read the restored paths again.
    fn refresh_reverted_files(&mut self, workspace: &Path, files: &[RevertFile]) {
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        self.refresh_clean_code_editor_paths(workspace, &paths);
        self.invalidate_workspace_files();
    }

    /// Markers move before the journal does, in their conservative direction,
    /// so a failure here or in the rewind leaves a valid session.
    fn rewind_selected_native(
        &mut self,
        session_id: &str,
        before_sequence: u64,
    ) -> Result<RewindUndo, String> {
        let store = self.store.clone().ok_or("El historial no está disponible")?;
        let storage = store.storage_root().to_owned();
        crate::session_changes_editor::rewind_marker(&storage, session_id, before_sequence)
            .and_then(|()| {
                crate::session_changes_external::rewind_marker(&storage, session_id, before_sequence)
            })
            .map_err(|error| format!("No se pudo ajustar el registro de cambios: {error}"))?;
        let rewind = store
            .rewind_native(session_id, before_sequence)
            .map_err(|error| refusal_text(&error))?;
        let calls: Vec<&str> = rewind
            .removed
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::ToolStarted { tool_call } => Some(tool_call.id.as_str()),
                _ => None,
            })
            .collect();
        // A capture left behind only costs a diff if its call id is reused.
        let snapshots = crate::session_changes::take_tool_snapshots(&storage, session_id, &calls)
            .unwrap_or_default();
        Ok(RewindUndo { rewind, snapshots, files: Vec::new(), workspace: PathBuf::new() })
    }

    /// Reload through the path every session takes; nothing is patched here.
    fn reload_rewound_session(&mut self, session_id: &str) {
        self.chat_diffs = Default::default();
        self.open_session(session_id);
    }

    fn confirm_message_edit(&mut self, revert: bool) {
        match &self.view.message_edit.confirm {
            Some(confirm) if confirm.stopping => return,
            Some(confirm) if confirm.stops_turn => return self.stop_then_confirm_message_edit(revert),
            _ => {}
        }
        let (Some(editing), Some(session_id)) = (
            self.view.message_edit.editing.clone(),
            self.view.selected_session.clone(),
        ) else {
            return;
        };
        let blocked = self
            .message_edit_blocked()
            .or_else(|| self.message_edit_send_blocked());
        let undo = match blocked {
            Some(reason) => Err(reason.to_owned()),
            None if revert => self.revert_and_rewind(&session_id, editing.sequence),
            None => self
                .rewind_selected_native(&session_id, editing.sequence)
                .map(|undo| (undo, Vec::new())),
        };
        let (undo, skipped_files) = match undo {
            Ok(undo) => undo,
            Err(error) => {
                if let Some(confirm) = &mut self.view.message_edit.confirm {
                    confirm.error = Some(error);
                }
                return;
            }
        };
        let text = self.view.prompt.clone();
        let attachments = self.view.composer_attachments.clone();
        let removed_messages = undo
            .rewind
            .removed
            .iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    AgentEventKind::TurnAdmitted { .. } | AgentEventKind::TurnAdmittedWithImages { .. }
                )
            })
            .count();
        // Still editing here, so the reload does not save the edited text as
        // the session's draft; it ends the edit and restores the bottom draft.
        self.reload_rewound_session(&session_id);
        self.refresh_reverted_files(&undo.workspace.clone(), &undo.files);
        let draft = (self.view.prompt.clone(), self.view.composer_attachments.clone());
        self.view.message_edit.undo = Some(RewindUndoView {
            session_id,
            removed_messages,
            error: None,
            restored_files: undo.files.len(),
            skipped_files,
            undone: false,
        });
        self.message_edit.undo = Some(undo);
        self.view.prompt = text.clone();
        self.view.composer_attachments = attachments;
        self.tree.set_input_value("prompt", text);
        self.start_run();
        if self.view.prompt.is_empty() {
            // Sent. The bottom draft returns to the composer.
            self.view.prompt = draft.0.clone();
            self.view.composer_attachments = draft.1;
            self.tree.set_input_value("prompt", draft.0);
            self.draft_dirty_at = Some(Instant::now());
            self.save_draft();
        } else if !draft.0.is_empty() {
            // Not sent: the edited text stays to be sent again, and the
            // draft it displaced follows it rather than being lost.
            self.view.prompt = format!("{}\n\n{}", self.view.prompt, draft.0);
            self.tree.set_input_value("prompt", self.view.prompt.clone());
            self.draft_dirty_at = Some(Instant::now());
        }
    }

    /// Put the removed messages back. What was sent after the rewind is
    /// itself rewound first, to its own saved tail, so nothing is discarded
    /// and the journal ends exactly where the first rewind left it.
    ///
    /// Files a revert restored return to what the removed turns had left,
    /// each only while the disk still holds what the revert wrote. File
    /// changes of the replacement turn stay on disk: a path it touched no
    /// longer holds that content, so it is left alone and listed.
    fn undo_message_rewind(&mut self) {
        let Some(undo) = self.message_edit.undo.as_ref() else {
            self.view.message_edit.undo = None;
            return;
        };
        let session_id = undo.rewind.session_id.clone();
        let before_sequence = undo.rewind.before_sequence;
        if self.view.selected_session.as_deref() != Some(session_id.as_str()) {
            return;
        }
        let result = (|| -> Result<Vec<String>, String> {
            if let Some(reason) = self.message_edit_blocked() {
                return Err(reason.to_owned());
            }
            let undo = self.message_edit.undo.as_ref().expect("the undo was checked above");
            if !undo.files.is_empty()
                && self.native_command_sessions.has_running_in_worktree(&undo.workspace)?
            {
                return Err("Esperá a que terminen los comandos del proyecto antes de deshacer.".into());
            }
            let store = self.store.clone().ok_or("El historial no está disponible")?;
            let stored = store
                .load(&session_id)
                .map_err(|error| format!("No se pudo leer la conversación: {error}"))?
                .ok_or("La conversación ya no existe")?;
            if stored.events.iter().any(|event| event.sequence >= before_sequence) {
                self.rewind_selected_native(&session_id, before_sequence)?;
            }
            let undo = self.message_edit.undo.as_ref().expect("the undo was checked above");
            store
                .undo_native_rewind(&undo.rewind)
                .map_err(|error| refusal_text(&error))?;
            // Their diffs read from these; a failure only loses that detail.
            let _ = crate::session_changes::restore_tool_snapshots(
                store.storage_root(),
                &undo.snapshots,
            );
            Ok(crate::session_changes::reapply_reverted_files(
                store.storage_root(),
                &undo.workspace,
                &undo.files,
            ))
        })();
        match result {
            Ok(left) => {
                let undo = self.message_edit.undo.take().expect("the undo was checked above");
                // Paths left alone stay on a notice that only reports them.
                self.view.message_edit.undo = self
                    .view
                    .message_edit
                    .undo
                    .take()
                    .filter(|_| !left.is_empty())
                    .map(|notice| RewindUndoView { skipped_files: left, undone: true, error: None, ..notice });
                self.cancel_message_edit();
                self.reload_rewound_session(&session_id);
                self.refresh_reverted_files(&undo.workspace, &undo.files);
                self.view.status = "Rebobinado deshecho".to_owned();
            }
            Err(error) => {
                // A partial undo may have rewound the newer turn already.
                self.reload_rewound_session(&session_id);
                if let Some(notice) = &mut self.view.message_edit.undo {
                    notice.error = Some(error);
                }
            }
        }
    }
}

fn show_preflight(confirm: &mut RewindConfirm, preflight: &RevertPreflight) {
    confirm.restorable = preflight.restorable.iter().map(|file| file.path.clone()).collect();
    confirm.skipped = preflight.skipped.clone();
    confirm.opaque = preflight.opaque;
    confirm.unavailable = None;
}
