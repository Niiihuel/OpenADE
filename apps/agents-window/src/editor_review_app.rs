//! Review an agent's exact file edit in the ordinary editor. File I/O and
//! durable acceptance run off-thread; a stale tab never consumes their result.
use super::*;
use crate::{
    editor_review_ui::EditorReviewView,
    session_changes::{self, FileReview, FileReviewDecision, FileReviewDecisionTarget},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    workspace: PathBuf,
    session: String,
    path: String,
    tab: u64,
    /// Missing/deleted documents retain the exact history row they opened.
    call: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    scope: Scope,
    sequence: u64,
    document: Option<(u64, u64)>,
    dirty: bool,
    running: bool,
}

struct Response {
    key: Key,
    decision: Option<FileReviewDecision>,
    disk_changed: bool,
    warning: Option<String>,
    result: Result<Option<FileReview>, String>,
}

#[derive(Default)]
pub(super) struct EditorReviewHost {
    key: Option<Key>,
    loaded: Option<FileReview>,
    pending: Option<Receiver<Response>>,
    accepting: bool,
    token: String,
    current_hunk: usize,
    menu_open: bool,
    projection: Option<Arc<[openide_ui::CodeReviewHunk]>>,
    matches: Option<((u64, u64, String), bool)>,
}

impl AgentsWindow {
    fn editor_review_key(&self) -> Option<Key> {
        let session = self.view.selected_session.clone()?;
        let (tab, path, call, document) = if let Some(view) = &self.view.code_editor {
            let snapshot = view.controller.as_ref()?.snapshot().ok()?;
            (
                view.tab_id,
                view.path.clone(),
                None,
                Some((snapshot.document_id(), snapshot.revision())),
            )
        } else if let Some(view) = &self.view.code_snapshot {
            let (owner, call, _) =
                serde_json::from_str::<(String, String, usize)>(&view.source).ok()?;
            if owner != session {
                return None;
            }
            if !self.view.transcript.iter().any(|item| matches!(item,
                TranscriptItem::Tool { id, name, activity: crate::transcript::ActivityStatus::Completed,
                    is_error: false, output: Some(_), .. }
                    if id == &call && matches!(name.as_str(), "write_file" | "apply_patch"))) {
                return None;
            }
            (view.tab_id, view.path.clone(), Some(call), None)
        } else {
            return None;
        };
        Some(Key {
            scope: Scope {
                workspace: PathBuf::from(&self.view.workspace),
                session,
                path,
                tab,
                call,
            },
            sequence: self.last_sequence,
            document,
            dirty: self
                .view
                .code_editor
                .as_ref()
                .and_then(|v| v.controller.as_ref())
                .and_then(|c| c.snapshot().ok())
                .is_some_and(|s| s.is_dirty()),
            running: self.view.running
                || self.view.background_run_selected
                || self.active_run.is_some()
                || !self.background_automation_runs.is_empty()
                || !self.background_resumes.is_empty()
                || !self.subagent_dispatches.is_empty(),
        })
    }

    fn clear_editor_review_surface(&mut self) -> bool {
        self.editor_review.menu_open = false;
        let mut changed = self.view.editor_workbench.review.take().is_some();
        if let Some(view) = &mut self.view.code_snapshot {
            changed |= view.review.take().is_some();
            if let Some(cached) = self.code_snapshots.get_mut(&view.tab_id) {
                cached.review = None;
            }
        }
        changed
    }

    /// Session changes are computed by the journal worker. Never derive review
    /// navigation from the Git worktree: it can contain unrelated human edits.
    fn editor_review_files<'a>(&'a self, key: &Key) -> Vec<&'a session_changes::SessionFileChange> {
        let state = &self.view.session_changes;
        if state.loading
            || state.workspace.as_ref() != Some(&key.scope.workspace)
            || state.session_id.as_deref() != Some(key.scope.session.as_str())
        {
            return Vec::new();
        }
        let Some(Ok(changes)) = &state.result else {
            return Vec::new();
        };
        if changes.workspace != key.scope.workspace || changes.session_id != key.scope.session {
            return Vec::new();
        }
        changes
            .files
            .iter()
            .filter(|file| file.unavailable.is_none() && file.last_tool_sequence > 0)
            .collect()
    }

    fn editor_review_file_position(&self, key: &Key) -> (usize, usize) {
        let files = self.editor_review_files(key);
        files
            .iter()
            .position(|file| file.file.path == key.scope.path)
            .map_or((1, 0), |index| (files.len(), index))
    }

    /// Missing files reuse the same immutable, owner-scoped capture as their
    /// confirmed chat activity. Preview inputs and failed calls are never links.
    fn editor_review_deleted_file_source(&self, key: &Key, path: &str) -> Option<String> {
        for item in self.view.transcript.iter().rev() {
            let TranscriptItem::Tool {
                id,
                name,
                activity: crate::transcript::ActivityStatus::Completed,
                is_error: false,
                output: Some(_),
                ..
            } = item
            else {
                continue;
            };
            if !matches!(name.as_str(), "write_file" | "apply_patch") {
                continue;
            }
            let Some(preview) = crate::file_activity_preview::preview(item, None) else {
                continue;
            };
            if let Some(index) = preview.files.iter().position(|file| {
                let candidate = Path::new(&file.path);
                let relative = if candidate.is_absolute() {
                    candidate.strip_prefix(&key.scope.workspace).ok()
                } else {
                    Some(candidate)
                };
                relative.is_some_and(|relative| relative == Path::new(path))
            }) {
                return Some(crate::file_activity_ui::open_action(
                    &key.scope.session,
                    id,
                    index,
                ));
            }
        }
        None
    }

    fn publish_editor_review(&mut self, key: &Key) -> bool {
        let (file_count, current_file) = self.editor_review_file_position(key);
        let Some(review) = self.editor_review.loaded.as_ref() else {
            return self.clear_editor_review_surface();
        };
        if review.resolved {
            return self.clear_editor_review_surface();
        }
        let mut reason = review.acceptance_unavailable.clone();
        let mut can_accept = review.can_accept && self.editor_review.pending.is_none();
        if key.running {
            can_accept = false;
            reason = Some("Espera a que finalice la ejecución para revisar esta versión.".into());
        }
        let hunks = self.editor_review.projection.clone().unwrap_or_default();
        let (document_id, revision) = key.document.unwrap_or_default();
        let mut display_hunks = hunks.clone();
        if let Some(view) = self.view.code_editor.as_ref() {
            let snapshot = view.controller.as_ref().and_then(|c| c.snapshot().ok());
            let matches = snapshot.as_ref().is_some_and(|snapshot| {
                let identity = (
                    snapshot.document_id(),
                    snapshot.revision(),
                    review.decision_version.clone(),
                );
                if let Some((previous, matches)) = &self.editor_review.matches {
                    if previous == &identity {
                        return *matches;
                    }
                }
                let matches = review.current.as_ref().is_some_and(|after| {
                    snapshot.byte_count() == after.len()
                        && snapshot.chunks().flat_map(str::bytes).eq(after.bytes())
                });
                self.editor_review.matches = Some((identity, matches));
                matches
            });
            if snapshot.as_ref().is_none_or(|s| s.is_dirty())
                || view.saving
                || view.conflict
                || view.loading
            {
                can_accept = false;
                reason = Some(
                    "Guarda o descarta tus cambios antes de revisar la edición del agente.".into(),
                );
            }
            if !matches {
                display_hunks = Arc::from([]);
                can_accept = false;
                reason = Some(
                    "El contenido del editor ya no coincide con esta edición del agente.".into(),
                );
            }
        }
        let can_reject = can_accept && review.can_reject;
        let reject_reason = if can_accept {
            review.rejection_unavailable.clone()
        } else {
            reason.clone()
        };
        let mut state = EditorReviewView {
            token: self.editor_review.token.clone(),
            document_id,
            revision,
            additions: review.file.additions,
            deletions: review.file.deletions,
            hunk_count: if key.document.is_some() {
                display_hunks.len()
            } else {
                0
            },
            current_hunk: self.editor_review.current_hunk,
            file_count,
            current_file,
            hunks: display_hunks,
            actions: Arc::from([]),
            busy: self.editor_review.pending.is_some(),
            menu_open: self.editor_review.menu_open,
            can_accept,
            reason,
            can_reject,
            reject_reason,
        };
        // Compare the small presentation identity; do not rebuild the editor on
        // every poll or allocate whole documents just to compare a view.
        let previous = if key.document.is_some() {
            self.view.editor_workbench.review.as_ref()
        } else {
            self.view
                .code_snapshot
                .as_ref()
                .and_then(|v| v.review.as_ref())
        };
        let changed = previous.is_none_or(|old| {
            old.token != state.token
                || old.document_id != state.document_id
                || old.revision != state.revision
                || old.current_hunk != state.current_hunk
                || old.hunk_count != state.hunk_count
                || old.file_count != state.file_count
                || old.current_file != state.current_file
                || old.busy != state.busy
                || old.menu_open != state.menu_open
                || old.can_accept != state.can_accept
                || old.reason != state.reason
                || old.can_reject != state.can_reject
                || old.reject_reason != state.reject_reason
        });
        if changed {
            state.actions = (0..state.hunks.len())
                .map(|index| openide_ui::CodeReviewHunkActions {
                    accept_id: format!(
                        "editor_review:accept_hunk:{}:{index}",
                        self.editor_review.token
                    ),
                    reject_id: format!(
                        "editor_review:reject_hunk:{}:{index}",
                        self.editor_review.token
                    ),
                    previous_id: Some(format!(
                        "editor_review:previous:{}:{index}",
                        self.editor_review.token
                    )),
                    next_id: Some(format!(
                        "editor_review:next:{}:{index}",
                        self.editor_review.token
                    )),
                    enabled: can_accept,
                    reject_enabled: can_reject,
                    disabled_reason: if can_accept {
                        state.reject_reason.clone()
                    } else {
                        state.reason.clone()
                    },
                })
                .collect::<Vec<_>>()
                .into();
            if key.document.is_some() {
                self.view.editor_workbench.review = Some(state);
            } else if let Some(snapshot) = self.view.code_snapshot.as_mut() {
                snapshot.review = Some(state);
                self.code_snapshots
                    .insert(snapshot.tab_id, snapshot.clone());
            }
        }
        changed
    }

    pub(super) fn poll_editor_review(&mut self, ctx: &egui::Context) -> bool {
        let mut key = self.editor_review_key();
        let mut changed = false;
        let response = match self.editor_review.pending.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(response)) => Some(response),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.editor_review.pending = None;
                self.editor_review.accepting = false;
                self.editor_review.loaded = None;
                self.view.status = "No se pudo completar la revisión del archivo. Vuelve a abrirlo para reintentar.".into();
                changed = true;
                None
            }
            _ => None,
        };
        if let Some(response) = response {
            let recover_rejection =
                response.decision == Some(FileReviewDecision::Reject) && response.result.is_err();
            self.editor_review.pending = None;
            self.editor_review.accepting = false;
            let mut removed_editor = false;
            if response.disk_changed {
                if let Ok(Some(review)) = &response.result {
                    let scope = &response.key.scope;
                    if review.current.is_none() {
                        if let Some(identity) = response.key.document {
                            if self.retire_reviewed_code_editor(
                                &scope.workspace,
                                &scope.path,
                                scope.tab,
                                identity,
                            ) {
                                if let Some(tab) = crate::dock::DockTabId::from_raw(scope.tab) {
                                    self.dock.close(
                                        &crate::dock::DockScope::for_owner(
                                            &scope.workspace,
                                            &scope.session,
                                        ),
                                        tab,
                                    );
                                    self.tree.forget_code_view(&crate::code_editor_ui::view_id(
                                        scope.tab,
                                    ));
                                    self.sync_dock_view();
                                    removed_editor = true;
                                }
                            }
                        }
                    } else if scope.call.is_some() && key.as_ref() == Some(&response.key) {
                        // Rejected deletion restores a real file. Open it only
                        // when its historical snapshot is still the active tab.
                        self.dock.open_file(
                            &scope.workspace,
                            &scope.session,
                            Path::new(&scope.path),
                        );
                        self.sync_dock_view();
                    }
                }
            }
            if (response.disk_changed && !removed_editor) || recover_rejection {
                // Reconcile the affected cached path even after switching tabs.
                // Worker and delivery both preserve dirty/saving documents.
                self.refresh_clean_code_editor_path(
                    &response.key.scope.workspace,
                    &response.key.scope.path,
                );
            }
            key = self.editor_review_key();
            if key.as_ref() == Some(&response.key) {
                self.editor_review.key = Some(response.key.clone());
                match response.result {
                    Ok(review) => {
                        if let Some(review) = &review {
                            self.editor_review.projection = Some(
                                review
                                    .blocks
                                    .iter()
                                    .map(|block| block.hunk.clone())
                                    .collect::<Vec<_>>()
                                    .into(),
                            );
                            if key.as_ref().is_some_and(|key| key.scope.call.is_some()) {
                                self.install_snapshot_review_content(review);
                            }
                        }
                        self.editor_review.loaded = review;
                        self.editor_review.token = uuid::Uuid::new_v4().to_string();
                        self.editor_review.menu_open = false;
                        self.editor_review.current_hunk = 0;
                        if let Some(decision) = response.decision {
                            self.view.status =
                                response.warning.clone().unwrap_or_else(|| match decision {
                                    FileReviewDecision::Accept => "Cambios aceptados".into(),
                                    FileReviewDecision::Reject => "Cambios rechazados".into(),
                                });
                        }
                    }
                    Err(error) => {
                        if response.decision.is_some() {
                            if let Some(review) = self.editor_review.loaded.as_mut() {
                                review.can_accept = false;
                                review.acceptance_unavailable = Some(error.clone());
                            }
                        } else {
                            self.editor_review.loaded = None;
                            self.editor_review.projection = None;
                        }
                        if response.decision.is_some() || response.key.scope.call.is_some() {
                            self.view.editor_workbench.error = Some(error);
                        }
                    }
                }
                changed = true;
            } else {
                // Even a completed acceptance of an old tab only invalidates
                // its cached decision. It never clears another file's review.
                self.editor_review.key = None;
            }
            if recover_rejection {
                // A filesystem write can succeed before publishing its durable
                // receipt fails. Reload once to reconcile that pending receipt;
                // never infer that an error means the file was left untouched.
                self.editor_review.key = None;
            }
        }
        let Some(key) = key else {
            self.editor_review.key = None;
            self.editor_review.loaded = None;
            return changed | self.clear_editor_review_surface();
        };
        if self.editor_review.key.as_ref() != Some(&key) {
            self.editor_review.menu_open = false;
        }
        let scope_changed = self
            .editor_review
            .key
            .as_ref()
            .is_none_or(|old| old.scope != key.scope);
        if scope_changed {
            self.editor_review.loaded = None;
            self.editor_review.projection = None;
            changed |= self.clear_editor_review_surface();
        }
        // Typing only invalidates the decorations for this buffer revision.
        // Reload immutable disk evidence after save, not on each keystroke.
        if key.dirty && !scope_changed && self.editor_review.pending.is_none() {
            self.editor_review.key = Some(key.clone());
        }
        if self.editor_review.key.as_ref() != Some(&key) && self.editor_review.pending.is_none() {
            if let Some(store) = &self.store {
                let storage = store.storage_root().to_path_buf();
                let requested = key.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                let spawn = std::thread::Builder::new()
                    .name("file-review-load".into())
                    .spawn(move || {
                        let scope = &requested.scope;
                        let result = match &scope.call {
                            Some(call) => session_changes::load_file_review(
                                &storage,
                                &scope.session,
                                &scope.workspace,
                                call,
                                &scope.path,
                            )
                            .map(Some),
                            None => session_changes::load_latest_file_review(
                                &storage,
                                &scope.session,
                                &scope.workspace,
                                &scope.path,
                            ),
                        }
                        .map_err(|e| e.to_string());
                        let _ = tx.send(Response {
                            key: requested,
                            decision: None,
                            disk_changed: false,
                            warning: None,
                            result,
                        });
                    });
                self.editor_review.key = Some(key.clone());
                if spawn.is_ok() {
                    self.editor_review.pending = Some(rx);
                } else {
                    self.view.status = "No se pudo cargar la revisión del archivo".into();
                }
            }
        }
        if key.document.is_some() && !key.dirty && self.editor_review.pending.is_none() {
            if let Some(position) = self
                .tree
                .code_review_current_hunk(&crate::code_editor_ui::view_id(key.scope.tab))
            {
                let action = format!(
                    "editor_review:accept_hunk:{}:{}",
                    self.editor_review.token, position.index
                );
                if key.document == Some((position.document_id, position.revision))
                    && position.accept_id == action
                {
                    self.editor_review.current_hunk = position.index;
                }
            }
        }
        if self.editor_review.loaded.is_some()
            && (self.view.session_changes.workspace.as_ref() != Some(&key.scope.workspace)
                || self.view.session_changes.session_id.as_deref()
                    != Some(key.scope.session.as_str())
                || (!self.view.session_changes.loading
                    && self.view.session_changes.result.is_none()))
        {
            // The environment normally keeps this snapshot current even while
            // the Changes tab is closed. Request its existing worker on first
            // direct editor entry as well; no filesystem work runs in render.
            self.request_session_changes();
        }
        if self.editor_review.menu_open {
            let anchor = crate::editor_review_ui::action(&self.editor_review.token, "menu_toggle");
            let decision =
                crate::editor_review_ui::action(&self.editor_review.token, "accept_file");
            let regions = &self.tree.frame().regions;
            // Only inspect a laid-out surface of this exact review generation.
            // A resized header can stop needing overflow; do not resurrect its
            // old menu when the pane becomes narrow again. The closed trigger
            // already exists in the frame that emitted menu_toggle.
            if regions.iter().any(|region| region.id == decision)
                && !regions.iter().any(|region| region.id == anchor)
            {
                self.editor_review.menu_open = false;
            }
        }
        changed |= self.publish_editor_review(&key);
        if self.editor_review.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        changed
    }

    fn install_snapshot_review_content(&mut self, review: &FileReview) {
        use crate::code_snapshot_ui::CodeSnapshotContent;
        use crate::git_review::ReviewLineKind;
        use openide_ui::{CodeDiffLine, CodeDiffLineKind};
        let Some(snapshot) = self.view.code_snapshot.as_mut() else {
            return;
        };
        if review.resolved {
            snapshot.caption = "Cambios revisados · versión registrada".into();
            self.code_snapshots
                .insert(snapshot.tab_id, snapshot.clone());
            return;
        }
        let lines: Arc<[CodeDiffLine]> = review
            .file
            .lines
            .iter()
            .filter_map(|line| {
                let kind = match line.kind {
                    ReviewLineKind::Added => CodeDiffLineKind::Added,
                    ReviewLineKind::Removed => CodeDiffLineKind::Removed,
                    ReviewLineKind::Context => CodeDiffLineKind::Context,
                    ReviewLineKind::Hunk => CodeDiffLineKind::Context,
                    _ => return None,
                };
                Some(CodeDiffLine {
                    kind,
                    text: if line.kind == ReviewLineKind::Hunk {
                        line.text.clone()
                    } else {
                        line.text.get(1..).unwrap_or_default().to_owned()
                    },
                    old_line: line.old_number,
                    new_line: line.new_number,
                })
            })
            .collect::<Vec<_>>()
            .into();
        snapshot.content = CodeSnapshotContent::Diff {
            lines,
            language: openide_ui::CodeLanguage::from_extension(
                Path::new(&review.path)
                    .extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or_default(),
            ),
            single_gutter: review.before.is_none() || review.after.is_none(),
        };
        snapshot.caption = if review.resolved {
            "Cambios revisados · versión registrada"
        } else if review.after.is_none() {
            "Archivo eliminado · cambios pendientes"
        } else {
            "Cambios registrados · solo lectura"
        }
        .into();
        self.code_snapshots
            .insert(snapshot.tab_id, snapshot.clone());
    }

    /// Context-specific review bindings. Native modifiers are checked before
    /// UI translation combines Ctrl/Cmd or drops Alt. Cursor moved rejection
    /// to Cmd+Shift+Backspace in 0.48.5; plain Ctrl+Backspace remains editing.
    /// F7/Shift+F7 adapt Code OSS's next/previous difference convention.
    pub(super) fn editor_review_shortcut_action(&self, event: &egui::Event) -> Option<UiAction> {
        let egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        else {
            return None;
        };
        if self.view.show_settings
            || self.view.show_command_center
            || self.view.main_page != MainPage::Workspace
            || (!self.ide_workspace_active() && !self.view.show_side_pane)
        {
            return None;
        }
        let (id, review) = if let Some(editor) = self.view.code_editor.as_ref() {
            (
                crate::code_editor_ui::view_id(editor.tab_id),
                self.view.editor_workbench.review.as_ref()?,
            )
        } else {
            let snapshot = self.view.code_snapshot.as_ref()?;
            (
                crate::code_snapshot_ui::view_id(snapshot.tab_id),
                snapshot.review.as_ref()?,
            )
        };
        if self.tree.focused_id() != Some(id.as_str()) {
            return None;
        }
        let primary = if cfg!(target_os = "macos") {
            modifiers.mac_cmd && !modifiers.ctrl
        } else {
            modifiers.ctrl && !modifiers.mac_cmd
        };
        let operation = match key {
            egui::Key::Enter if primary && !modifiers.shift && !modifiers.alt => "accept_file",
            egui::Key::Backspace if primary && modifiers.shift && !modifiers.alt => "reject_file",
            egui::Key::F7
                if !modifiers.ctrl
                    && !modifiers.mac_cmd
                    && !modifiers.command
                    && !modifiers.alt
                    && review.hunk_count > 0 =>
            {
                if modifiers.shift {
                    "previous"
                } else {
                    "next"
                }
            }
            _ => return None,
        };
        // The existing action handler rechecks token, document, dirty/running
        // state and worker availability. A disabled review still owns its chord
        // so a held decision key cannot become an edit while work is pending.
        Some(UiAction::Clicked {
            id: crate::editor_review_ui::action(&review.token, operation),
        })
    }

    pub(super) fn dismiss_editor_review_menu(&mut self) -> bool {
        if !self.editor_review.menu_open {
            return false;
        }
        self.editor_review.menu_open = false;
        if let Some(key) = self.editor_review_key() {
            self.publish_editor_review(&key);
        }
        true
    }

    pub(super) fn apply_editor_review_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let Some(raw) = id.strip_prefix("editor_review:") else {
            return false;
        };
        let Some((operation, target)) = raw.split_once(':') else {
            return true;
        };
        let (token, block_index) = match target.split_once(':') {
            Some((token, index)) => {
                let Ok(index) = index.parse::<usize>() else {
                    return true;
                };
                (token, Some(index))
            }
            None => (target, None),
        };
        let Some(key) = self.editor_review_key() else {
            return true;
        };
        if token != self.editor_review.token
            || self.editor_review.key.as_ref() != Some(&key)
            || self.editor_review.accepting
        {
            return true;
        }
        let Some(review) = self.editor_review.loaded.clone() else {
            return true;
        };
        if review.resolved {
            return true;
        }
        if matches!(operation, "menu_toggle" | "menu_dismiss") {
            if block_index.is_none() {
                self.editor_review.menu_open = operation == "menu_toggle"
                    && !self.editor_review.menu_open
                    && self.editor_review.pending.is_none();
                self.publish_editor_review(&key);
                ctx.request_repaint();
            }
            return true;
        }
        if matches!(
            operation,
            "previous_file"
                | "next_file"
                | "previous"
                | "next"
                | "accept_file"
                | "reject_file"
                | "accept_hunk"
                | "reject_hunk"
        ) {
            self.dismiss_editor_review_menu();
        }
        if matches!(operation, "previous_file" | "next_file") {
            if block_index.is_some()
                || self.editor_review.pending.is_some()
                || key.dirty
                || key.running
                || self
                    .view
                    .code_editor
                    .as_ref()
                    .is_some_and(|view| view.saving || view.conflict || view.loading)
            {
                return true;
            }
            let files = self.editor_review_files(&key);
            let Some(index) = files
                .iter()
                .position(|file| file.file.path == key.scope.path)
            else {
                return true;
            };
            let next = if operation == "previous_file" {
                index.saturating_sub(1)
            } else {
                (index + 1).min(files.len().saturating_sub(1))
            };
            if index == next {
                return true;
            }
            let path = files[next].file.path.clone();
            let deleted = files[next].file.status == "Eliminado";
            if deleted {
                if let Some(id) = self.editor_review_deleted_file_source(&key, &path) {
                    self.apply_file_activity_open(&UiAction::Clicked { id });
                } else {
                    self.view.status =
                        "Abre los cambios de este archivo desde su actividad confirmada.".into();
                }
            } else {
                self.close_environment();
                let scope = self.dock_scope();
                let tab = self
                    .dock
                    .open(&scope, DockTabKind::FilePreview { path: path.into() });
                self.dock.activate(&scope, tab);
                self.dock.set_collapsed(&scope, false);
                self.view.show_dock_overview = false;
                self.sync_dock_view();
                self.refresh_clean_code_editor();
            }
            ctx.request_repaint();
            return true;
        }
        if matches!(operation, "previous" | "next") {
            if block_index.is_some_and(|index| index != self.editor_review.current_hunk) {
                return true;
            }
            self.publish_editor_review(&key);
            if self
                .view
                .editor_workbench
                .review
                .as_ref()
                .is_none_or(|view| {
                    view.token != token
                        || view.busy
                        || view.hunk_count == 0
                        || key.document != Some((view.document_id, view.revision))
                })
            {
                return true;
            }
            let Some(hunks) = self.editor_review.projection.as_ref() else {
                return true;
            };
            if hunks.is_empty() {
                return true;
            }
            let next = if operation == "previous" {
                self.editor_review.current_hunk.saturating_sub(1)
            } else {
                (self.editor_review.current_hunk + 1).min(hunks.len() - 1)
            };
            if next == self.editor_review.current_hunk {
                return true;
            }
            let Some((tab, _, controller, snapshot)) = self.active_editor() else {
                return true;
            };
            let row = hunks[next]
                .new_rows
                .start
                .min(snapshot.line_count().saturating_sub(1));
            if let Ok(range) = snapshot.line_range(row) {
                if let Ok(mut buffer) = controller.shared_buffer().lock() {
                    let _ = buffer.set_selection(openide_editor::Selection::caret(range.start));
                }
                self.editor_review.current_hunk = next;
                self.tree
                    .reveal_code_review_hunk(&crate::code_editor_ui::view_id(tab), next);
                self.publish_editor_review(&key);
                ctx.request_repaint();
            }
            return true;
        }
        let (decision, target) = match operation {
            "accept_file" if block_index.is_none() => {
                (FileReviewDecision::Accept, FileReviewDecisionTarget::File)
            }
            "reject_file" if block_index.is_none() => {
                (FileReviewDecision::Reject, FileReviewDecisionTarget::File)
            }
            "accept_hunk" | "reject_hunk" => {
                let Some(block) = block_index.and_then(|index| review.blocks.get(index)) else {
                    return true;
                };
                if key.document.is_none() {
                    return true;
                }
                (
                    if operation == "accept_hunk" {
                        FileReviewDecision::Accept
                    } else {
                        FileReviewDecision::Reject
                    },
                    FileReviewDecisionTarget::Block(block.id.clone()),
                )
            }
            _ => return true,
        };
        if self.editor_review.pending.is_some() {
            return true;
        }
        self.publish_editor_review(&key);
        let enabled = if key.document.is_some() {
            self.view.editor_workbench.review.as_ref()
        } else {
            self.view
                .code_snapshot
                .as_ref()
                .and_then(|v| v.review.as_ref())
        }
        .is_some_and(|review| {
            !review.busy
                && match decision {
                    FileReviewDecision::Accept => review.can_accept,
                    FileReviewDecision::Reject => review.can_reject,
                }
        });
        if !enabled {
            return true;
        }
        match self
            .native_command_sessions
            .has_running_in_worktree(&key.scope.workspace)
        {
            Ok(false) => {}
            Ok(true) => {
                self.view.status =
                    "Espera a que terminen los comandos del proyecto antes de revisar los cambios."
                        .into();
                return true;
            }
            Err(error) => {
                self.view.status = error;
                return true;
            }
        }
        let Some(store) = self.store.as_ref() else {
            return true;
        };
        let storage = store.storage_root().to_path_buf();
        let requested = key.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("file-review-decision".into())
            .spawn(move || {
                let result = session_changes::decide_file_review(
                    &storage,
                    &review.session_id,
                    &review.workspace,
                    &review.tool_call_id,
                    &review.path,
                    &review.version,
                    &review.decision_version,
                    target,
                    decision,
                );
                let (result, disk_changed, warning) = match result {
                    Ok(result) => (Ok(Some(result.review)), result.disk_changed, result.warning),
                    Err(error) => (Err(error.to_string()), false, None),
                };
                let _ = tx.send(Response {
                    key: requested,
                    decision: Some(decision),
                    disk_changed,
                    warning,
                    result,
                });
            });
        if spawn.is_ok() {
            self.editor_review.pending = Some(rx);
            self.editor_review.accepting = true;
            self.publish_editor_review(&key);
            ctx.request_repaint();
        } else {
            self.view.status = "No se pudo guardar la decisión sobre el archivo".into();
        }
        true
    }
}

#[cfg(test)]
#[path = "editor_review_app_tests.rs"]
mod tests;
