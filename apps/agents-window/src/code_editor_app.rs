//! Host ownership for documents, file workers, and explicit destructive choices.
use super::*;
use crate::code_editor_files::{
    EditorFileOperation, EditorFileResponse, EditorFileScope, EditorFileWorker,
};
use crate::code_editor_ui::{self, CodeEditorView};
use openide_editor::{BufferSnapshot, Selection};
use openide_ui::EditorController;

const MAX_OPEN_DOCUMENTS: usize = 16;
type DocumentKey = (PathBuf, PathBuf);

#[derive(Clone, Debug)]
pub(super) struct EditorDocumentInfo {
    pub workspace: PathBuf,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub(super) struct EditorConflictView {
    pub disk: Option<EditorController>,
}

type DraftIdentity = (u64, u64, usize, usize);
fn draft_identity(snapshot: &BufferSnapshot) -> DraftIdentity {
    let selection = snapshot.selection();
    (
        snapshot.document_id(),
        snapshot.revision(),
        selection.anchor,
        selection.head,
    )
}

#[derive(Default)]
struct DraftTracking {
    observed: Option<DraftIdentity>,
    saved: Option<DraftIdentity>,
    queued: Option<(u64, Option<DraftIdentity>)>,
    due: Option<Instant>,
}

struct Document {
    scope: EditorFileScope,
    controller: Option<EditorController>,
    digest: Option<[u8; 32]>,
    loading: bool,
    saving: Option<(u64, u64)>,
    error: Option<String>,
    notice: Option<String>,
    canonical_location: Option<(PathBuf, PathBuf)>,
    disk_version: Option<EditorController>,
    disk_digest: Option<[u8; 32]>,
    missing: bool,
    recovered: bool,
    inspecting: bool,
    inspected_at: Option<Instant>,
    draft: DraftTracking,
    retired_drafts: Vec<EditorFileScope>,
    save_as_owner: Option<DockScope>,
}

pub(super) struct CodeEditorHost {
    worker: Option<EditorFileWorker>,
    documents: BTreeMap<DocumentKey, Document>,
    active: Option<DocumentKey>,
    generation: u64,
    allow_close: bool,
    retired_documents: BTreeSet<u64>,
    cached_views: VecDeque<(u64, u64)>,
    saved_workspaces: BTreeSet<PathBuf>,
    recovery_requested: bool,
    recovery_loading: bool,
    recovery_available: bool,
    recovery_error: Option<String>,
    recovery_token: u64,
    recovery_cleanup: VecDeque<EditorFileScope>,
    recovery_cleanup_pending: BTreeMap<u64, EditorFileScope>,
    recovered_keys: Vec<DocumentKey>,
    opened_documents: Vec<(DocumentKey, Option<u64>, Option<DockScope>)>,
    discard_and_close: bool,
    keep_and_close: bool,
}

impl Default for CodeEditorHost {
    fn default() -> Self {
        Self {
            worker: EditorFileWorker::new().ok(),
            documents: BTreeMap::new(),
            active: None,
            generation: 0,
            allow_close: false,
            retired_documents: BTreeSet::new(),
            cached_views: VecDeque::new(),
            saved_workspaces: BTreeSet::new(),
            recovery_requested: false,
            recovery_loading: false,
            recovery_available: false,
            recovery_error: None,
            recovery_token: 0,
            recovery_cleanup: VecDeque::new(),
            recovery_cleanup_pending: BTreeMap::new(),
            recovered_keys: Vec::new(),
            opened_documents: Vec::new(),
            discard_and_close: false,
            keep_and_close: false,
        }
    }
}

impl CodeEditorHost {
    pub(in crate::app) fn close_authorized(&self) -> bool {
        self.allow_close
    }

    pub(in crate::app) fn has_pending_changes(&self) -> bool {
        self.documents.values().any(|d| {
            d.saving.is_some()
                || d.controller
                    .as_ref()
                    .is_some_and(|c| c.snapshot().map_or(true, |s| s.is_dirty()))
        })
    }
    /// Whether a document at or under `relative` holds text not yet on disk.
    pub(in crate::app) fn has_unsaved_under(&self, workspace: &Path, relative: &Path) -> bool {
        self.documents.iter().any(|((root, path), d)| {
            root == workspace
                && path.starts_with(relative)
                && (d.saving.is_some()
                    || d.controller
                        .as_ref()
                        .is_some_and(|c| c.snapshot().map_or(true, |s| s.is_dirty())))
        })
    }
    fn active_document(&self) -> Option<&Document> {
        self.documents.get(self.active.as_ref()?)
    }
    fn view(&self) -> Option<CodeEditorView> {
        Self::document_view(self.active_document()?)
    }

    pub(in crate::app) fn view_for_path(
        &self,
        workspace: &Path,
        path: &Path,
    ) -> Option<CodeEditorView> {
        Self::document_view(
            self.documents
                .get(&(workspace.to_path_buf(), path.to_path_buf()))?,
        )
    }

    fn document_view(d: &Document) -> Option<CodeEditorView> {
        Some(CodeEditorView {
            tab_id: d.scope.tab_id,
            path: d.scope.relative_path.to_string_lossy().into_owned(),
            controller: d.controller.clone(),
            loading: d.loading,
            saving: d.saving.is_some(),
            error: d.error.clone(),
            notice: d.notice.clone(),
            conflict: d.disk_version.is_some() || d.missing,
            recovered: d.recovered,
            missing: d.missing,
        })
    }
    fn start_load(&mut self, tab_id: u64, workspace: PathBuf, relative_path: PathBuf) {
        let key = (workspace.clone(), relative_path.clone());
        self.active = Some(key.clone());
        if let Some(document) = self.documents.get_mut(&key) {
            // Tab identity may change on reopen; the document and pending writes do not.
            if !document.loading {
                document.scope.tab_id = tab_id;
            }
            return;
        }
        if self.documents.len() >= MAX_OPEN_DOCUMENTS {
            let evict = self
                .documents
                .iter()
                .find(|(_, d)| {
                    !d.loading
                        && d.saving.is_none()
                        && d.controller
                            .as_ref()
                            .is_none_or(|c| c.snapshot().is_ok_and(|s| !s.is_dirty()))
                })
                .map(|(key, _)| key.clone());
            if let Some(key) = evict {
                let removed = self.documents.remove(&key).expect("selected eviction");
                if self.recovery_available {
                    self.recovery_cleanup.push_back(removed.scope);
                }
                let identity = removed
                    .controller
                    .and_then(|controller| controller.snapshot().ok())
                    .map(|snapshot| snapshot.document_id());
                self.retired_documents.extend(identity);
            } else {
                return;
            }
        }
        self.generation = self.generation.saturating_add(1);
        let scope = EditorFileScope {
            tab_id,
            generation: self.generation,
            workspace,
            relative_path,
        };
        let result = self
            .worker
            .as_ref()
            .ok_or_else(|| "El servicio de archivos no está disponible".to_owned())
            .and_then(|w| w.try_load(scope.clone()).map_err(|e| e.to_string()));
        self.documents.insert(
            key,
            Document {
                scope,
                controller: None,
                digest: None,
                loading: result.is_ok(),
                saving: None,
                error: result.err(),
                notice: None,
                canonical_location: None,
                disk_version: None,
                disk_digest: None,
                missing: false,
                recovered: false,
                inspecting: false,
                inspected_at: None,
                draft: DraftTracking::default(),
                retired_drafts: Vec::new(),
                save_as_owner: None,
            },
        );
    }
    fn save(&mut self, storage_root: Option<PathBuf>) {
        let Some(key) = self.active.clone() else {
            return;
        };
        self.save_document(key, storage_root);
    }

    pub(in crate::app) fn save_for_path(
        &mut self,
        workspace: &Path,
        path: &Path,
        storage_root: Option<PathBuf>,
    ) {
        self.save_document((workspace.to_path_buf(), path.to_path_buf()), storage_root);
    }

    fn save_document(&mut self, key: (PathBuf, PathBuf), storage_root: Option<PathBuf>) {
        let Some(d) = self.documents.get_mut(&key) else {
            return;
        };
        if d.loading || d.saving.is_some() {
            return;
        }
        let Some(controller) = &d.controller else {
            return;
        };
        let snapshot = match controller.snapshot() {
            Ok(s) => s,
            Err(error) => {
                d.error = Some(error);
                return;
            }
        };
        if !snapshot.is_dirty() {
            return;
        }
        let identity = (snapshot.document_id(), snapshot.revision());
        let result = self
            .worker
            .as_ref()
            .ok_or_else(|| "El servicio de archivos no está disponible".to_owned())
            .and_then(|w| {
                let storage_root = storage_root.ok_or_else(|| {
                    "El historial no está disponible; no se guardó el archivo.".to_owned()
                })?;
                if d.missing || d.digest.is_none() {
                    w.try_save_as(
                        d.scope.clone(),
                        d.scope.relative_path.clone(),
                        snapshot,
                        storage_root,
                    )
                    .map_err(|e| e.to_string())
                } else {
                    w.try_save_with_history(
                        d.scope.clone(),
                        snapshot,
                        d.digest.expect("checked digest"),
                        storage_root,
                    )
                    .map_err(|e| e.to_string())
                }
            });
        d.error = result.as_ref().err().cloned();
        d.notice = None;
        if result.is_ok() {
            d.saving = Some(identity);
        }
    }
    fn reload(&mut self) {
        let Some(key) = self.active.clone() else {
            return;
        };
        let Some(d) = self.documents.get(&key) else {
            return;
        };
        if d.saving.is_some() {
            return;
        }
        let scope = d.scope.clone();
        if self.recovery_available {
            self.recovery_cleanup.push_back(scope.clone());
        }
        if let Some(snapshot) = d.controller.as_ref().and_then(|c| c.snapshot().ok()) {
            self.retired_documents.insert(snapshot.document_id());
        }
        if let Some(worker) = &self.worker {
            worker.cancel_loads(scope.tab_id);
        }
        self.documents.remove(&key);
        self.start_load(scope.tab_id, scope.workspace, scope.relative_path);
    }

    // An explicit file open from tool activity may follow an agent write. Check
    // a cached clean document off-thread; keep dirty buffers and unchanged IDs.
    fn refresh_clean(&mut self) {
        let Some(key) = self.active.clone() else {
            return;
        };
        self.refresh_clean_path(&key);
    }

    fn retire_exact_clean(
        &mut self,
        key: &(PathBuf, PathBuf),
        tab: u64,
        identity: (u64, u64),
    ) -> bool {
        let Some(document) = self.documents.get(key) else {
            return false;
        };
        let exact = !document.loading
            && document.saving.is_none()
            && document.scope.tab_id == tab
            && document
                .controller
                .as_ref()
                .and_then(|controller| controller.snapshot().ok())
                .is_some_and(|snapshot| {
                    !snapshot.is_dirty()
                        && (snapshot.document_id(), snapshot.revision()) == identity
                });
        if !exact {
            return false;
        }
        let document = self.documents.remove(key).expect("checked document");
        if self.recovery_available {
            self.recovery_cleanup.push_back(document.scope);
        }
        self.retired_documents.insert(identity.0);
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
        true
    }

    fn refresh_clean_path(&mut self, key: &(PathBuf, PathBuf)) {
        let Some(d) = self.documents.get_mut(key) else {
            return;
        };
        if d.loading
            || d.saving.is_some()
            || !d
                .controller
                .as_ref()
                .is_some_and(|c| c.snapshot().is_ok_and(|s| !s.is_dirty()))
        {
            return;
        }
        self.generation = self.generation.saturating_add(1);
        let mut scope = d.scope.clone();
        scope.generation = self.generation;
        let result = self
            .worker
            .as_ref()
            .ok_or_else(|| "El servicio de archivos no está disponible".to_owned())
            .and_then(|worker| {
                worker
                    .try_load(scope.clone())
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                d.scope = scope;
                d.loading = true;
                d.inspecting = false;
                d.error = None;
            }
            Err(error) => d.error = Some(error),
        }
    }

    fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(response) = self.worker.as_ref().and_then(|w| w.try_recv()) {
            match response {
                response @ (EditorFileResponse::RecoveryLoaded { .. }
                | EditorFileResponse::RecoveryStored { .. }
                | EditorFileResponse::Inspected { .. }
                | EditorFileResponse::SavedAs { .. }) => {
                    changed |= self.poll_lifecycle_response(response);
                }
                EditorFileResponse::Loaded {
                    scope,
                    canonical_workspace,
                    canonical_path,
                    buffer,
                    digest,
                } => {
                    let key = (scope.workspace.clone(), scope.relative_path.clone());
                    let Some(d) = self.documents.get_mut(&key) else {
                        continue;
                    };
                    if d.scope != scope || !d.loading {
                        continue;
                    }
                    if canonical_path != canonical_workspace.join(&scope.relative_path) {
                        d.loading = false;
                        d.error = Some(
                            "La respuesta del archivo no corresponde a la carpeta elegida".into(),
                        );
                        changed = true;
                        continue;
                    }
                    d.canonical_location = Some((canonical_workspace, canonical_path));
                    if let Some(controller) = &d.controller {
                        let current = controller.snapshot();
                        if current
                            .as_ref()
                            .map_or(true, |snapshot| snapshot.is_dirty())
                        {
                            // The user may type while the asynchronous check runs.
                            // Never replace those edits with the worker's snapshot.
                            d.loading = false;
                            if d.digest != Some(digest) {
                                d.notice = Some("El archivo cambió en disco. Se conservan tus cambios sin guardar.".into());
                            }
                            changed = true;
                            continue;
                        }
                        if d.digest == Some(digest) {
                            d.loading = false;
                            d.error = None;
                            changed = true;
                            continue;
                        }
                        if let Ok(snapshot) = current {
                            self.retired_documents.insert(snapshot.document_id());
                        }
                    }
                    d.controller = Some(EditorController::new(buffer));
                    d.digest = Some(digest);
                    d.loading = false;
                    d.error = None;
                    d.notice = None;
                    d.missing = false;
                    d.disk_version = None;
                    d.disk_digest = None;
                    changed = true;
                }
                EditorFileResponse::Saved {
                    scope,
                    canonical_workspace,
                    canonical_path,
                    document_id,
                    revision,
                    snapshot,
                    digest,
                    durability_warning,
                } => {
                    // The file effect and its history barriers are already
                    // committed even if this tab/document acknowledgement is stale.
                    self.saved_workspaces.insert(canonical_workspace.clone());
                    let key = (scope.workspace.clone(), scope.relative_path.clone());
                    let Some(d) = self.documents.get_mut(&key) else {
                        continue;
                    };
                    if d.scope.generation != scope.generation
                        || d.saving != Some((document_id, revision))
                    {
                        continue;
                    }
                    if d.canonical_location.as_ref() != Some(&(canonical_workspace, canonical_path))
                    {
                        d.saving = None;
                        d.error = Some("El archivo guardado pertenece a otra carpeta; se conserva el texto sin guardar".into());
                        changed = true;
                        continue;
                    }
                    if let Some(c) = &d.controller {
                        let shared = c.shared_buffer();
                        match shared.lock() {
                            Ok(mut buffer) if buffer.document_id() == document_id => {
                                if let Err(error) = buffer.mark_saved_snapshot(&snapshot) {
                                    d.error = Some(error.to_string());
                                } else {
                                    d.digest = Some(digest);
                                    d.error = None;
                                    d.notice = durability_warning;
                                    d.recovered = false;
                                    d.missing = false;
                                    d.disk_version = None;
                                    d.disk_digest = None;
                                    d.draft.due = Some(Instant::now());
                                }
                            }
                            _ => {
                                d.error =
                                    Some("La versión guardada pertenece a otro documento".into())
                            }
                        };
                    }
                    d.saving = None;
                    changed = true;
                }
                EditorFileResponse::Failed {
                    scope,
                    operation,
                    document_id,
                    revision,
                    error,
                } => {
                    let key = (scope.workspace.clone(), scope.relative_path.clone());
                    let Some(d) = self.documents.get_mut(&key) else {
                        continue;
                    };
                    if d.scope.generation != scope.generation {
                        continue;
                    }
                    if matches!(
                        operation,
                        EditorFileOperation::Save | EditorFileOperation::SaveAs
                    ) != document_id.is_some()
                    {
                        continue;
                    }
                    if let (Some(id), Some(rev)) = (document_id, revision) {
                        if d.saving != Some((id, rev)) {
                            continue;
                        }
                        d.saving = None;
                    } else {
                        if !d.loading || d.scope != scope {
                            continue;
                        }
                        d.loading = false;
                    }
                    d.error = Some(error.to_string());
                    changed = true;
                }
            }
        }
        changed
    }
}

impl AgentsWindow {
    pub(super) fn refresh_clean_code_editor(&mut self) {
        self.code_editor.refresh_clean();
        self.view.code_editor = self.code_editor.view();
    }

    pub(super) fn retire_reviewed_code_editor(
        &mut self,
        workspace: &Path,
        path: &str,
        tab: u64,
        identity: (u64, u64),
    ) -> bool {
        self.code_editor.retire_exact_clean(
            &(workspace.to_path_buf(), PathBuf::from(path)),
            tab,
            identity,
        )
    }

    pub(super) fn refresh_clean_code_editor_path(&mut self, workspace: &Path, path: &str) {
        self.code_editor
            .refresh_clean_path(&(workspace.to_path_buf(), PathBuf::from(path)));
        self.view.code_editor = self.code_editor.view();
    }

    fn maintain_code_editor_views(&mut self) {
        let retired = std::mem::take(&mut self.code_editor.retired_documents);
        self.code_editor.cached_views.retain(|(tab, document)| {
            if retired.contains(document) {
                self.tree.forget_code_view(&code_editor_ui::view_id(*tab));
                false
            } else {
                true
            }
        });
        let current = self.view.code_editor.as_ref().and_then(|view| {
            view.controller
                .as_ref()?
                .snapshot()
                .ok()
                .map(|snapshot| (view.tab_id, snapshot.document_id()))
        });
        if let Some((tab, document)) = current {
            self.code_editor
                .cached_views
                .retain(|(old_tab, old_document)| {
                    if *old_tab == tab {
                        if *old_document != document {
                            self.tree.forget_code_view(&code_editor_ui::view_id(tab));
                        }
                        false
                    } else {
                        true
                    }
                });
            self.code_editor.cached_views.push_back((tab, document));
        }
        while self.code_editor.cached_views.len() > MAX_OPEN_DOCUMENTS {
            if let Some((tab, _)) = self.code_editor.cached_views.pop_front() {
                self.tree.forget_code_view(&code_editor_ui::view_id(tab));
            }
        }
    }
    pub(super) fn sync_code_editor(&mut self) {
        let active = self.dock.active_tab(&self.dock_scope()).cloned();
        if self.view.ide.extensions_open
            && active.as_ref().map(|tab| tab.id.get()) != self.view.ide.extensions_editor_tab
        {
            self.view.ide.extensions_open = false;
        }
        match active {
            Some(tab) => match tab.kind {
                DockTabKind::FilePreview { ref path }
                    if matches!(
                        openide_preview::kind_for_path(path),
                        Some(
                            openide_preview::PreviewKind::Image | openide_preview::PreviewKind::Pdf
                        )
                    ) =>
                {
                    self.code_editor.active = None
                }
                DockTabKind::FilePreview { path } => self.code_editor.start_load(
                    tab.id.get(),
                    PathBuf::from(&self.view.workspace),
                    path,
                ),
                _ => self.code_editor.active = None,
            },
            None => self.code_editor.active = None,
        }
        self.view.code_editor = self.code_editor.view();
        self.view.code_editor_pending_saves = self
            .code_editor
            .documents
            .values()
            .any(|d| d.saving.is_some());
        self.sync_document_preview();
        if self.view.side_tab == SideTab::FilePreview
            && self.view.code_editor.is_none()
            && !self
                .view
                .document_preview
                .as_ref()
                .is_some_and(|view| view.kind != openide_preview::PreviewKind::Markdown)
        {
            if let Some(preview) = &self.view.file_preview {
                self.view.code_editor = Some(CodeEditorView { tab_id: preview.tab_id, path: preview.path.clone(), controller: None, loading: false, saving: false, conflict: false, recovered: false, missing: false, error: Some("Se alcanzó el límite de 16 documentos. Guarda algún archivo antes de abrir otro.".into()), notice: None });
            }
        }
        self.refresh_code_editor_titles();
        self.maintain_code_editor_views();
    }
    pub(super) fn refresh_code_editor_titles(&mut self) {
        let workspace = Path::new(&self.view.workspace);
        for tab in &mut self.view.dock_tabs {
            let dirty = self.code_editor.documents.values().any(|d| {
                d.scope.tab_id == tab.id
                    && d.scope.workspace.as_path() == workspace
                    && d.controller
                        .as_ref()
                        .is_some_and(|c| c.snapshot().is_ok_and(|s| s.is_dirty()))
            });
            if dirty && !tab.title.ends_with(" •") {
                tab.title.push_str(" •");
            }
            if !dirty {
                tab.title = tab.title.trim_end_matches(" •").to_owned();
            }
        }
    }
    pub(super) fn poll_code_editor(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = self.code_editor.poll();
        changed |= self.install_recovered_editor_tabs();
        self.code_editor.tick_lifecycle(Instant::now());
        let saved_workspaces = std::mem::take(&mut self.code_editor.saved_workspaces);
        for workspace in saved_workspaces {
            if workspace.as_path() == Path::new(&self.view.workspace) {
                self.environment_dirty = true;
                self.invalidate_workspace_files();
                self.invalidate_chat_diffs_after_editor_save(&workspace);
                changed = true;
            }
            // Inactive workspaces/sessions are freshly projected by the existing
            // scope-change path when selected; never mutate the current scope.
        }
        if changed {
            self.sync_code_editor();
        }
        if self
            .code_editor
            .documents
            .values()
            .any(|d| d.loading || d.saving.is_some() || d.inspecting)
        {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
        if self.code_editor.recovery_loading
            || !self.code_editor.recovery_cleanup_pending.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(20));
        } else if !self.code_editor.documents.is_empty() {
            let pending_draft = self
                .code_editor
                .documents
                .values()
                .any(|d| d.draft.queued.is_some() || d.draft.due.is_some());
            ctx.request_repaint_after(if pending_draft {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(2)
            });
        }
        if self.code_editor.discard_and_close
            && self.code_editor.recovery_cleanup.is_empty()
            && self.code_editor.recovery_cleanup_pending.is_empty()
            && self
                .code_editor
                .documents
                .values()
                .all(|d| d.draft.queued.is_none())
        {
            if self.code_editor.recovery_error.is_none() {
                self.code_editor.allow_close = true;
                self.request_close_after_debugger(ctx);
            } else {
                self.code_editor.discard_and_close = false;
                self.view.status = self.code_editor.recovery_error.clone().unwrap_or_default();
            }
        }
        if self.code_editor.keep_and_close {
            let durable = self.code_editor.documents.values().all(|d| {
                d.saving.is_none()
                    && d.draft.queued.is_none()
                    && d.controller.as_ref().is_none_or(|c| {
                        c.snapshot().is_ok_and(|s| {
                            !s.is_dirty() || d.draft.saved == Some(draft_identity(&s))
                        })
                    })
            });
            if self.code_editor.recovery_error.is_some() || !self.code_editor.recovery_available {
                self.code_editor.keep_and_close = false;
                self.view.status = self.code_editor.recovery_error.clone().unwrap_or_else(|| {
                    "No hay una copia de recuperación disponible; la ventana permanece abierta"
                        .into()
                });
            } else if durable
                && self.code_editor.recovery_cleanup.is_empty()
                && self.code_editor.recovery_cleanup_pending.is_empty()
            {
                self.code_editor.allow_close = true;
                self.request_close_after_debugger(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
        }
        changed
    }
    pub(super) fn guard_editor_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.code_editor.allow_close
            && self.code_editor.has_pending_changes()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.view.code_editor_quit_confirm = true;
        }
    }
    pub(super) fn cancel_editor_loads(&self) {
        if let Some(worker) = &self.code_editor.worker {
            worker.cancel_all_loads();
        }
    }
    pub(super) fn apply_code_editor_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let visible = self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && self.resource_pane_visible()
            && self.view.side_tab == SideTab::FilePreview;
        let source_visible = visible
            && self
                .view
                .document_preview
                .as_ref()
                .is_none_or(|view| view.is_source_visible());
        let matches = |id: &str, doc: u64, rev: u64, host: &CodeEditorHost| {
            source_visible
                && host.active_document().is_some_and(|d| {
                    id == code_editor_ui::view_id(d.scope.tab_id)
                        && d.controller.as_ref().is_some_and(|c| {
                            c.snapshot()
                                .is_ok_and(|s| s.document_id() == doc && s.revision() == rev)
                        })
                })
        };
        match action {
            UiAction::EditorChanged {
                id,
                document_id,
                revision,
            } => {
                if matches(id, *document_id, *revision, &self.code_editor) {
                    self.view.code_editor = self.code_editor.view();
                    self.refresh_code_editor_titles();
                }
            }
            UiAction::EditorSaveRequested {
                id,
                document_id,
                revision,
            } => {
                if matches(id, *document_id, *revision, &self.code_editor) {
                    self.code_editor
                        .save(self.store.as_ref().map(|s| s.storage_root().to_path_buf()));
                    self.view.code_editor = self.code_editor.view();
                }
            }
            UiAction::EditorFailed { id, error } => {
                if !source_visible {
                    return true;
                }
                if let Some(d) = self
                    .code_editor
                    .active
                    .clone()
                    .and_then(|key| self.code_editor.documents.get_mut(&key))
                {
                    if id == &code_editor_ui::view_id(d.scope.tab_id) {
                        d.error = Some(error.clone());
                        self.view.code_editor = self.code_editor.view();
                    }
                }
            }
            UiAction::Clicked { id } => match id.as_str() {
                "code_editor_quit_cancel" => {
                    self.view.code_editor_quit_confirm = false;
                    self.code_editor.discard_and_close = false;
                    self.code_editor.keep_and_close = false;
                    self.code_editor.allow_close = false;
                    self.cancel_close_after_debugger();
                }
                "code_editor_quit_discard" if self.view.code_editor_quit_confirm => {
                    if self.code_editor.discard_and_close {
                        return true;
                    }
                    if self
                        .code_editor
                        .documents
                        .values()
                        .any(|d| d.saving.is_some())
                    {
                        return true;
                    }
                    if self.code_editor.recovery_available {
                        self.code_editor.keep_and_close = false;
                        self.code_editor.discard_and_close = true;
                        self.code_editor.recovery_error = None;
                        self.code_editor
                            .recovery_cleanup
                            .extend(self.code_editor.documents.values().map(|d| d.scope.clone()));
                        ctx.request_repaint();
                    } else {
                        self.code_editor.allow_close = true;
                        self.view.code_editor_quit_confirm = false;
                        self.request_close_after_debugger(ctx);
                    }
                }
                "code_editor_quit_keep" if self.view.code_editor_quit_confirm => {
                    if self
                        .code_editor
                        .documents
                        .values()
                        .any(|d| d.saving.is_some())
                    {
                        return true;
                    }
                    if !self.code_editor.recovery_available {
                        self.view.status = "La recuperación no está disponible; guarda los archivos antes de cerrar".into();
                        return true;
                    }
                    self.code_editor.discard_and_close = false;
                    self.code_editor.keep_and_close = true;
                    self.code_editor.recovery_error = None;
                    ctx.request_repaint();
                }
                "code_editor_reload_cancel" => self.view.code_editor_reload_confirm = None,
                "code_editor_reload_confirm" => {
                    let scope = self.view.code_editor_reload_confirm.take();
                    if source_visible
                        && scope.as_ref() == self.code_editor.active_document().map(|d| &d.scope)
                    {
                        self.code_editor.reload();
                        self.view.code_editor = self.code_editor.view();
                    }
                }
                _ if id.starts_with("code_editor_save:")
                    || id.starts_with("code_editor_reload:") =>
                {
                    let Some((kind, raw)) = id.split_once(':') else {
                        return true;
                    };
                    let Some(d) = self.code_editor.active_document() else {
                        return true;
                    };
                    if raw.parse::<u64>().ok() != Some(d.scope.tab_id) || !visible {
                        return true;
                    }
                    if kind != "code_editor_save" && !source_visible {
                        return true;
                    }
                    self.view.editor_workbench.menu_open = false;
                    if kind == "code_editor_save" {
                        self.code_editor
                            .save(self.store.as_ref().map(|s| s.storage_root().to_path_buf()));
                    } else if d.saving.is_none() && !d.loading {
                        if d.controller
                            .as_ref()
                            .is_some_and(|c| c.snapshot().map_or(true, |s| s.is_dirty()))
                        {
                            self.view.code_editor_reload_confirm = Some(d.scope.clone());
                        } else {
                            self.code_editor.reload();
                        }
                    }
                    self.view.code_editor = self.code_editor.view();
                }
                _ => return false,
            },
            _ => return false,
        }
        self.view.code_editor_pending_saves = self
            .code_editor
            .documents
            .values()
            .any(|d| d.saving.is_some());
        self.maintain_code_editor_views();
        true
    }
}

#[cfg(test)]
#[path = "code_editor_app_tests.rs"]
mod tests;

#[path = "code_editor_lifecycle.rs"]
mod lifecycle;
