use super::*;
use openide_ui::Key;

#[derive(Clone, Copy)]
pub(super) struct TabDrag {
    id: DockTabId,
    start: openide_ui::Point,
}

impl AgentsWindow {
    fn tab_at(&self, point: openide_ui::Point) -> Option<DockTabId> {
        // Overview rows are actions rather than draggable strip tabs. Floating
        // menus retain pointer ownership, including clicks over their backdrop.
        if self.view.show_dock_overview
            || self.view.show_dock_tabs_menu
            || self.view.show_dock_add_menu
        {
            return None;
        }
        let id = self.tree.frame().hit_test(point)?;
        id.strip_prefix("dock_tab:")
            .and_then(|raw| raw.parse().ok())
            .and_then(DockTabId::from_raw)
    }

    fn cycle_dock_tab(&mut self, backwards: bool) {
        let scope = self.dock_scope();
        let ids: Vec<_> = self
            .dock
            .right_tabs(&scope)
            .iter()
            .filter(|tab| {
                !self.ide_workspace_active()
                    || !matches!(tab.kind, DockTabKind::Files | DockTabKind::Context)
            })
            .map(|tab| tab.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let index = self
            .dock
            .active_tab(&scope)
            .and_then(|active| ids.iter().position(|id| *id == active.id))
            .unwrap_or(0);
        let next = if backwards {
            (index + ids.len() - 1) % ids.len()
        } else {
            (index + 1) % ids.len()
        };
        self.dock.activate(&scope, ids[next]);
        self.view.show_dock_overview = false;
        self.view.show_dock_add_menu = false;
        self.view.show_dock_tabs_menu = false;
        self.sync_dock_view();
    }

    pub(super) fn handle_dock_tab_event(&mut self, event: &UiEvent) -> bool {
        if !self.resource_pane_visible()
            || self.ide_modal_panel_visible()
            || self.view.show_settings
            || self.view.environment.open
            || self.view.show_command_center
            || self.view.composer_full_access_confirm.is_some()
            || self.view.main_page != MainPage::Workspace
        {
            self.dock_tab_drag = None;
            return false;
        }
        match event {
            UiEvent::Key {
                key: Key::Tab,
                ctrl: true,
                shift,
            } => {
                self.cycle_dock_tab(*shift);
                true
            }
            UiEvent::PointerDown(point) => {
                let Some(id) = self.tab_at(*point) else {
                    return false;
                };
                self.dock_tab_drag = Some(TabDrag { id, start: *point });
                let scope = self.dock_scope();
                self.dock.activate(&scope, id);
                self.view.show_dock_overview = false;
                self.sync_dock_view();
                self.tree
                    .focus_from_pointer(&format!("dock_tab:{}", id.get()));
                true
            }
            UiEvent::PointerMove(point) => {
                let Some(drag) = self.dock_tab_drag else {
                    return false;
                };
                if (point.x - drag.start.x).abs() > 5.0 {
                    if let Some(over) = self.tab_at(*point) {
                        let scope = self.dock_scope();
                        if over != drag.id && self.dock.reorder(&scope, drag.id, over) {
                            self.sync_dock_view();
                        }
                    }
                }
                true
            }
            UiEvent::PointerUp(_) => self.dock_tab_drag.take().is_some(),
            UiEvent::PointerLeave => {
                self.dock_tab_drag = None;
                false
            }
            _ => false,
        }
    }

    pub(super) fn handle_dock_middle_click(
        &mut self,
        ctx: &egui::Context,
        origin: egui::Pos2,
    ) -> bool {
        if !self.resource_pane_visible()
            || self.ide_modal_panel_visible()
            || self.view.show_settings
            || self.view.show_command_center
            || self.view.environment.open
            || self.view.composer_full_access_confirm.is_some()
            || self.view.main_page != MainPage::Workspace
        {
            return false;
        }
        let points: Vec<_> = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Middle,
                        pressed: false,
                        ..
                    } => Some(openide_ui::Point::new(pos.x - origin.x, pos.y - origin.y)),
                    _ => None,
                })
                .collect()
        });
        let mut changed = false;
        for point in points {
            let preview_close = self.view.markdown_preview.as_ref().and_then(|tab| {
                let prefix = tab.action_prefix();
                self.tree.frame().hit_test(point).and_then(|id| {
                    matches!(id.strip_prefix(&prefix), Some(":tab" | ":close"))
                        .then(|| format!("{prefix}:close"))
                })
            });
            if let Some(id) = preview_close {
                self.apply_action(UiAction::Clicked { id }, ctx);
                changed = true;
                continue;
            }
            if let Some(id) = self.tab_at(point).filter(|id| {
                self.view
                    .dock_tabs
                    .iter()
                    .any(|tab| tab.id == id.get() && tab.closable)
            }) {
                self.apply_action(
                    UiAction::Clicked {
                        id: format!("dock_close:{}", id.get()),
                    },
                    ctx,
                );
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_a_draggable_tab_keeps_pointer_focus_without_a_keyboard_ring() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.show_side_pane = true;
        let id = app.view.dock_tabs.first().expect("initial tab").id;
        let action = format!("dock_tab:{id}");
        let size = openide_ui::Size::new(120.0, 40.0);
        app.tree.set_root(
            openide_ui::Element::button(&action, "Archivo")
                .size(Some(size.width), Some(size.height)),
        );
        let initial = app.tree.layout(size);
        let strokes = |frame: &openide_ui::Frame| {
            frame
                .commands
                .iter()
                .filter(|command| matches!(command, openide_ui::DrawCommand::StrokeRect { .. }))
                .count()
        };
        assert!(
            app.handle_dock_tab_event(&UiEvent::PointerDown(openide_ui::Point::new(30.0, 15.0)))
        );
        assert_eq!(app.tree.focused_id(), Some(action.as_str()));
        assert_eq!(strokes(&app.tree.layout(size)), strokes(&initial));
        assert!(app.handle_dock_tab_event(&UiEvent::PointerUp(openide_ui::Point::new(30.0, 15.0))));
        assert!(app.tree.focus(&action));
        assert_eq!(strokes(&app.tree.layout(size)), strokes(&initial) + 1);
    }

    #[test]
    fn menu_and_overview_rows_cannot_start_tab_dragging() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_path_buf());
        app.view.show_side_pane = true;
        let id = app.view.dock_tabs.first().expect("initial tab").id;
        app.tree.set_root(
            openide_ui::Element::button(format!("dock_tab:{id}"), "Tab")
                .size(Some(100.0), Some(34.0)),
        );
        app.tree.layout(openide_ui::Size::new(100.0, 34.0));
        let point = openide_ui::Point::new(30.0, 15.0);
        assert_eq!(app.tab_at(point).map(|tab| tab.get()), Some(id));
        app.view.show_dock_tabs_menu = true;
        assert!(!app.handle_dock_tab_event(&UiEvent::PointerDown(point)));
        assert!(app.dock_tab_drag.is_none());
        app.view.show_dock_tabs_menu = false;
        app.view.show_dock_overview = true;
        assert!(!app.handle_dock_tab_event(&UiEvent::PointerDown(point)));
        assert!(app.dock_tab_drag.is_none());
        app.view.show_dock_overview = false;
        app.view.show_dock_add_menu = true;
        assert!(!app.handle_dock_tab_event(&UiEvent::PointerDown(point)));
        assert!(app.dock_tab_drag.is_none());
    }
}
