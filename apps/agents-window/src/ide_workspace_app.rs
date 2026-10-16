//! IDE and Agents are two presentations of one workspace host. Switching the
//! shell never recreates documents, terminal processes, sessions or the harness.
use super::*;
use crate::ide_workspace::{
    IdeActivity, IdePanel, WindowMode, ACTIVITY_BAR_WIDTH, DIVIDER, OUTER_INSET, SIDEBAR_GAP,
    STATUSBAR_HEIGHT, TITLEBAR_HEIGHT,
};

impl AgentsWindow {
    pub(super) fn ide_workspace_active(&self) -> bool {
        self.view.ide.is_active()
            && !self.view.show_settings
            && self.view.main_page == MainPage::Workspace
    }

    /// Cursor-only input is retained inside CodeEditor and emits no document
    /// change action. Both document footers rebuild only when this key changes.
    pub(super) fn ide_editor_status_key(&self) -> Option<(u64, u64, openide_editor::Selection)> {
        if !self.document_status_visible() {
            return None;
        }
        let editor = self.view.code_editor.as_ref()?;
        if editor.loading
            || !self
                .view
                .dock_tabs
                .iter()
                .any(|tab| tab.active && tab.id == editor.tab_id)
        {
            return None;
        }
        let shared = editor.controller.as_ref()?.shared_buffer();
        let buffer = shared.lock().ok()?;
        if self.view.editor_workbench.document_id != Some(buffer.document_id()) {
            return None;
        }
        // Read three scalars from the existing buffer: no document snapshot,
        // file tree or language analysis is cloned for input.
        Some((buffer.document_id(), buffer.revision(), buffer.selection()))
    }

    pub(super) fn resource_pane_visible(&self) -> bool {
        self.view.show_side_pane || self.ide_workspace_active()
    }

    pub(super) fn ide_modal_panel_visible(&self) -> bool {
        if !self.ide_workspace_active() {
            return false;
        }
        let layout = crate::ide_workspace::nominal_layout(&self.view, self.viewport_width);
        let drawer = match self.view.ide.compact_panel {
            Some(IdePanel::Explorer) => layout.left <= 0.0,
            Some(IdePanel::Agent) => layout.right.is_none(),
            None => false,
        };
        drawer || self.view.ide.sessions_open || self.view.ide.menu_open.is_some()
    }

    pub(super) fn composer_popup_blocks_input(&self) -> bool {
        if self.ide_workspace_active()
            && self.view.ide.compact_panel == Some(IdePanel::Agent)
            && crate::ide_workspace::nominal_layout(&self.view, self.viewport_width)
                .right
                .is_none()
        {
            self.tree.has_open_popover_except("ide_panel_dismiss")
        } else {
            self.tree.has_open_popover()
        }
    }

    pub(super) fn persist_ide_workspace(&mut self) {
        if let Err(error) = self.view.ide.save(&self.data_root) {
            self.view.status = error;
        }
    }

    fn prefer_ide_document(&mut self) {
        let scope = self.dock_scope();
        if self
            .dock
            .active_tab(&scope)
            .is_some_and(|tab| !matches!(tab.kind, DockTabKind::Files | DockTabKind::Context))
        {
            return;
        }
        let document = self
            .dock
            .right_tabs(&scope)
            .into_iter()
            .rev()
            .find(|tab| matches!(tab.kind, DockTabKind::FilePreview { .. }))
            .map(|tab| tab.id);
        if let Some(id) = document {
            let collapsed = self.dock.is_collapsed(&scope);
            self.dock.activate(&scope, id);
            self.dock.set_collapsed(&scope, collapsed);
        }
    }

    pub(super) fn open_ide_explorer(&mut self) {
        self.view.ide.activity = IdeActivity::Files;
        self.view.ide.extensions_open = false;
        self.view.ide.explorer_open = true;
        self.view.show_dock_add_menu = false;
        self.view.show_dock_tabs_menu = false;
        self.refresh_file_rows();
        self.view.ide.compact_panel =
            (crate::ide_workspace::nominal_layout(&self.view, self.viewport_width).left <= 0.0)
                .then_some(IdePanel::Explorer);
        self.persist_ide_workspace();
    }

    pub(super) fn toggle_ide_panel(&mut self, panel: IdePanel) {
        if panel == IdePanel::Agent {
            self.close_environment();
        }
        // Toggle the requested state, even while the preceding presentation
        // is still closing or has not acquired visible width yet.
        let layout = crate::ide_workspace::nominal_layout(&self.view, self.viewport_width);
        let visible = match panel {
            IdePanel::Explorer => layout.left > 0.0,
            IdePanel::Agent => layout.right.is_some(),
        };
        if visible {
            match panel {
                IdePanel::Explorer => self.view.ide.explorer_open = false,
                IdePanel::Agent => self.view.ide.agent_open = false,
            }
            self.view.ide.compact_panel = None;
        } else {
            match panel {
                IdePanel::Explorer => self.view.ide.explorer_open = true,
                IdePanel::Agent => self.view.ide.agent_open = true,
            }
            let next = crate::ide_workspace::nominal_layout(&self.view, self.viewport_width);
            let fits = match panel {
                IdePanel::Explorer => next.left > 0.0,
                IdePanel::Agent => next.right.is_some(),
            };
            self.view.ide.compact_panel = if fits || self.view.ide.compact_panel == Some(panel) {
                None
            } else {
                Some(panel)
            };
        }
        self.view.ide.sessions_open = false;
        self.close_composer_floating_controls();
        if panel == IdePanel::Explorer
            && self.view.ide.explorer_open
            && self.view.ide.activity == IdeActivity::Files
        {
            self.refresh_file_rows();
        }
        self.persist_ide_workspace();
    }

    pub(super) fn reveal_ide_agent(&mut self) {
        if !self.view.ide.is_active() {
            return;
        }
        self.close_environment();
        self.view.ide.agent_open = true;
        self.view.ide.sessions_open = false;
        self.view.ide.compact_panel =
            if crate::ide_workspace::nominal_layout(&self.view, self.viewport_width)
                .right
                .is_none()
            {
                Some(IdePanel::Agent)
            } else {
                None
            };
    }

    pub(super) fn apply_ide_workspace_action(&mut self, action: &UiAction) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        // Session navigation keeps using the same host route and scope checks.
        if id.starts_with("session:")
            || matches!(
                id.as_str(),
                "new_session" | "new_session_sidebar" | "new_session_header"
            )
        {
            self.view.ide.sessions_open = false;
            if self.ide_workspace_active() {
                self.reveal_ide_agent();
            }
            return false;
        }
        if id == "files_search" && self.ide_workspace_active() {
            self.view.ide.compact_panel = None;
            self.view.ide.sessions_open = false;
            self.open_command_center();
            self.view.command_scope = Scope::Files;
            self.request_command_search();
            return true;
        }
        if id == "window_mode_toggle" {
            if self.view.automations.screen == AutomationScreen::Edit {
                self.close_automation_editor();
            }
            self.view.ide.menu_open = None;
            self.view.ide.new_file = None;
            self.view.ide.extensions_open = false;
            self.tree.dismiss_context_menu();
            self.close_composer_floating_controls();
            self.close_environment();
            if self.view.environment.workflow.panel
                == crate::environment_git_ui::EnvironmentGitPanel::Actions
            {
                // Project actions belong to Environment in the Agents shell;
                // the IDE Git sidebar must not inherit an empty Actions page.
                // Keep the separate action editor and command drafts intact.
                self.view.environment.workflow.panel =
                    crate::environment_git_ui::EnvironmentGitPanel::Overview;
            }
            self.close_session_menu();
            self.view.ide.sessions_open = false;
            self.view.ide.compact_panel = None;
            self.view.show_command_center = false;
            self.view.show_dock_add_menu = false;
            self.view.show_dock_tabs_menu = false;
            self.view.editor_workbench.menu_open = false;
            self.view.ide.mode = match self.view.ide.mode {
                WindowMode::Agents => WindowMode::Ide,
                WindowMode::Ide => WindowMode::Agents,
            };
            self.view.show_settings = false;
            self.view.main_page = MainPage::Workspace;
            self.dock_drag = None;
            self.dock_tab_drag = None;
            self.dock_motion = None;
            self.environment_motion = None;
            self.view.environment_presentation_progress = None;
            self.view.left_presentation_width = None;
            self.view.right_presentation_width = None;
            self.view.ide.explorer_content_width = None;
            self.view.ide.agent_content_width = None;
            self.view.bottom_presentation_height = None;
            if self.view.ide.is_active() {
                self.prefer_ide_document();
                self.refresh_file_rows();
            }
            self.sync_dock_view();
            if self.view.ide.is_active() {
                self.prompt_focus_pending = false;
                if let Some(editor) = &self.view.code_editor {
                    self.editor_services.focus_next =
                        Some(crate::code_editor_ui::view_id(editor.tab_id));
                }
            }
            self.persist_ide_workspace();
            return true;
        }
        if let Some(task) = id.strip_prefix("ide_agent_tab_close:") {
            self.close_ide_agent_tab(task);
            return true;
        }
        if !matches!(
            id.as_str(),
            "ide_explorer_toggle"
                | "ide_agent_toggle"
                | "ide_quick_open"
                | "ide_sessions_toggle"
                | "ide_sessions_dismiss"
                | "ide_agent_new_toggle"
                | "ide_agent_new_dismiss"
                | "ide_agent_list_toggle"
                | "ide_open_files"
                | "ide_panel_dismiss"
        ) {
            return false;
        }
        if !self.ide_workspace_active() {
            return true;
        }
        self.view.ide.menu_open = None;
        match id.as_str() {
            "ide_explorer_toggle" => self.toggle_ide_panel(IdePanel::Explorer),
            "ide_agent_toggle" => self.toggle_ide_panel(IdePanel::Agent),
            "ide_quick_open" => {
                self.view.ide.sessions_open = false;
                self.view.ide.compact_panel = None;
                self.open_command_center();
                self.view.command_scope = Scope::Files;
                self.request_command_search();
            }
            "ide_sessions_toggle" => {
                self.view.ide.sessions_open = !self.view.ide.sessions_open;
            }
            "ide_sessions_dismiss" => self.view.ide.sessions_open = false,
            "ide_agent_new_toggle" => {
                self.view.ide.agent_new_open = !self.view.ide.agent_new_open;
            }
            "ide_agent_new_dismiss" => self.view.ide.agent_new_open = false,
            "ide_agent_list_toggle" => {
                self.view.ide.agent_list_open = !self.view.ide.agent_list_open;
                self.persist_ide_workspace();
            }
            "ide_panel_dismiss" => {
                self.view.ide.compact_panel = None;
                self.view.ide.sessions_open = false;
            }
            "ide_open_files" => self.open_ide_explorer(),
            _ => unreachable!(),
        }
        true
    }

    /// Keeps the agent panel's tabs to tasks that still exist and makes sure
    /// the task on screen has one.
    pub(super) fn follow_ide_agent_tabs(&mut self) -> bool {
        let before = self.view.ide.agent_tabs.clone();
        let sessions = &self.view.sessions;
        self.view
            .ide
            .agent_tabs
            .retain(|id| sessions.iter().any(|session| &session.id == id && !session.archived));
        if let Some(selected) = &self.view.selected_session {
            if !self.view.ide.agent_tabs.contains(selected)
                && sessions.iter().any(|session| &session.id == selected)
            {
                self.view.ide.agent_tabs.push(selected.clone());
            }
        }
        before != self.view.ide.agent_tabs
    }

    /// Closing a tab leaves the task in the list; the neighbouring tab, or a
    /// new task, takes its place when it was the one on screen.
    fn close_ide_agent_tab(&mut self, id: &str) {
        let Some(index) = self.view.ide.agent_tabs.iter().position(|tab| tab == id) else {
            return;
        };
        if self.view.selected_session.as_deref() != Some(id) {
            self.view.ide.agent_tabs.remove(index);
            return;
        }
        if self.view.running {
            self.view.status = "Detén el agente antes de cerrar su pestaña".to_owned();
            return;
        }
        self.view.ide.agent_tabs.remove(index);
        let neighbour = self
            .view
            .ide
            .agent_tabs
            .get(index.min(self.view.ide.agent_tabs.len().saturating_sub(1)))
            .cloned();
        match neighbour {
            Some(next) => self.open_session(&next),
            None => self.new_session(),
        }
    }

    pub(super) fn ide_dock_hit(
        &self,
        point: openide_ui::Point,
        layout: DockLayout,
    ) -> Option<DockDrag> {
        let bottom = self.viewport_height - OUTER_INSET - STATUSBAR_HEIGHT;
        if point.y < OUTER_INSET + TITLEBAR_HEIGHT || point.y > bottom {
            return None;
        }
        if layout
            .left_split_x
            .is_some_and(|x| (point.x - x).abs() <= 5.0)
        {
            return Some(DockDrag::Left);
        }
        if layout
            .right_split_x
            .is_some_and(|x| (point.x - x).abs() <= 5.0)
        {
            return Some(DockDrag::Right);
        }
        let terminal = crate::view::terminal_panel_height(&self.view, self.viewport_height);
        let center_start = OUTER_INSET
            + ACTIVITY_BAR_WIDTH
            + layout.left
            + if layout.left > 0.0 { SIDEBAR_GAP } else { 0.0 };
        if terminal > 0.0
            && point.x >= center_start
            && point.x <= center_start + layout.center
            && (point.y - (bottom - terminal)).abs() <= 5.0
        {
            return Some(DockDrag::Bottom);
        }
        None
    }

    pub(super) fn handle_ide_dock_drag(&mut self, event: &UiEvent, viewport_width: f32) -> bool {
        match event {
            UiEvent::PointerDown(point) => {
                self.dock_drag = self.ide_dock_hit(*point, dock_layout(&self.view, viewport_width));
                self.dock_drag.is_some()
            }
            UiEvent::PointerMove(point) => match self.dock_drag {
                Some(DockDrag::Left) => {
                    self.view.ide.explorer_width =
                        (point.x - OUTER_INSET - ACTIVITY_BAR_WIDTH - SIDEBAR_GAP / 2.0)
                            .clamp(180.0, 420.0);
                    true
                }
                Some(DockDrag::Right) => {
                    let list = crate::ide_workspace::agent_list_width(&self.view, viewport_width);
                    let list = if list > 0.0 { list + DIVIDER } else { 0.0 };
                    self.view.ide.agent_width =
                        (viewport_width - point.x - OUTER_INSET - DIVIDER / 2.0 - list)
                            .clamp(320.0, 720.0);
                    true
                }
                Some(DockDrag::Bottom) => {
                    let scope = self.dock_scope();
                    self.dock.set_terminal_maximized(&scope, false);
                    self.view.terminal_maximized = false;
                    self.dock.set_terminal_height_px(
                        &scope.workspace_id,
                        self.viewport_height - OUTER_INSET - STATUSBAR_HEIGHT - point.y,
                    );
                    self.view.terminal_panel_height =
                        self.dock.preferred_terminal_height_px(&scope.workspace_id);
                    true
                }
                None => false,
            },
            UiEvent::PointerUp(_) | UiEvent::PointerLeave => {
                if self.dock_drag.take().is_some() {
                    self.persist_ide_workspace();
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn click(id: &str) -> UiAction {
        UiAction::Clicked { id: id.into() }
    }

    fn loaded_editor_app() -> (tempfile::TempDir, AgentsWindow, DockTabId) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(workspace.join("lib.rs"), "pub fn run() {}\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.selected_session = Some("current-session".into());
        app.viewport_width = 1440.0;
        app.viewport_height = 900.0;
        let scope = app.dock_scope();
        let file = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "main.rs".into(),
            },
        );
        app.dock.activate(&scope, file);
        app.dock.set_collapsed(&scope, true);
        app.sync_dock_view();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app
            .view
            .code_editor
            .as_ref()
            .is_none_or(|editor| editor.loading)
        {
            app.poll_code_editor(&egui::Context::default());
            assert!(
                Instant::now() < deadline,
                "editor worker failed to load fixture"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        app.refresh_file_rows();
        (root, app, file)
    }

    #[test]
    fn ide_mode_round_trip_keeps_dirty_buffer_selection_session_and_terminals() {
        let (root, mut app, file) = loaded_editor_app();
        let scope = app.dock_scope();
        let controller = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .clone()
            .unwrap();
        {
            let shared = controller.shared_buffer();
            let mut buffer = shared.lock().unwrap();
            buffer
                .set_selection(openide_editor::Selection::caret(3))
                .unwrap();
            buffer
                .insert("edited_", openide_editor::EditKind::Paste, 100)
                .unwrap();
            buffer
                .set_selection(openide_editor::Selection {
                    anchor: 3,
                    head: 10,
                })
                .unwrap();
        }
        let before = controller.snapshot().unwrap();
        assert!(before.is_dirty());
        let terminal = app.dock.new_terminal(&scope);
        app.dock.set_terminal_collapsed(&scope, true);
        let files = app.dock.open(&scope, DockTabKind::Files);
        app.dock.activate(&scope, files);
        app.dock.set_collapsed(&scope, true);
        app.view.prompt = "Borrador que continúa entre ventanas".into();
        app.view.running = true;
        app.view.sidebar_collapsed = false;
        app.view.left_dock_width = Some(277.0);
        app.sync_dock_view();
        let agents_width = app.view.right_dock_width;
        assert!(app.apply_ide_workspace_action(&click("window_mode_toggle")));
        assert!(app.ide_workspace_active());
        assert!(
            crate::ide_workspace::IdeWorkspaceState::load(&root.path().join("data")).is_active()
        );
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        assert_eq!(app.dock.active_terminal(&scope).unwrap().id, terminal);
        assert!(app.dock.is_collapsed(&scope));
        let after = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(after.document_id(), before.document_id());
        assert_eq!(after.revision(), before.revision());
        assert_eq!(after.selection(), before.selection());
        assert!(after.is_dirty());
        assert!(app.view.running);
        assert_eq!(
            app.view.selected_session.as_deref(),
            Some("current-session")
        );
        assert_eq!(app.view.prompt, "Borrador que continúa entre ventanas");
        assert!(app.apply_ide_workspace_action(&click("window_mode_toggle")));
        assert!(!app.view.ide.is_active());
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        assert_eq!(app.dock.active_terminal(&scope).unwrap().id, terminal);
        assert!(app.dock.is_collapsed(&scope));
        assert_eq!(app.view.left_dock_width, Some(277.0));
        assert_eq!(app.view.right_dock_width, agents_width);
        assert!(!app.view.sidebar_collapsed);
        assert!(
            !crate::ide_workspace::IdeWorkspaceState::load(&root.path().join("data")).is_active()
        );
    }

    #[test]
    fn ide_footer_rebuilds_for_keyboard_and_pointer_selection_without_document_edits() {
        check_live_footer(WindowMode::Ide);
    }

    #[test]
    fn agents_footer_rebuilds_for_keyboard_and_pointer_selection_without_document_edits() {
        check_live_footer(WindowMode::Agents);
    }

    fn check_live_footer(mode: WindowMode) {
        let (_root, mut app, file) = loaded_editor_app();
        app.view.ide.mode = mode;
        if mode == WindowMode::Agents {
            app.dock.set_collapsed(&app.dock_scope(), false);
            app.sync_dock_view();
            app.view.right_dock_width = Some(600.0);
        }
        let controller = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .clone()
            .unwrap();
        app.view.editor_workbench.document_id = Some(controller.snapshot().unwrap().document_id());
        let size = Size::new(1440.0, 900.0);
        app.tree.set_reduced_motion(true);
        app.tree.set_root(crate::view::build_window_view(
            &app.view,
            size.width,
            size.height,
        ));
        let mut frame = app.tree.layout(size);
        let editor_id = crate::code_editor_ui::view_id(file.get());
        assert!(app.tree.focus(&editor_id));
        let initial = app.ide_editor_status_key().unwrap();

        let assert_cursor = |frame: &UiFrame, expected: &str| {
            assert!(
                frame.commands.iter().any(|command| matches!(command,
                    openide_ui::DrawCommand::Text { text, .. } if text == expected
                )),
                "missing live footer {expected}"
            );
        };
        for (shift, expected) in [(false, "Ln 1, Col 2"), (true, "Ln 1, Col 3 · 1 sel.")] {
            let before = app.ide_editor_status_key();
            // Navigation changes selection without changing revision or emitting
            // EditorChanged; the old cached-root path therefore missed it.
            assert!(app
                .tree
                .handle_event(UiEvent::Key {
                    key: openide_ui::Key::ArrowRight,
                    ctrl: false,
                    shift,
                })
                .is_empty());
            let after = app.ide_editor_status_key();
            assert_ne!(before, after);
            assert_eq!(after.unwrap().1, initial.1);
            let view = &app.view;
            frame =
                relayout_after_updates(&mut app.tree, frame, size, before != after, true, || {
                    crate::view::build_window_view(view, size.width, size.height)
                });
            assert_cursor(&frame, expected);
        }

        let bounds = frame
            .regions
            .iter()
            .find(|region| region.id == editor_id)
            .unwrap()
            .rect;
        let start = openide_ui::Point::new(bounds.x + 50.0, bounds.y + 8.0);
        let end = openide_ui::Point::new(bounds.x + 115.0, bounds.y + 8.0);
        let before = app.ide_editor_status_key();
        for event in [
            UiEvent::PointerDown(start),
            UiEvent::PointerMove(end),
            UiEvent::PointerUp(end),
        ] {
            let actions = app.tree.handle_event(event);
            assert!(!actions
                .iter()
                .any(|action| matches!(action, UiAction::EditorChanged { .. })));
        }
        let after = app.ide_editor_status_key();
        assert_ne!(before, after);
        assert_eq!(after.unwrap().1, initial.1);
        assert!(!after.unwrap().2.is_empty());
        let view = &app.view;
        frame = relayout_after_updates(&mut app.tree, frame, size, before != after, true, || {
            crate::view::build_window_view(view, size.width, size.height)
        });
        let snapshot = controller.snapshot().unwrap();
        let position = snapshot.position_of(snapshot.selection().head).unwrap();
        assert_cursor(
            &frame,
            &format!(
                "Ln {}, Col {} · {} sel.",
                position.row + 1,
                position.column + 1,
                snapshot.selection().range().len()
            ),
        );

        let before_hover = app.ide_editor_status_key();
        app.tree
            .handle_event(UiEvent::PointerMove(openide_ui::Point::new(10.0, 10.0)));
        assert_eq!(app.ide_editor_status_key(), before_hover);
        app.view.ide.mode = WindowMode::Agents;
        app.dock.set_collapsed(&app.dock_scope(), true);
        app.sync_dock_view();
        assert!(app.ide_editor_status_key().is_none());
    }

    #[test]
    fn ide_file_row_click_opens_center_and_retains_explorer() {
        let (_root, mut app, _) = loaded_editor_app();
        app.apply_ide_workspace_action(&click("window_mode_toggle"));
        let index = app
            .view
            .file_rows
            .iter()
            .position(|row| row.name == "lib.rs")
            .unwrap();
        app.tree
            .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
        let frame = app.tree.layout(Size::new(1440.0, 900.0));
        let id = format!("file_row:{index}");
        let rect = frame.regions.iter().find(|r| r.id == id).unwrap().rect;
        let point = openide_ui::Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        for event in [UiEvent::PointerDown(point), UiEvent::PointerUp(point)] {
            for action in app.tree.handle_event(event) {
                app.apply_action(action, &egui::Context::default());
            }
        }
        assert_eq!(app.view.code_editor.as_ref().unwrap().path, "lib.rs");
        app.tree
            .set_root(crate::view::build_window_view(&app.view, 1440.0, 900.0));
        let frame = app.tree.layout(Size::new(1440.0, 900.0));
        let explorer = frame
            .regions
            .iter()
            .find(|r| r.id == "file_tree")
            .unwrap()
            .rect;
        assert!(explorer.x >= OUTER_INSET + ACTIVITY_BAR_WIDTH);
        assert!(explorer.x < OUTER_INSET + ACTIVITY_BAR_WIDTH + 20.0);
        assert!(explorer.width < 300.0);
        assert!(app.view.ide.explorer_open);
    }

    #[test]
    fn ide_control_tab_cycles_only_visible_resource_tabs_when_agents_dock_collapsed() {
        let (_root, mut app, first) = loaded_editor_app();
        let scope = app.dock_scope();
        app.dock.open(&scope, DockTabKind::Files);
        app.dock.open(&scope, DockTabKind::Context);
        let second = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "lib.rs".into(),
            },
        );
        app.dock.activate(&scope, first);
        app.dock.set_collapsed(&scope, true);
        app.view.ide.mode = WindowMode::Ide;
        app.sync_dock_view();
        assert!(!app.view.show_side_pane);
        let forward = UiEvent::Key {
            key: openide_ui::Key::Tab,
            ctrl: true,
            shift: false,
        };
        assert!(app.handle_dock_tab_event(&forward));
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, second);
        assert!(app.handle_dock_tab_event(&forward));
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, first);
    }

    #[test]
    fn ide_explorer_and_quick_open_do_not_replace_active_document() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.ide.mode = WindowMode::Ide;
        app.view.ide.explorer_open = false;
        let scope = app.dock_scope();
        let file = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "README.md".into(),
            },
        );
        app.dock.activate(&scope, file);
        app.open_files_dock();
        assert!(app.view.ide.explorer_open);
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        assert!(app.apply_ide_workspace_action(&click("ide_quick_open")));
        assert!(app.view.show_command_center);
        assert_eq!(app.view.command_scope, Scope::Files);
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
    }

    #[test]
    fn ide_mode_and_panel_preferences_restore_in_a_new_host() {
        let root = tempfile::tempdir().unwrap();
        {
            let mut app = AgentsWindow::with_data_root(root.path().to_owned());
            app.view.ide.explorer_width = 282.0;
            app.view.ide.agent_width = 415.0;
            app.view.ide.agent_open = false;
            app.apply_ide_workspace_action(&click("window_mode_toggle"));
        }
        let reopened = AgentsWindow::with_data_root(root.path().to_owned());
        assert!(reopened.ide_workspace_active());
        assert_eq!(reopened.view.ide.explorer_width, 282.0);
        assert_eq!(reopened.view.ide.agent_width, 415.0);
        assert!(!reopened.view.ide.agent_open);
        assert!(reopened.view.ide.compact_panel.is_none());
    }

    #[test]
    fn ide_panel_toggles_reverse_requested_visibility_without_opening_animation_drawers() {
        for (panel, action) in [
            (IdePanel::Explorer, "ide_explorer_toggle"),
            (IdePanel::Agent, "ide_agent_toggle"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut app = AgentsWindow::with_data_root(root.path().to_owned());
            app.view.ide.mode = WindowMode::Ide;
            app.viewport_width = 1440.0;
            app.viewport_height = 900.0;
            let requested = |app: &AgentsWindow| match panel {
                IdePanel::Explorer => app.view.ide.explorer_open,
                IdePanel::Agent => app.view.ide.agent_open,
            };
            assert!(app.apply_ide_workspace_action(&click(action)));
            assert!(!requested(&app));
            match panel {
                IdePanel::Explorer => app.view.left_presentation_width = Some(120.0),
                IdePanel::Agent => app.view.right_presentation_width = Some(160.0),
            }
            // Still painted during closing, but the next click must reopen.
            assert!(app.apply_ide_workspace_action(&click(action)));
            assert!(requested(&app));
            assert!(app.view.ide.compact_panel.is_none());
            assert!(!app.ide_modal_panel_visible());
            match panel {
                IdePanel::Explorer => app.view.left_presentation_width = Some(0.0),
                IdePanel::Agent => app.view.right_presentation_width = Some(0.0),
            }
            // The first opening frame is still zero pixels wide. A second
            // click closes the destination instead of creating a compact drawer.
            assert!(app.apply_ide_workspace_action(&click(action)));
            assert!(!requested(&app));
            assert!(app.view.ide.compact_panel.is_none());
            match panel {
                IdePanel::Explorer => app.open_ide_explorer(),
                IdePanel::Agent => app.reveal_ide_agent(),
            }
            assert!(requested(&app));
            assert!(app.view.ide.compact_panel.is_none());
            assert_eq!(app.view.ide.explorer_width, 240.0);
            assert_eq!(app.view.ide.agent_width, 380.0);
        }
    }

    #[test]
    fn switching_workspace_mode_clears_all_transient_dock_dimensions() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        for _ in 0..2 {
            app.view.left_presentation_width = Some(110.0);
            app.view.right_presentation_width = Some(150.0);
            app.view.ide.explorer_content_width = Some(240.0);
            app.view.ide.agent_content_width = Some(380.0);
            app.view.bottom_presentation_height = Some(70.0);
            assert!(app.apply_ide_workspace_action(&click("window_mode_toggle")));
            assert!(app.view.left_presentation_width.is_none());
            assert!(app.view.right_presentation_width.is_none());
            assert!(app.view.ide.explorer_content_width.is_none());
            assert!(app.view.ide.agent_content_width.is_none());
            assert!(app.view.bottom_presentation_height.is_none());
        }
    }

    #[test]
    fn ide_compact_actions_reveal_panels_and_search_replaces_the_drawer() {
        let (_root, mut app, file) = loaded_editor_app();
        app.view.ide.mode = WindowMode::Ide;
        app.viewport_width = 320.0;
        app.apply_ide_workspace_action(&click("ide_explorer_toggle"));
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Explorer));
        assert!(app.view.ide.explorer_open);
        assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, file);
        app.apply_action(click("files_search"), &egui::Context::default());
        assert!(app.view.ide.compact_panel.is_none());
        assert!(app.view.show_command_center);
        assert_eq!(app.view.command_scope, Scope::Files);
        app.view.show_command_center = false;
        app.apply_ide_workspace_action(&click("ide_agent_toggle"));
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Agent));
        assert!(!app.handle_dock_tab_event(&UiEvent::Key {
            key: openide_ui::Key::Tab,
            ctrl: true,
            shift: false
        }));
        assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, file);
        app.apply_ide_workspace_action(&click("ide_panel_dismiss"));
        assert!(app.view.ide.compact_panel.is_none());
        app.view.ide.agent_open = false;
        app.new_session();
        assert!(app.view.ide.agent_open);
        assert_eq!(app.view.ide.compact_panel, Some(IdePanel::Agent));
    }

    #[test]
    fn ide_agent_drawer_composer_keeps_enter_and_suggestions_with_nested_menu_guards() {
        let (_root, mut app, _) = loaded_editor_app();
        let ctx = egui::Context::default();
        let size = Size::new(800.0, 900.0);
        app.viewport_width = size.width;
        app.viewport_height = size.height;
        app.view.ide.mode = WindowMode::Ide;
        app.view.ide.compact_panel = Some(IdePanel::Agent);
        app.view.selected_session = None;
        app.tree.set_reduced_motion(true);
        app.tree.set_root(crate::view::build_window_view(
            &app.view,
            size.width,
            size.height,
        ));
        app.tree.layout(size);
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert!(app.tree.has_open_popover());
        assert!(!app.composer_popup_blocks_input());
        let enter = UiEvent::Key {
            key: openide_ui::Key::Enter,
            shift: false,
            ctrl: false,
        };
        // An empty draft cannot launch a provider; Enter still follows the
        // composer's submit route rather than inserting a raw newline.
        assert!(app.handle_composer_key(&enter));
        assert!(app.view.prompt.is_empty());
        assert!(app.active_run.is_none());
        app.view.prompt = "/".into();
        app.tree.set_input_value("prompt", "/");
        app.tree.set_input_cursor("prompt", 1);
        app.update_composer_suggestions(&ctx);
        assert!(app.view.composer_suggestions.trigger.is_some());
        assert!(app.handle_composer_suggestion_key(
            &UiEvent::Key {
                key: openide_ui::Key::ArrowDown,
                ctrl: false,
                shift: false
            },
            &ctx
        ));
        app.tree.handle_event(UiEvent::ContextMenuKeyboard);
        assert!(app.tree.context_menu_open());
        assert!(app.composer_popup_blocks_input());
        assert!(!app.handle_composer_key(&enter));
        app.tree.dismiss_context_menu();
        app.view.composer_context_open = true;
        app.tree.set_root(crate::view::build_window_view(
            &app.view,
            size.width,
            size.height,
        ));
        let frame = app.tree.layout(size);
        assert!(frame
            .regions
            .iter()
            .any(|region| region.id == "popover_surface:composer_context_dismiss"));
        assert!(app.composer_popup_blocks_input());
        assert!(!app.handle_composer_key(&enter));
        assert!(!app.handle_composer_suggestion_key(&enter, &ctx));
    }

    #[test]
    fn ide_resizers_exclude_title_status_and_assistant_terminal_area() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().to_owned());
        app.view.ide.mode = WindowMode::Ide;
        app.viewport_width = 1440.0;
        app.viewport_height = 900.0;
        app.view.show_terminal_panel = true;
        let layout = dock_layout(&app.view, app.viewport_width);
        let left = layout.left_split_x.unwrap();
        assert_eq!(
            app.ide_dock_hit(openide_ui::Point::new(left, 10.0), layout),
            None
        );
        assert_eq!(
            app.ide_dock_hit(openide_ui::Point::new(left, 100.0), layout),
            Some(DockDrag::Left)
        );
        assert_eq!(
            app.ide_dock_hit(openide_ui::Point::new(left, 890.0), layout),
            None
        );
        let split = app.viewport_height
            - OUTER_INSET
            - STATUSBAR_HEIGHT
            - crate::view::terminal_panel_height(&app.view, app.viewport_height);
        assert_eq!(
            app.ide_dock_hit(openide_ui::Point::new(700.0, split), layout),
            Some(DockDrag::Bottom)
        );
        assert_eq!(
            app.ide_dock_hit(openide_ui::Point::new(1400.0, split), layout),
            None
        );
    }
}
