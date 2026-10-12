//! Scoped asynchronous document previews. The editable Markdown buffer remains
//! owned by CodeEditor; previewing never saves, reloads or reformats that buffer.
use super::*;
use crate::document_preview::{PreviewMode, PreviewView};
use openide_preview::{Document, PreviewKind, Request, Scope, Worker};

type PreviewKey = (PathBuf, PathBuf);

#[derive(Default)]
pub(super) struct PreviewHost {
    worker: Option<Worker>,
    next_generation: u64,
    modes: std::collections::BTreeMap<PreviewKey, PreviewMode>,
    positions: std::collections::BTreeMap<PreviewKey, (usize, Option<f32>)>,
    observed: Option<(u64, u64)>,
    due: Option<Instant>,
    pending_anchor: Option<(PreviewKey, String)>,
}

impl AgentsWindow {
    pub(super) fn sync_document_preview(&mut self) {
        let active = self.dock.active_tab(&self.dock_scope()).cloned();
        let selected = active.and_then(|tab| match tab.kind {
            DockTabKind::FilePreview { path } => {
                openide_preview::kind_for_path(&path).map(|kind| (tab.id.get(), path, kind))
            }
            _ => None,
        });
        let Some((tab, path, kind)) = selected else {
            if self.view.document_preview.is_some() {
                self.remember_document_preview_position();
                if let Some(worker) = &self.document_preview.worker {
                    worker.cancel();
                }
            }
            self.view.document_preview = None;
            self.document_preview.observed = None;
            self.document_preview.due = None;
            self.document_preview.pending_anchor = None;
            return;
        };
        let workspace = PathBuf::from(&self.view.workspace);
        if self.view.document_preview.as_ref().is_some_and(|view| {
            view.scope.tab_id == tab && view.scope.workspace == workspace && view.scope.path == path
        }) {
            return;
        }
        self.remember_document_preview_position();
        let (page, zoom) = self
            .document_preview
            .positions
            .get(&(workspace.clone(), path.clone()))
            .copied()
            .unwrap_or_default();
        let mode = if kind == PreviewKind::Markdown {
            self.document_preview
                .modes
                .get(&(workspace.clone(), path.clone()))
                .copied()
                .unwrap_or_default()
        } else {
            PreviewMode::Preview
        };
        if let Some(worker) = &self.document_preview.worker {
            worker.cancel();
        }
        if self
            .document_preview
            .pending_anchor
            .as_ref()
            .is_some_and(|(key, _)| key != &(workspace.clone(), path.clone()))
        {
            self.document_preview.pending_anchor = None;
        }
        self.document_preview.next_generation =
            self.document_preview.next_generation.wrapping_add(1);
        self.view.document_preview = Some(PreviewView {
            scope: Scope {
                workspace,
                path,
                tab_id: tab,
                generation: self.document_preview.next_generation,
            },
            kind,
            mode,
            document: None,
            raster: None,
            loading: false,
            error: None,
            page,
            pages: 0,
            zoom,
        });
        self.document_preview.observed = None;
        self.document_preview.due = (mode != PreviewMode::Source).then(Instant::now);
    }

    fn remember_document_preview_position(&mut self) {
        let Some(view) = &self.view.document_preview else {
            return;
        };
        let key = (view.scope.workspace.clone(), view.scope.path.clone());
        if self.document_preview.positions.len() >= 32
            && !self.document_preview.positions.contains_key(&key)
        {
            self.document_preview.positions.pop_first();
        }
        self.document_preview
            .positions
            .insert(key, (view.page, view.zoom));
    }

    /// The remembered mode and position follow a file that moved to `new`,
    /// and go with one that is gone.
    pub(super) fn retarget_document_previews(&mut self, old: &Path, new: Option<&Path>) {
        // The preview on screen is remembered under the path it still has.
        self.remember_document_preview_position();
        let workspace = PathBuf::from(&self.view.workspace);
        let moved = |key: PreviewKey| {
            if key.0 != workspace {
                return Some(key);
            }
            match crate::file_ops::moved(&key.1, old, new) {
                Some(path) => path.map(|path| (key.0, path)),
                None => Some(key),
            }
        };
        let host = &mut self.document_preview;
        host.modes = std::mem::take(&mut host.modes)
            .into_iter()
            .filter_map(|(key, mode)| Some((moved(key)?, mode)))
            .collect();
        host.positions = std::mem::take(&mut host.positions)
            .into_iter()
            .filter_map(|(key, position)| Some((moved(key)?, position)))
            .collect();
    }

    fn submit_document_preview(&mut self) {
        let Some(view) = self.view.document_preview.as_ref() else {
            return;
        };
        let source = if view.kind == PreviewKind::Markdown {
            let Some(editor) = self
                .view
                .code_editor
                .as_ref()
                .filter(|editor| editor.tab_id == view.scope.tab_id)
            else {
                self.fail_document_preview("No se pudo cargar el buffer de Markdown. Volvé a Código para revisar el archivo.".into());
                return;
            };
            let Some(snapshot) = editor
                .controller
                .as_ref()
                .and_then(|controller| controller.snapshot().ok())
            else {
                if !editor.loading {
                    let error = editor.error.clone().unwrap_or_else(|| "No se pudo cargar el buffer de Markdown. Volvé a Código para revisar el archivo.".into());
                    self.fail_document_preview(error);
                }
                return;
            };
            if snapshot.byte_count() > openide_preview::markdown::MAX_SOURCE_BYTES {
                self.fail_document_preview("El Markdown supera los 2 MiB admitidos por la vista previa. Podés seguir editando el archivo completo.".into());
                return;
            }
            match snapshot.text_range(0..snapshot.char_count()) {
                Ok(source) => Some(source),
                Err(error) => {
                    self.fail_document_preview(error.to_string());
                    return;
                }
            }
        } else {
            None
        };
        if self.document_preview.worker.is_none() {
            match Worker::new() {
                Ok(worker) => self.document_preview.worker = Some(worker),
                Err(error) => {
                    self.fail_document_preview(error.to_string());
                    return;
                }
            }
        }
        self.document_preview.next_generation =
            self.document_preview.next_generation.wrapping_add(1);
        let view = self.view.document_preview.as_mut().unwrap();
        view.scope.generation = self.document_preview.next_generation;
        view.loading = true;
        view.error = None;
        let request = Request {
            scope: view.scope.clone(),
            source,
            page: view.page,
            render_width: 1440,
        };
        if let Err(error) = self
            .document_preview
            .worker
            .as_ref()
            .unwrap()
            .submit(request)
        {
            view.error = Some(error.to_string());
            view.loading = false;
        }
        self.document_preview.due = None;
    }

    fn fail_document_preview(&mut self, error: String) {
        if let Some(worker) = &self.document_preview.worker {
            worker.cancel();
        }
        self.document_preview.next_generation =
            self.document_preview.next_generation.wrapping_add(1);
        if let Some(view) = &mut self.view.document_preview {
            view.scope.generation = self.document_preview.next_generation;
            view.error = Some(error);
            view.loading = false;
            view.document = None;
            view.raster = None;
        }
        self.document_preview.due = None;
        self.document_preview.pending_anchor = None;
    }

    pub(super) fn reveal_document_preview_source(&mut self) {
        if self
            .view
            .document_preview
            .as_ref()
            .is_some_and(|view| view.kind == PreviewKind::Markdown && !view.is_source_visible())
        {
            self.set_document_preview_mode(PreviewMode::Source);
        }
    }

    pub(super) fn poll_document_preview(&mut self, ctx: &egui::Context) -> bool {
        let before = self
            .view
            .document_preview
            .as_ref()
            .map(|view| view.scope.clone());
        self.sync_document_preview();
        let mut changed = before
            != self
                .view
                .document_preview
                .as_ref()
                .map(|view| view.scope.clone());
        let Some(view) = self.view.document_preview.as_ref() else {
            return changed;
        };
        if view.kind == PreviewKind::Markdown && view.mode != PreviewMode::Source {
            if let Some(snapshot) = self
                .view
                .code_editor
                .as_ref()
                .filter(|editor| editor.tab_id == view.scope.tab_id)
                .and_then(|editor| editor.controller.as_ref())
                .and_then(|controller| controller.snapshot().ok())
            {
                let identity = (snapshot.document_id(), snapshot.revision());
                if self.document_preview.observed != Some(identity) {
                    let first = self.document_preview.observed.is_none();
                    self.document_preview.observed = Some(identity);
                    self.document_preview.due = Some(
                        Instant::now()
                            + if first {
                                Duration::ZERO
                            } else {
                                Duration::from_millis(200)
                            },
                    );
                }
            }
        }
        if self
            .document_preview
            .due
            .is_some_and(|due| Instant::now() >= due)
        {
            self.submit_document_preview();
            changed = true;
        }
        while let Some(response) = self
            .document_preview
            .worker
            .as_ref()
            .and_then(Worker::try_recv)
        {
            let Some(view) = self.view.document_preview.as_mut() else {
                continue;
            };
            if response.scope != view.scope
                || response.scope.workspace != Path::new(&self.view.workspace)
            {
                continue;
            }
            view.loading = false;
            match response.result {
                Ok(document) => {
                    view.error = None;
                    view.raster = match &document {
                        Document::Image(image) | Document::Pdf { image, .. } => {
                            crate::view::document_markdown_ui::image_to_raster(
                                image,
                                "Vista previa del documento",
                            )
                        }
                        Document::Markdown(_) => None,
                    };
                    if let Document::Pdf { page, pages, .. } = &document {
                        view.page = *page;
                        view.pages = *pages;
                    }
                    view.document = Some(Arc::new(document));
                }
                Err(error) => {
                    view.error = Some(error);
                    view.document = None;
                    view.raster = None;
                }
            }
            changed = true;
        }
        if self.document_preview.due.is_some()
            || self
                .view
                .document_preview
                .as_ref()
                .is_some_and(|view| view.loading)
        {
            ctx.request_repaint_after(Duration::from_millis(25));
        }
        changed
    }

    fn set_document_preview_mode(&mut self, mode: PreviewMode) {
        let Some(view) = self.view.document_preview.as_mut() else {
            return;
        };
        if view.kind != PreviewKind::Markdown {
            return;
        }
        view.mode = mode;
        let key = (view.scope.workspace.clone(), view.scope.path.clone());
        if self.document_preview.modes.len() >= 32
            && !self.document_preview.modes.contains_key(&key)
        {
            self.document_preview.modes.pop_first();
        }
        self.document_preview.modes.insert(key, mode);
        self.view.editor_workbench.search_open = false;
        self.view.editor_workbench.menu_open = false;
        self.view.editor_workbench.form = None;
        self.editor_services.focus_next = None;
        if mode == PreviewMode::Source {
            if let Some(worker) = &self.document_preview.worker {
                worker.cancel();
            }
            view.loading = false;
            self.document_preview.due = None;
            self.document_preview.pending_anchor = None;
            self.editor_services.focus_next = self
                .view
                .code_editor
                .as_ref()
                .filter(|editor| editor.tab_id == view.scope.tab_id && editor.controller.is_some())
                .map(|editor| crate::code_editor_ui::view_id(editor.tab_id));
        } else {
            self.document_preview.due = Some(Instant::now());
            self.document_preview.observed = None;
        }
    }

    pub(super) fn apply_document_preview_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let (tab, generation, operation) =
            if let Some(rest) = id.strip_prefix("document_preview_view:") {
                let Some((tab, operation)) = rest.split_once(':') else {
                    return true;
                };
                // Stable chrome never dispatches render-indexed links.
                if operation.starts_with("link:") {
                    return true;
                }
                (tab.parse::<u64>().ok(), None, operation)
            } else if let Some(rest) = id.strip_prefix("document_preview:") {
                let mut parts = rest.splitn(3, ':');
                let tab = parts.next().and_then(|part| part.parse::<u64>().ok());
                let generation = parts.next().and_then(|part| part.parse::<u64>().ok());
                let Some(operation) = parts.next() else {
                    return true;
                };
                let Some(generation) = generation else {
                    return true;
                };
                (tab, Some(generation), operation)
            } else {
                return false;
            };
        let Some(view) = self.view.document_preview.as_ref() else {
            return true;
        };
        if tab != Some(view.scope.tab_id)
            || generation.is_some_and(|generation| generation != view.scope.generation)
            || view.scope.workspace != Path::new(&self.view.workspace)
            || self
                .dock
                .active_tab(&self.dock_scope())
                .map(|tab| tab.id.get())
                != tab
        {
            return true;
        }
        match operation {
            "source" => self.set_document_preview_mode(PreviewMode::Source),
            "preview" => self.set_document_preview_mode(PreviewMode::Preview),
            "split" => self.set_document_preview_mode(PreviewMode::Split),
            "reload" => self.document_preview.due = Some(Instant::now()),
            // The picture the panel already decoded, in the window's viewer:
            // nothing is read again, and an SVG is the raster it was drawn as.
            "fullscreen" => {
                if let (PreviewKind::Image, Some(image)) = (view.kind, view.raster.clone()) {
                    let path = view.scope.path.to_string_lossy().into_owned();
                    let label = view
                        .scope
                        .path
                        .file_name()
                        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
                    self.close_composer_floating_controls();
                    // A response still on its way to an earlier viewer is not this one's.
                    self.attachment_viewer_generation =
                        self.attachment_viewer_generation.wrapping_add(1);
                    self.view.composer_image_viewer =
                        Some(crate::composer_attachments::ImageGalleryView {
                            workspace: self.view.workspace.clone(),
                            attachments: vec![crate::view::ComposerAttachmentView {
                                path,
                                label,
                                is_dir: false,
                                selection: None,
                            }],
                            selected: 0,
                            generation: self.attachment_viewer_generation,
                            image: Some(image),
                            error: None,
                            loading: false,
                            tool_source: None,
                        });
                    self.prompt_focus_pending = false;
                    self.dock_drag = None;
                }
            }
            // An image only fits its panel: these belong to a PDF's page.
            "fit" | "zoom_in" | "zoom_out" if view.kind == PreviewKind::Pdf => {
                let base = view.zoom.unwrap_or_else(|| {
                    let pixels = match view.document.as_deref() {
                        Some(Document::Image(image) | Document::Pdf { image, .. }) => image.width,
                        _ => 1,
                    };
                    self.tree
                        .frame()
                        .commands
                        .iter()
                        .find_map(|command| match command {
                            openide_ui::DrawCommand::RasterImage { rect, image, .. }
                                if view
                                    .raster
                                    .as_ref()
                                    .is_some_and(|raster| raster.sha256() == image.sha256()) =>
                            {
                                Some(rect.width / pixels.max(1) as f32)
                            }
                            _ => None,
                        })
                        .unwrap_or(1.0)
                });
                let view = self.view.document_preview.as_mut().unwrap();
                view.zoom = match operation {
                    "fit" => None,
                    "zoom_in" => Some((base * 1.25).min(8.0_f32.max(base))),
                    _ => Some((base / 1.25).max(0.01_f32.min(base))),
                };
            }
            "previous" | "next" => {
                let view = self.view.document_preview.as_mut().unwrap();
                if view.kind == PreviewKind::Pdf && !view.loading {
                    let page = if operation == "next" {
                        (view.page + 1).min(view.pages.saturating_sub(1))
                    } else {
                        view.page.saturating_sub(1)
                    };
                    if page != view.page {
                        view.page = page;
                        view.document = None;
                        view.raster = None;
                        self.tree.scroll_to_start(&view.scroll_id());
                        self.document_preview.due = Some(Instant::now());
                    }
                }
            }
            _ => {
                if let Some(index) = operation
                    .strip_prefix("link:")
                    .and_then(|index| index.parse::<usize>().ok())
                {
                    let destination =
                        view.document
                            .as_deref()
                            .and_then(|document| match document {
                                Document::Markdown(document) => document
                                    .links
                                    .get(index)
                                    .map(|link| link.destination.clone()),
                                _ => None,
                            });
                    if let Some(destination) = destination {
                        self.open_document_preview_link(&destination, ctx);
                    }
                }
            }
        }
        ctx.request_repaint();
        true
    }

    fn open_document_preview_link(&mut self, destination: &str, ctx: &egui::Context) {
        if let Ok(url) = reqwest::Url::parse(destination) {
            if matches!(url.scheme(), "http" | "https") {
                ctx.open_url(egui::OpenUrl::new_tab(url.to_string()));
            } else {
                self.view.status =
                    "La vista previa admite enlaces web HTTP/HTTPS y archivos del proyecto.".into();
            }
            return;
        }
        let Some(view) = self.view.document_preview.as_ref() else {
            return;
        };
        let Some(path) = local_link_path(&view.scope.path, destination) else {
            self.view.status = "El enlace debe apuntar a un archivo dentro del proyecto.".into();
            return;
        };
        let anchor = destination
            .split_once('#')
            .and_then(|(_, fragment)| decode_fragment(fragment));
        let markdown = openide_preview::kind_for_path(&path) == Some(PreviewKind::Markdown);
        if markdown {
            self.document_preview.pending_anchor =
                anchor.map(|anchor| ((PathBuf::from(&self.view.workspace), path.clone()), anchor));
        }
        if path == view.scope.path && destination.starts_with('#') {
            if self.document_preview.pending_anchor.is_none() {
                self.tree.scroll_to_start(&view.scroll_id());
            }
            return;
        }
        if markdown {
            if self.document_preview.modes.len() >= 32 {
                self.document_preview.modes.pop_first();
            }
            self.document_preview.modes.insert(
                (PathBuf::from(&self.view.workspace), path.clone()),
                PreviewMode::Preview,
            );
        }
        let scope = self.dock_scope();
        self.dock.open(&scope, DockTabKind::FilePreview { path });
        self.dock.set_collapsed(&scope, false);
        self.view.ide.extensions_open = false;
        self.sync_dock_view();
    }

    pub(super) fn focus_document_preview_anchor(&mut self) -> bool {
        let Some((key, anchor)) = self.document_preview.pending_anchor.clone() else {
            return false;
        };
        let Some(view) = self.view.document_preview.as_ref() else {
            return false;
        };
        if key != (view.scope.workspace.clone(), view.scope.path.clone()) {
            self.document_preview.pending_anchor = None;
            return false;
        }
        if view.error.is_some() {
            self.document_preview.pending_anchor = None;
            return false;
        }
        if view.loading || view.document.is_none() {
            return false;
        }
        let id = crate::view::document_markdown_ui::heading_action(&view.action_prefix(), &anchor);
        let found = self.tree.focus(&id);
        self.document_preview.pending_anchor = None;
        if !found {
            self.view.status =
                format!("No se encontró el encabezado «{anchor}» en la vista previa.");
        }
        found
    }
}

pub(super) fn decode_fragment(fragment: &str) -> Option<String> {
    if fragment.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(fragment.len());
    let mut input = fragment.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

pub(super) fn local_link_path(document: &Path, destination: &str) -> Option<PathBuf> {
    let raw = destination.split(['#', '?']).next()?;
    if raw.is_empty() {
        return Some(document.to_path_buf());
    }
    let mut bytes = Vec::with_capacity(raw.len());
    let mut encoded = raw.bytes();
    while let Some(byte) = encoded.next() {
        if byte == b'%' {
            let high = (encoded.next()? as char).to_digit(16)?;
            let low = (encoded.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    if decoded.contains(['\0', '\\', ':']) {
        return None;
    }
    let path = Path::new(&decoded);
    if path.is_absolute() {
        return None;
    }
    let mut result = document.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => result.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (!result.as_os_str().is_empty()).then_some(result)
}

#[cfg(test)]
#[path = "document_preview_app_tests.rs"]
mod tests;
