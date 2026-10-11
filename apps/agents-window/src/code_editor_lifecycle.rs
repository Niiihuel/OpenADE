//! Asynchronous document recovery, external versions and exclusive Save As.
use super::*;
use crate::code_editor_files::{EditorFileError, MAX_EDITABLE_FILE_BYTES};
use openide_editor::{EditorLimits, TextBuffer};
use std::path::Component;

const DRAFT_DEBOUNCE: Duration = Duration::from_millis(400);
const INSPECT_INTERVAL: Duration = Duration::from_secs(2);

fn relative_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|p| {
            matches!(
                p,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Err("Elige una ruta relativa dentro del proyecto, sin '..'.".into())
    } else if !path.components().any(|p| matches!(p, Component::Normal(_))) {
        Err("Indica un nombre de archivo.".into())
    } else {
        Ok(())
    }
}

impl CodeEditorHost {
    fn new_buffer_document(
        &mut self,
        scope: EditorFileScope,
        buffer: TextBuffer,
        base_digest: Option<[u8; 32]>,
        recovered: bool,
    ) -> Document {
        Document {
            scope,
            controller: Some(EditorController::new(buffer)),
            digest: base_digest,
            loading: false,
            saving: None,
            error: None,
            notice: recovered.then(|| {
                "Borrador recuperado. Tus cambios todavía no se guardaron en el archivo.".into()
            }),
            canonical_location: None,
            disk_version: None,
            disk_digest: None,
            missing: base_digest.is_none(),
            recovered,
            inspecting: false,
            inspected_at: None,
            draft: DraftTracking::default(),
            retired_drafts: Vec::new(),
            save_as_owner: None,
        }
    }

    /// The file or folder `old` of `workspace` moved to `new`, or is gone
    /// when `new` is `None`. A document with text follows it and keeps its
    /// buffer and tab; one that is gone, has no text yet, or whose new path
    /// already has a document is dropped. The caller refuses unsaved ones.
    pub(in crate::app) fn retarget(&mut self, workspace: &Path, old: &Path, new: Option<&Path>) {
        let moved = |key: &DocumentKey| {
            (key.0 == workspace)
                .then(|| crate::file_ops::moved(&key.1, old, new))
                .flatten()
                .map(|path| path.map(|path| (key.0.clone(), path)))
        };
        let keys: Vec<_> = self
            .documents
            .keys()
            .filter_map(|key| Some((key.clone(), moved(key)?)))
            .collect();
        for (key, next) in keys {
            let mut d = self.documents.remove(&key).expect("listed document");
            // A draft is stored by path: the old one is discarded, a new one starts.
            if self.recovery_available {
                self.recovery_cleanup.push_back(d.scope.clone());
            }
            let active = self.active.as_ref() == Some(&key);
            let next = next
                .filter(|next| d.controller.is_some() && !self.documents.contains_key(next));
            let Some(next) = next else {
                if let Some(worker) = &self.worker {
                    worker.cancel_loads(d.scope.tab_id);
                }
                let identity = d
                    .controller
                    .and_then(|controller| controller.snapshot().ok())
                    .map(|snapshot| snapshot.document_id());
                self.retired_documents.extend(identity);
                if active {
                    self.active = None;
                }
                continue;
            };
            // Answers still on their way to the old path no longer match.
            self.generation = self.generation.saturating_add(1);
            d.scope.generation = self.generation;
            d.scope.relative_path = next.1.clone();
            d.canonical_location = d
                .canonical_location
                .take()
                .map(|(root, _)| (root.clone(), root.join(&next.1)));
            d.loading = false;
            d.saving = None;
            d.missing = false;
            d.disk_version = None;
            d.disk_digest = None;
            d.inspecting = false;
            d.inspected_at = None;
            d.draft = DraftTracking::default();
            if active {
                self.active = Some(next.clone());
            }
            self.documents.insert(next, d);
        }
        self.recovered_keys = std::mem::take(&mut self.recovered_keys)
            .into_iter()
            .filter_map(|key| moved(&key).unwrap_or(Some(key)))
            .collect();
        self.opened_documents = std::mem::take(&mut self.opened_documents)
            .into_iter()
            .filter_map(|(key, tab, owner)| Some((moved(&key).unwrap_or(Some(key))?, tab, owner)))
            .collect();
    }

    pub(super) fn tick_lifecycle(&mut self, now: Instant) {
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        // Earlier explicit discards precede new edits of the same document.
        while let Some(scope) = self.recovery_cleanup.front().cloned() {
            self.recovery_token = self.recovery_token.saturating_add(1);
            let token = self.recovery_token;
            if worker
                .try_persist_recovery(scope.clone(), None, None, token)
                .is_err()
            {
                break;
            }
            self.recovery_cleanup.pop_front();
            self.recovery_cleanup_pending.insert(token, scope);
        }
        for d in self.documents.values_mut() {
            let Some(snapshot) = d.controller.as_ref().and_then(|c| c.snapshot().ok()) else {
                continue;
            };
            // Inspect one document per frame. The worker bounds reading and caches stat evidence.
            if !d.loading
                && d.saving.is_none()
                && !d.inspecting
                && d.inspected_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= INSPECT_INTERVAL)
            {
                if worker
                    .try_inspect(d.scope.clone(), d.disk_digest.or(d.digest))
                    .is_ok()
                {
                    d.inspecting = true;
                    d.inspected_at = Some(now);
                }
            }
            if !self.recovery_available || self.discard_and_close {
                continue;
            }
            let identity = snapshot.is_dirty().then(|| draft_identity(&snapshot));
            if d.draft.observed != identity {
                d.draft.observed = identity;
                d.draft.due = Some(now + DRAFT_DEBOUNCE);
            }
            if self.keep_and_close && d.draft.saved != identity {
                d.draft.due = Some(now);
            }
            if d.draft.saved == identity && d.draft.queued.is_none() {
                d.draft.due = None;
                continue;
            }
            if d.draft.queued.is_some()
                || d.draft.saved == identity
                || d.draft.due.is_none_or(|due| now < due)
                || self.recovery_cleanup.iter().any(|scope| {
                    scope.workspace == d.scope.workspace
                        && scope.relative_path == d.scope.relative_path
                })
            {
                continue;
            }
            self.recovery_token = self.recovery_token.saturating_add(1);
            let token = self.recovery_token;
            if worker
                .try_persist_recovery(d.scope.clone(), d.digest, identity.map(|_| snapshot), token)
                .is_ok()
            {
                d.draft.queued = Some((token, identity));
                d.draft.due = None;
            }
        }
    }

    pub(super) fn poll_lifecycle_response(&mut self, response: EditorFileResponse) -> bool {
        match response {
            EditorFileResponse::RecoveryLoaded {
                drafts,
                errors,
                available,
            } => {
                self.recovery_loading = false;
                self.recovery_available = available;
                self.recovery_error = (!errors.is_empty()).then(|| errors.join("\n"));
                for recovered in drafts {
                    let key = (recovered.workspace.clone(), recovered.relative_path.clone());
                    if self.documents.get(&key).is_some_and(|d| {
                        d.saving.is_some()
                            || d.controller
                                .as_ref()
                                .is_some_and(|c| c.snapshot().map_or(true, |s| s.is_dirty()))
                    }) || (self.documents.len() >= MAX_OPEN_DOCUMENTS
                        && !self.documents.contains_key(&key))
                    {
                        self.recovery_error = Some("Hay un borrador conservado que no pudo abrirse automáticamente; no se reemplazó ningún documento activo.".into());
                        // Never overwrite the preexisting recovery copy with the
                        // concurrent in-memory draft that prevented its restoration.
                        self.recovery_available = false;
                        continue;
                    }
                    let existing_tab = self
                        .documents
                        .get(&key)
                        .map(|d| d.scope.tab_id)
                        .unwrap_or(0);
                    if let Some(previous) = self.documents.remove(&key) {
                        if let Some(snapshot) =
                            previous.controller.as_ref().and_then(|c| c.snapshot().ok())
                        {
                            self.retired_documents.insert(snapshot.document_id());
                        }
                    }
                    if let Some(worker) = &self.worker {
                        worker.cancel_loads(existing_tab);
                    }
                    self.generation = self.generation.saturating_add(1);
                    let scope = EditorFileScope {
                        tab_id: existing_tab,
                        generation: self.generation,
                        workspace: recovered.workspace,
                        relative_path: recovered.relative_path,
                    };
                    let identity = draft_identity(&recovered.buffer.snapshot());
                    let mut document = self.new_buffer_document(
                        scope,
                        recovered.buffer,
                        recovered.base_digest,
                        true,
                    );
                    document.draft.observed = Some(identity);
                    document.draft.saved = Some(identity);
                    self.documents.insert(key.clone(), document);
                    self.recovered_keys.push(key);
                }
            }
            EditorFileResponse::RecoveryStored {
                scope,
                token,
                result,
            } => {
                if self.recovery_cleanup_pending.remove(&token).is_some() {
                    if let Some(d) = self
                        .documents
                        .get_mut(&(scope.workspace, scope.relative_path))
                    {
                        // An unlink may have succeeded even if directory sync
                        // failed. Re-establish recovery if closing is cancelled.
                        d.draft.saved = None;
                        d.draft.due = Some(Instant::now());
                    }
                    match result {
                        Err(error) => self.recovery_error = Some(error),
                        Ok(()) => {}
                    }
                    return true;
                }
                let key = (scope.workspace, scope.relative_path);
                if let Some(d) = self.documents.get_mut(&key) {
                    if let Some((queued, identity)) = d.draft.queued {
                        if queued == token {
                            d.draft.queued = None;
                            match result {
                                Ok(()) => {
                                    d.draft.saved = identity;
                                    d.draft.due = Some(Instant::now());
                                    self.recovery_cleanup
                                        .extend(std::mem::take(&mut d.retired_drafts));
                                }
                                Err(error) => {
                                    d.notice = Some(format!(
                                        "El borrador aún no tiene copia de recuperación: {error}"
                                    ));
                                    d.draft.due = Some(Instant::now() + Duration::from_secs(2));
                                    self.recovery_error = Some(error);
                                }
                            }
                        }
                    }
                }
            }
            EditorFileResponse::Inspected { scope, result } => {
                let key = (scope.workspace.clone(), scope.relative_path.clone());
                let Some(d) = self.documents.get_mut(&key) else {
                    return false;
                };
                if d.scope.generation != scope.generation || !d.inspecting {
                    return false;
                }
                d.inspecting = false;
                if d.saving.is_some() || d.loading {
                    return false;
                }
                let dirty = d
                    .controller
                    .as_ref()
                    .is_some_and(|c| c.snapshot().map_or(true, |s| s.is_dirty()));
                match result {
                    Ok(Some((buffer, digest))) if dirty => {
                        if d.digest == Some(digest) {
                            d.disk_version = None;
                            d.disk_digest = None;
                            d.missing = false;
                            d.canonical_location = Some((
                                scope.workspace.clone(),
                                scope.workspace.join(&scope.relative_path),
                            ));
                            d.notice = d.recovered.then(|| "Borrador recuperado. Tus cambios todavía no se guardaron en el archivo.".into());
                            return true;
                        }
                        d.disk_version = Some(EditorController::new(buffer));
                        d.disk_digest = Some(digest);
                        d.missing = false;
                        d.canonical_location = Some((
                            scope.workspace.clone(),
                            scope.workspace.join(&scope.relative_path),
                        ));
                        d.notice = Some("El archivo cambió en disco. Tus cambios se conservan; compara ambas versiones antes de guardar.".into());
                    }
                    Ok(Some((mut buffer, digest))) => {
                        if let Some(snapshot) =
                            d.controller.as_ref().and_then(|c| c.snapshot().ok())
                        {
                            let selection = snapshot.selection();
                            let chars = buffer.snapshot().char_count();
                            let _ = buffer.set_selection(Selection {
                                anchor: selection.anchor.min(chars),
                                head: selection.head.min(chars),
                            });
                            self.retired_documents.insert(snapshot.document_id());
                        }
                        d.controller = Some(EditorController::new(buffer));
                        d.digest = Some(digest);
                        d.disk_version = None;
                        d.disk_digest = None;
                        d.missing = false;
                        d.error = None;
                        d.notice = Some("Archivo actualizado con los cambios del disco.".into());
                        d.canonical_location = Some((
                            scope.workspace.clone(),
                            scope.workspace.join(&scope.relative_path),
                        ));
                    }
                    Ok(None) => {
                        if !dirty && d.disk_version.is_some() {
                            if let Some(snapshot) =
                                d.controller.as_ref().and_then(|c| c.snapshot().ok())
                            {
                                self.retired_documents.insert(snapshot.document_id());
                            }
                            d.controller = d.disk_version.take();
                            d.digest = d.disk_digest.take();
                            d.notice =
                                Some("Archivo actualizado con los cambios del disco.".into());
                        } else if d.missing {
                            d.notice =
                                Some("El archivo volvió a estar disponible en disco.".into());
                        }
                        d.missing = false;
                        d.canonical_location = Some((
                            scope.workspace.clone(),
                            scope.workspace.join(&scope.relative_path),
                        ));
                    }
                    Err(EditorFileError::Io {
                        kind: std::io::ErrorKind::NotFound,
                        ..
                    }) => {
                        d.missing = true;
                        d.disk_version = None;
                        d.disk_digest = None;
                        d.notice = Some("El archivo ya no existe en disco. Se conserva el contenido; usa Guardar como para recuperarlo.".into());
                        // A clean cached document is now the only remaining copy.
                        if !dirty {
                            if let Some(controller) = &d.controller {
                                if let Ok(mut buffer) = controller.shared_buffer().lock() {
                                    buffer.mark_unsaved();
                                }
                            }
                        }
                    }
                    Err(error) => {
                        d.notice = Some(format!(
                            "No se pudo comprobar el archivo; se conserva tu versión: {error}"
                        ))
                    }
                }
            }
            EditorFileResponse::SavedAs {
                scope,
                destination,
                snapshot,
                canonical_workspace,
                canonical_path,
                digest,
                durability_warning,
            } => {
                self.saved_workspaces.insert(canonical_workspace.clone());
                let key = (scope.workspace.clone(), scope.relative_path.clone());
                let Some(d) = self.documents.get(&key) else {
                    return false;
                };
                if d.scope.generation != scope.generation
                    || d.saving != Some((snapshot.document_id(), snapshot.revision()))
                {
                    return false;
                }
                let mut d = self.documents.remove(&key).expect("validated document");
                if let Some(c) = &d.controller {
                    if let Ok(mut buffer) = c.shared_buffer().lock() {
                        if let Err(error) = buffer.mark_saved_snapshot(&snapshot) {
                            d.error = Some(error.to_string());
                        }
                    }
                }
                let next = (scope.workspace.clone(), destination.clone());
                if next != key && self.recovery_available {
                    if d.controller
                        .as_ref()
                        .is_some_and(|c| c.snapshot().is_ok_and(|s| s.is_dirty()))
                    {
                        d.retired_drafts.push(scope.clone());
                    } else {
                        self.recovery_cleanup.push_back(scope.clone());
                    }
                }
                let previous_tab = scope.tab_id;
                let owner = d.save_as_owner.take();
                self.generation = self.generation.saturating_add(1);
                d.scope.relative_path = destination;
                d.scope.generation = self.generation;
                d.scope.tab_id = 0;
                d.saving = None;
                d.digest = Some(digest);
                d.canonical_location = Some((canonical_workspace, canonical_path));
                d.disk_version = None;
                d.disk_digest = None;
                d.missing = false;
                d.recovered = false;
                d.notice = durability_warning;
                d.inspecting = false;
                if next != key {
                    d.draft = DraftTracking::default();
                }
                self.documents.insert(next.clone(), d);
                if self.active.as_ref() == Some(&key) {
                    self.active = Some(next.clone());
                }
                self.opened_documents
                    .push((next, Some(previous_tab), owner));
            }
            _ => return false,
        }
        true
    }
}

impl AgentsWindow {
    pub(in crate::app) fn ensure_code_editor_loaded(
        &mut self,
        path: &Path,
    ) -> Result<Option<EditorController>, String> {
        relative_path(path)?;
        let workspace = PathBuf::from(&self.view.workspace);
        let key = (workspace.clone(), path.to_path_buf());
        if let Some(document) = self.code_editor.documents.get(&key) {
            if let Some(controller) = &document.controller {
                return Ok(Some(controller.clone()));
            }
            if document.loading {
                return Ok(None);
            }
            return Err(document
                .error
                .clone()
                .unwrap_or_else(|| "El documento no está disponible".into()));
        }
        // Background loads need independent cancellation tokens. They are not
        // dock tabs and must never replace the current presentation or focus.
        let background_id = u64::MAX.saturating_sub(self.code_editor.generation);
        let active = self.code_editor.active.clone();
        self.code_editor
            .start_load(background_id, workspace, path.to_path_buf());
        self.code_editor.active = active;
        let document =
            self.code_editor.documents.get(&key).ok_or(
                "Se alcanzó el límite de 16 documentos; guarda un archivo antes de continuar",
            )?;
        if let Some(error) = &document.error {
            return Err(error.clone());
        }
        Ok(document.controller.clone())
    }

    pub(in crate::app) fn code_editor_controller(
        &self,
        relative: &Path,
    ) -> Option<EditorController> {
        self.code_editor
            .documents
            .get(&(PathBuf::from(&self.view.workspace), relative.to_path_buf()))?
            .controller
            .clone()
    }
    pub(in crate::app) fn init_code_editor_recovery(&mut self) {
        if self.code_editor.recovery_requested {
            return;
        }
        match self
            .code_editor
            .worker
            .as_ref()
            .ok_or_else(|| "El servicio del editor no está disponible".to_owned())
            .and_then(|worker| {
                worker
                    .try_init_recovery(self.data_root.join("editor-drafts"))
                    .map_err(|e| e.to_string())
            }) {
            Ok(()) => {
                self.code_editor.recovery_requested = true;
                self.code_editor.recovery_loading = true;
            }
            Err(error) => self.view.status = format!("No se pudo iniciar la recuperación: {error}"),
        }
    }

    pub(in crate::app) fn code_editor_documents(&self) -> Vec<EditorDocumentInfo> {
        self.code_editor
            .documents
            .values()
            .map(|d| EditorDocumentInfo {
                workspace: d.scope.workspace.clone(),
                path: d.scope.relative_path.clone(),
            })
            .collect()
    }

    pub(in crate::app) fn code_editor_conflict(&self) -> Option<EditorConflictView> {
        let d = self.code_editor.active_document()?;
        d.controller.as_ref()?;
        (d.disk_version.is_some() || d.missing).then(|| EditorConflictView {
            disk: d.disk_version.clone(),
        })
    }

    pub(in crate::app) fn code_editor_save_as(&mut self, relative: PathBuf) -> Result<(), String> {
        relative_path(&relative)?;
        let owner = self.dock_scope();
        let key = self
            .code_editor
            .active
            .clone()
            .ok_or("No hay un documento activo")?;
        let destination_key = (key.0.clone(), relative.clone());
        if destination_key != key && self.code_editor.documents.contains_key(&destination_key) {
            return Err("Ese destino ya tiene un documento abierto; elige otro nombre".into());
        }
        let d = self
            .code_editor
            .documents
            .get_mut(&key)
            .ok_or("No hay un documento activo")?;
        if d.loading || d.saving.is_some() {
            return Err("Espera a que termine la operación actual".into());
        }
        let snapshot = d
            .controller
            .as_ref()
            .ok_or("No hay contenido para guardar")?
            .snapshot()?;
        let identity = (snapshot.document_id(), snapshot.revision());
        let store = self
            .store
            .as_ref()
            .ok_or("El historial no está disponible; no se guardó el archivo")?
            .storage_root()
            .to_path_buf();
        self.code_editor
            .worker
            .as_ref()
            .ok_or("El servicio del editor no está disponible")?
            .try_save_as(d.scope.clone(), relative, snapshot, store)
            .map_err(|e| e.to_string())?;
        d.saving = Some(identity);
        d.save_as_owner = Some(owner);
        d.error = None;
        self.view.code_editor = self.code_editor.view();
        self.view.code_editor_pending_saves = true;
        Ok(())
    }

    pub(in crate::app) fn code_editor_create(
        &mut self,
        relative: PathBuf,
        content: String,
    ) -> Result<(), String> {
        relative_path(&relative)?;
        let workspace = PathBuf::from(&self.view.workspace);
        let key = (workspace.clone(), relative.clone());
        if self.code_editor.documents.contains_key(&key) {
            return Err("Ese archivo ya tiene un documento abierto".into());
        }
        if self.code_editor.documents.len() >= MAX_OPEN_DOCUMENTS {
            return Err("Guarda o cierra un documento antes de crear otro".into());
        }
        let buffer = TextBuffer::restore_unsaved(
            &content,
            Selection::caret(0),
            EditorLimits {
                max_document_bytes: MAX_EDITABLE_FILE_BYTES,
                ..EditorLimits::default()
            },
        )
        .map_err(|e| e.to_string())?;
        self.code_editor.generation = self.code_editor.generation.saturating_add(1);
        let scope = EditorFileScope {
            tab_id: 0,
            generation: self.code_editor.generation,
            workspace,
            relative_path: relative.clone(),
        };
        let document = self
            .code_editor
            .new_buffer_document(scope, buffer, None, false);
        self.code_editor.documents.insert(key.clone(), document);
        self.code_editor.active = Some(key.clone());
        self.code_editor
            .opened_documents
            .push((key, None, Some(self.dock_scope())));
        self.install_recovered_editor_tabs();
        self.code_editor_save_as(relative)
    }

    /// Explicit discard only; external changes and failed saves never call this.
    pub(in crate::app) fn code_editor_discard_active(&mut self) -> Result<(), String> {
        let d = self
            .code_editor
            .active_document()
            .ok_or("No hay un documento activo")?;
        if d.saving.is_some() {
            return Err("Espera a que termine el guardado".into());
        }
        self.code_editor.reload();
        self.view.code_editor = self.code_editor.view();
        Ok(())
    }

    pub(in crate::app) fn install_recovered_editor_tabs(&mut self) -> bool {
        let workspace = PathBuf::from(&self.view.workspace);
        let owner = self.view.selected_session.clone().unwrap_or_default();
        let mut open = std::mem::take(&mut self.code_editor.opened_documents);
        self.code_editor.recovered_keys.retain(|key| {
            if key.0 == workspace {
                open.push((key.clone(), None, None));
                false
            } else {
                true
            }
        });
        let mut changed = false;
        for (key, old_tab, owner_scope) in open {
            if key.0 != workspace && owner_scope.is_none() {
                self.code_editor.recovered_keys.push(key);
                continue;
            }
            let target = owner_scope
                .or_else(|| {
                    old_tab
                        .and_then(|id| {
                            self.dock
                                .tabs(&key.0.to_string_lossy())
                                .iter()
                                .find(|tab| tab.id.get() == id)
                        })
                        .map(|tab| DockScope::new(tab.workspace_id.clone(), tab.owner.clone()))
                })
                .unwrap_or_else(|| DockScope::for_owner(&workspace, &owner));
            let current = self.dock_scope();
            let previous_active = self.dock.active_tab(&target).map(|tab| tab.id);
            let collapsed = self.dock.is_collapsed(&target);
            let follow_open = old_tab.is_none()
                || (current == target && previous_active.map(|id| id.get()) == old_tab);
            let tab = self.dock.open(
                &target,
                DockTabKind::FilePreview {
                    path: key.1.clone(),
                },
            );
            {
                if let Some(d) = self.code_editor.documents.get_mut(&key) {
                    d.scope.tab_id = tab.get();
                }
                if let Some(old_tab) = old_tab
                    .and_then(DockTabId::from_raw)
                    .filter(|old| *old != tab)
                {
                    self.dock.close(&target, old_tab);
                    self.tree
                        .forget_code_view(&code_editor_ui::view_id(old_tab.get()));
                }
                if !follow_open {
                    if let Some(previous) =
                        previous_active.filter(|id| self.dock.contains_tab(id.get()))
                    {
                        self.dock.activate(&target, previous);
                    }
                    self.dock.set_collapsed(&target, collapsed);
                }
                changed = true;
            }
        }
        if changed {
            self.sync_dock_view();
        }
        if let Some(error) = &self.code_editor.recovery_error {
            self.view.status = format!("Recuperación del editor: {error}");
        }
        changed
    }
}

#[cfg(test)]
#[path = "code_editor_lifecycle_tests.rs"]
mod tests;
