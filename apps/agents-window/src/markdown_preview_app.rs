//! The secondary Markdown tab owns its presentation and render lifecycle, while
//! its source remains in the shared editor document store.
use super::*;
use crate::document_preview::{MarkdownPreviewTab, PreviewMode, PreviewView};
use openide_editor::BufferSnapshot;
use openide_preview::{Document, PreviewKind, Request, Scope, Worker};

#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceIdentity {
    Disk,
    Loading,
    Buffer(u64, u64),
    Error(String),
}

enum Source {
    Disk,
    Loading,
    Buffer(BufferSnapshot),
    Error(String),
}

impl Source {
    fn identity(&self) -> SourceIdentity {
        match self {
            Self::Disk => SourceIdentity::Disk,
            Self::Loading => SourceIdentity::Loading,
            Self::Buffer(snapshot) => {
                SourceIdentity::Buffer(snapshot.document_id(), snapshot.revision())
            }
            Self::Error(error) => SourceIdentity::Error(error.clone()),
        }
    }
}

#[derive(Default)]
pub(super) struct MarkdownPreviewHost {
    worker: Option<Worker>,
    owner: Option<DockScope>,
    next_tab: u64,
    next_generation: u64,
    observed: Option<SourceIdentity>,
    due: Option<Instant>,
    // Preserve the initiating input modality across the preview's first layout.
    focus_pending: Option<bool>,
    pending_anchor: Option<(u64, String)>,
}

impl AgentsWindow {
    pub(super) fn handle_markdown_preview_key(
        &mut self,
        event: &UiEvent,
        ctx: &egui::Context,
    ) -> bool {
        if !matches!(
            event,
            UiEvent::Key {
                key: openide_ui::Key::S,
                ctrl: true,
                shift: false
            }
        ) || self.view.main_page != MainPage::Workspace
            || self.view.show_settings
            || self.view.show_dock_overview
            || self.view.ide.extensions_open
            || !self.resource_pane_visible()
        {
            return false;
        }
        let Some(tab) = &self.view.markdown_preview else {
            return false;
        };
        let prefix = format!("{}:", tab.action_prefix());
        if self.markdown_preview.owner.as_ref() != Some(&self.dock_scope())
            || tab.preview.scope.workspace != Path::new(&self.view.workspace)
            || !self
                .tree
                .focused_id()
                .is_some_and(|id| id.starts_with(&prefix))
        {
            return false;
        }
        // This tab can refer to A while the primary editor displays B. Saving
        // by document identity preserves the active editor and keyboard focus.
        self.code_editor.save_for_path(
            &tab.preview.scope.workspace,
            &tab.preview.scope.path,
            self.store
                .as_ref()
                .map(|store| store.storage_root().to_path_buf()),
        );
        ctx.request_repaint();
        true
    }

    pub(super) fn open_markdown_preview(&mut self, ctx: &egui::Context) {
        let owner = self.dock_scope();
        let Some(tab) = self.dock.active_tab(&owner).cloned() else {
            return;
        };
        let DockTabKind::FilePreview { path } = tab.kind else {
            return;
        };
        if openide_preview::kind_for_path(&path) != Some(PreviewKind::Markdown) {
            return;
        }
        if self.markdown_preview.owner.as_ref() == Some(&owner)
            && self.view.markdown_preview.as_ref().is_some_and(|preview| {
                preview.preview.scope.workspace == Path::new(&self.view.workspace)
                    && preview.preview.scope.path == path
            })
        {
            self.view.markdown_preview.as_mut().unwrap().source_tab_id = tab.id.get();
            self.markdown_preview.focus_pending = Some(self.tree.has_keyboard_focus());
        } else {
            self.replace_markdown_preview(owner, path, tab.id.get());
        }
        self.view.ide.extensions_open = false;
        self.view.ide.compact_panel = None;
        self.view.show_dock_overview = false;
        self.view.show_dock_add_menu = false;
        self.view.show_dock_tabs_menu = false;
        self.view.editor_workbench.menu_open = false;
        self.view.editor_workbench.search_open = false;
        self.view.editor_workbench.form = None;
        self.editor_services.focus_next = None;
        ctx.request_repaint();
    }

    fn replace_markdown_preview(&mut self, owner: DockScope, path: PathBuf, source_tab_id: u64) {
        self.close_markdown_preview(false);
        self.markdown_preview.next_tab = self.markdown_preview.next_tab.wrapping_add(1).max(1);
        self.markdown_preview.next_generation =
            self.markdown_preview.next_generation.wrapping_add(1);
        let id = self.markdown_preview.next_tab;
        self.view.markdown_preview = Some(MarkdownPreviewTab {
            id,
            source_tab_id,
            preview: PreviewView {
                scope: Scope {
                    workspace: PathBuf::from(&self.view.workspace),
                    path,
                    tab_id: id,
                    generation: self.markdown_preview.next_generation,
                },
                kind: PreviewKind::Markdown,
                mode: PreviewMode::Preview,
                document: None,
                raster: None,
                loading: true,
                error: None,
                page: 0,
                pages: 0,
                zoom: None,
            },
        });
        self.markdown_preview.owner = Some(owner);
        self.markdown_preview.due = Some(Instant::now());
        self.markdown_preview.focus_pending = Some(self.tree.has_keyboard_focus());
    }

    fn close_markdown_preview(&mut self, focus_source: bool) {
        if let Some(worker) = &self.markdown_preview.worker {
            worker.cancel();
        }
        self.view.markdown_preview = None;
        self.markdown_preview.owner = None;
        self.markdown_preview.observed = None;
        self.markdown_preview.due = None;
        self.markdown_preview.focus_pending = None;
        self.markdown_preview.pending_anchor = None;
        if focus_source {
            self.focus_visible_markdown_source();
        }
    }

    fn focus_visible_markdown_source(&mut self) {
        let visible = self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && !self.view.ide.extensions_open
            && self.resource_pane_visible()
            && self.view.side_tab == SideTab::FilePreview
            && self
                .view
                .document_preview
                .as_ref()
                .is_none_or(PreviewView::is_source_visible);
        self.editor_services.focus_next = visible
            .then(|| self.view.code_editor.as_ref())
            .flatten()
            .map(|editor| crate::code_editor_ui::view_id(editor.tab_id));
    }

    fn markdown_preview_source(&self) -> Source {
        let Some(tab) = &self.view.markdown_preview else {
            return Source::Error("La vista previa está cerrada.".into());
        };
        let Some(editor) = self
            .code_editor
            .view_for_path(&tab.preview.scope.workspace, &tab.preview.scope.path)
        else {
            return Source::Disk;
        };
        if editor.loading {
            return Source::Loading;
        }
        if let Some(controller) = editor.controller {
            return match controller.snapshot() {
                Ok(snapshot) => Source::Buffer(snapshot),
                Err(error) => Source::Error(error.to_string()),
            };
        }
        Source::Error(editor.error.unwrap_or_else(|| {
            "No se pudo cargar el buffer de Markdown. Abrí el código para revisar el archivo."
                .into()
        }))
    }

    fn invalidate_markdown_preview(&mut self) {
        if let Some(worker) = &self.markdown_preview.worker {
            worker.cancel();
        }
        self.markdown_preview.next_generation =
            self.markdown_preview.next_generation.wrapping_add(1);
        if let Some(tab) = &mut self.view.markdown_preview {
            tab.preview.scope.generation = self.markdown_preview.next_generation;
            tab.preview.error = None;
            tab.preview.loading = true;
        }
    }

    fn fail_markdown_preview(&mut self, error: String) {
        self.invalidate_markdown_preview();
        self.markdown_preview.due = None;
        if let Some(tab) = &mut self.view.markdown_preview {
            tab.preview.loading = false;
            tab.preview.error = Some(error);
            tab.preview.document = None;
            tab.preview.raster = None;
        }
    }

    fn submit_markdown_preview(&mut self, source: Source) {
        let source = match source {
            Source::Disk => None,
            Source::Loading => {
                self.markdown_preview.due = None;
                return;
            }
            Source::Error(error) => {
                self.fail_markdown_preview(error);
                return;
            }
            Source::Buffer(snapshot) => {
                if snapshot.byte_count() > openide_preview::markdown::MAX_SOURCE_BYTES {
                    self.fail_markdown_preview("El Markdown supera los 2 MiB admitidos por la vista previa. Podés seguir editando el archivo completo.".into());
                    return;
                }
                match snapshot.text_range(0..snapshot.char_count()) {
                    Ok(source) => Some(source),
                    Err(error) => {
                        self.fail_markdown_preview(error.to_string());
                        return;
                    }
                }
            }
        };
        if self.markdown_preview.worker.is_none() {
            match Worker::new() {
                Ok(worker) => self.markdown_preview.worker = Some(worker),
                Err(error) => {
                    self.fail_markdown_preview(error.to_string());
                    return;
                }
            }
        }
        let Some(tab) = &self.view.markdown_preview else {
            return;
        };
        let request = Request {
            scope: tab.preview.scope.clone(),
            source,
            page: 0,
            render_width: 1440,
        };
        self.markdown_preview.due = None;
        if let Err(error) = self
            .markdown_preview
            .worker
            .as_ref()
            .unwrap()
            .submit(request)
        {
            self.fail_markdown_preview(error);
        }
    }

    pub(super) fn poll_markdown_preview(&mut self, ctx: &egui::Context) -> bool {
        let Some(tab) = &self.view.markdown_preview else {
            return false;
        };
        if self.markdown_preview.owner.as_ref() != Some(&self.dock_scope())
            || tab.preview.scope.workspace != Path::new(&self.view.workspace)
        {
            self.close_markdown_preview(false);
            return true;
        }
        let source = self.markdown_preview_source();
        let identity = source.identity();
        let mut changed = false;
        if self.markdown_preview.observed.as_ref() != Some(&identity) {
            let debounce = matches!(
                self.markdown_preview.observed,
                Some(SourceIdentity::Buffer(..))
            ) && matches!(identity, SourceIdentity::Buffer(..));
            self.markdown_preview.observed = Some(identity);
            // Invalidate before debounce or validation so an older render cannot
            // overwrite the current buffer's error or consume stale link actions.
            self.invalidate_markdown_preview();
            self.markdown_preview.due = match &source {
                Source::Loading => None,
                _ => Some(
                    Instant::now()
                        + if debounce {
                            Duration::from_millis(200)
                        } else {
                            Duration::ZERO
                        },
                ),
            };
            changed = true;
        }
        if self
            .markdown_preview
            .due
            .is_some_and(|due| Instant::now() >= due)
        {
            self.submit_markdown_preview(source);
            changed = true;
        }
        while let Some(response) = self
            .markdown_preview
            .worker
            .as_ref()
            .and_then(Worker::try_recv)
        {
            let Some(tab) = self.view.markdown_preview.as_mut() else {
                continue;
            };
            if response.scope != tab.preview.scope {
                continue;
            }
            tab.preview.loading = false;
            match response.result {
                Ok(Document::Markdown(document)) => {
                    tab.preview.document = Some(Arc::new(Document::Markdown(document)));
                    tab.preview.error = None;
                }
                Ok(_) => {
                    tab.preview.document = None;
                    tab.preview.error = Some("El archivo no es un documento Markdown.".into());
                }
                Err(error) => {
                    tab.preview.document = None;
                    tab.preview.error = Some(error);
                }
            }
            changed = true;
        }
        if self.markdown_preview.due.is_some()
            || self
                .view
                .markdown_preview
                .as_ref()
                .is_some_and(|tab| tab.preview.loading)
        {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        changed
    }

    pub(super) fn apply_markdown_preview_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if let Some(source_tab) = id.strip_prefix("markdown_preview_open:") {
            let active = self.dock.active_tab(&self.dock_scope());
            if self.view.main_page == MainPage::Workspace
                && !self.view.show_settings
                && !self.view.ide.extensions_open
                && self.resource_pane_visible()
                && active.is_some_and(|tab| {
                    source_tab.parse::<u64>().ok() == Some(tab.id.get())
                        && matches!(&tab.kind, DockTabKind::FilePreview { path }
                            if openide_preview::kind_for_path(path) == Some(PreviewKind::Markdown))
                })
            {
                self.open_markdown_preview(ctx);
            }
            return true;
        }
        let Some(rest) = id.strip_prefix("markdown_preview:") else {
            return false;
        };
        let Some((id, operation)) = rest.split_once(':') else {
            return true;
        };
        let Some(tab) = self.view.markdown_preview.as_ref() else {
            return true;
        };
        if id.parse::<u64>().ok() != Some(tab.id)
            || self.markdown_preview.owner.as_ref() != Some(&self.dock_scope())
            || tab.preview.scope.workspace != Path::new(&self.view.workspace)
        {
            return true;
        }
        match operation {
            "close" => self.close_markdown_preview(true),
            "tab" => {
                self.markdown_preview.focus_pending = Some(self.tree.has_keyboard_focus());
            }
            "source" => {
                let path = tab.preview.scope.path.clone();
                let source = self.open_markdown_link_source(path);
                if let Some(tab) = &mut self.view.markdown_preview {
                    tab.source_tab_id = source;
                }
                self.markdown_preview.focus_pending = None;
                self.focus_visible_markdown_source();
            }
            "reload" | "refresh" => {
                self.invalidate_markdown_preview();
                self.markdown_preview.due = Some(Instant::now());
            }
            _ => {
                if let Some((generation, operation)) = operation
                    .strip_prefix("document:")
                    .and_then(|operation| operation.split_once(':'))
                {
                    if generation.parse::<u64>().ok() == Some(tab.preview.scope.generation)
                        && !tab.preview.loading
                        && tab.preview.error.is_none()
                    {
                        if let Some(index) = operation
                            .strip_prefix("link:")
                            .and_then(|index| index.parse::<usize>().ok())
                        {
                            let destination = tab.preview.document.as_deref().and_then(
                                |document| match document {
                                    Document::Markdown(document) => document
                                        .links
                                        .get(index)
                                        .map(|link| link.destination.clone()),
                                    _ => None,
                                },
                            );
                            if let Some(destination) = destination {
                                self.open_markdown_preview_link(&destination, ctx);
                            }
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
        true
    }

    fn open_markdown_link_source(&mut self, path: PathBuf) -> u64 {
        let scope = self.dock_scope();
        let tab = self.dock.open(&scope, DockTabKind::FilePreview { path });
        self.dock.activate(&scope, tab);
        self.dock.set_collapsed(&scope, false);
        self.view.ide.extensions_open = false;
        self.view.ide.compact_panel = None;
        self.sync_dock_view();
        tab.get()
    }

    fn open_markdown_preview_link(&mut self, destination: &str, ctx: &egui::Context) {
        if let Ok(url) = reqwest::Url::parse(destination) {
            if matches!(url.scheme(), "http" | "https") {
                ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
            } else {
                self.view.status =
                    "La vista previa admite enlaces web HTTP/HTTPS y archivos del proyecto.".into();
            }
            return;
        }
        let Some(tab) = &self.view.markdown_preview else {
            return;
        };
        let Some(path) =
            super::document_preview_app::local_link_path(&tab.preview.scope.path, destination)
        else {
            self.view.status = "El enlace debe apuntar a un archivo dentro del proyecto.".into();
            return;
        };
        let anchor = destination
            .split_once('#')
            .and_then(|(_, fragment)| super::document_preview_app::decode_fragment(fragment));
        if path == tab.preview.scope.path {
            if let Some(anchor) = anchor {
                self.markdown_preview.pending_anchor = Some((tab.id, anchor));
            } else {
                self.tree.scroll_to_start(&tab.scroll_id());
            }
            return;
        }
        if openide_preview::kind_for_path(&path) == Some(PreviewKind::Markdown) {
            let source_tab_id = self
                .code_editor
                .view_for_path(&tab.preview.scope.workspace, &path)
                .map_or(tab.source_tab_id, |source| source.tab_id);
            self.replace_markdown_preview(self.dock_scope(), path, source_tab_id);
            if let Some(anchor) = anchor {
                self.markdown_preview.pending_anchor =
                    Some((self.view.markdown_preview.as_ref().unwrap().id, anchor));
            }
        } else {
            self.open_markdown_link_source(path);
            self.focus_visible_markdown_source();
        }
    }

    pub(super) fn focus_markdown_preview_anchor(&mut self) -> bool {
        let Some(tab) = &self.view.markdown_preview else {
            return false;
        };
        let mut changed = false;
        if let Some(keyboard) = self.markdown_preview.focus_pending {
            let id = format!("{}:tab", tab.action_prefix());
            let focused = if keyboard {
                self.tree.focus(&id)
            } else {
                self.tree.focus_from_pointer(&id)
            };
            if focused {
                self.markdown_preview.focus_pending = None;
                changed = true;
            }
        }
        let Some((id, anchor)) = self.markdown_preview.pending_anchor.clone() else {
            return changed;
        };
        if id != tab.id || tab.preview.error.is_some() {
            self.markdown_preview.pending_anchor = None;
            return changed;
        }
        if tab.preview.loading || tab.preview.document.is_none() {
            return changed;
        }
        let id = crate::view::document_markdown_ui::heading_action(
            &tab.document_action_prefix(),
            &anchor,
        );
        let found = self.tree.focus(&id);
        self.markdown_preview.pending_anchor = None;
        if !found {
            self.view.status =
                format!("No se encontró el encabezado «{anchor}» en la vista previa.");
        }
        changed || found
    }
}

#[cfg(test)]
#[path = "markdown_preview_app_tests.rs"]
mod tests;
