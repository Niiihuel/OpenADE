//! The graph tab in the window: the canvas's pointer events, its controls'
//! actions and its drawing in the frame.

use super::*;

impl AgentsWindow {
    /// Keeps the focused node on the file last shown in the dock, and lays the
    /// window out again when the graph's status changed while painting.
    pub(super) fn update_graph(&mut self) -> bool {
        if let Some(preview) = &self.view.file_preview {
            if self.graph_last_file.as_deref() != Some(preview.path.as_str()) {
                self.graph_last_file = Some(preview.path.clone());
            }
        }
        std::mem::take(&mut self.graph_relayout)
    }

    /// Events on the canvas, and the rest of a press that began there. Some
    /// when the graph took the event, true if the window must be laid out.
    pub(super) fn handle_graph_event(&mut self, event: &UiEvent, ctx: &egui::Context) -> Option<bool> {
        let frame = self.tree.frame();
        crate::graph_view::canvas_rect(&frame.commands)?;
        if matches!(event, UiEvent::Key { .. } | UiEvent::Text(_)) {
            let focused = self.tree.focused_id() == Some(crate::graph_view::CANVAS_ID);
            return (focused && self.graph.handle_key(event)).then(|| {
                ctx.request_repaint();
                false
            });
        }
        let position = match event {
            UiEvent::PointerMove(point) | UiEvent::PointerDown(point) | UiEvent::PointerUp(point) => Some(*point),
            UiEvent::PointerDownModified { position, .. }
            | UiEvent::PointerDownEditor { position, .. }
            | UiEvent::Scroll { position, .. }
            | UiEvent::ScrollDirect { position, .. }
            | UiEvent::Scroll2D { position, .. } => Some(*position),
            _ => None,
        };
        // The controls and any menu over the canvas keep their own events.
        let over = position.is_some_and(|point| frame.hit_test(point) == Some(crate::graph_view::CANVAS_ID));
        let outcome = self.graph.handle_event(event, &self.view.graph_options, over);
        if !outcome.consumed {
            return None;
        }
        if matches!(event, UiEvent::PointerDown(_) | UiEvent::PointerDownModified { .. } | UiEvent::PointerDownEditor { .. }) {
            self.tree.focus(crate::graph_view::CANVAS_ID);
        }
        ctx.request_repaint();
        let path = outcome.open?;
        let owner = self.view.selected_session.clone().unwrap_or_default();
        self.open_tool_workspace_file(&owner, &path);
        Some(true)
    }

    pub(super) fn apply_graph_change(&mut self, applied: crate::graph_ui::Applied, action: &UiAction, ctx: &egui::Context) {
        if applied.timelapse {
            self.graph.start_timelapse();
        }
        // A slider is saved when it is let go, not at every step of its drag.
        if !matches!(action, UiAction::SliderChanged { committed: false, .. }) {
            if let Err(error) = self.view.graph_options.save(&self.data_root) {
                self.view.status = error;
            }
        }
        ctx.request_repaint();
    }

    /// Draws the graph in place of its surface in `frame` while the tab shows
    /// it, keeping its index fresh and its layout running.
    pub(super) fn paint_graph(&mut self, ctx: &egui::Context, frame: &mut UiFrame) {
        let Some(rect) = crate::graph_view::canvas_rect(&frame.commands) else {
            return;
        };
        let wake = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        self.graph.keep_loaded(Path::new(&self.view.workspace), wake);
        if self.graph.poll(&self.view.graph_options, self.graph_last_file.as_deref()) {
            self.view.graph_status = self.graph.status.clone();
            self.graph_relayout = true;
            ctx.request_repaint();
        }
        self.graph.place(rect);
        if self.graph.animate() {
            ctx.request_repaint();
        }
        let measure = |text: &str, size: f32| {
            ctx.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(text.to_owned(), egui::FontId::proportional(size), egui::Color32::WHITE)
                    .size()
                    .x
            })
        };
        let commands = self.graph.paint(rect, &self.view.graph_options, &measure);
        crate::graph_view::splice(&mut frame.commands, commands);
    }
}
