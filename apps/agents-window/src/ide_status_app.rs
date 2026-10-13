//! Footer actions are scoped to the visible document, including its revision.
use super::*;
use crate::ide_status_ui::{self, IdeStatusView, StatusMenu, StatusScope};
use openide_editor::{EditorCommand, IndentStyle, LineEnding};

const INVALID_SERVICE_PATH: &str = "Ruta de servidor inválida";

impl AgentsWindow {
    fn ide_status_scope_current(&self, scope: &StatusScope) -> bool {
        self.dock.active_tab(&self.dock_scope()).is_some_and(|tab| {
            tab.id.get() == scope.tab
                && matches!(&tab.kind,
                DockTabKind::FilePreview { path } if path == Path::new(&scope.path))
        }) && self
            .code_editor
            .view_for_path(Path::new(&scope.workspace), Path::new(&scope.path))
            .filter(|view| view.tab_id == scope.tab)
            .and_then(|view| view.controller)
            .and_then(|controller| controller.snapshot().ok())
            .is_some_and(|snapshot| {
                (snapshot.document_id(), snapshot.revision()) == (scope.document, scope.revision)
            })
    }

    fn ide_status_visible(&self) -> bool {
        self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && !(self.ide_workspace_active() && self.view.ide.extensions_open)
            && !self.view.show_command_center
            && !self.view.environment.open
            && (self.ide_workspace_active()
                || (self.view.show_side_pane
                    && !self.dock.is_collapsed(&self.dock_scope())
                    && dock_layout(&self.view, self.viewport_width).right.is_some()))
    }

    /// Shared by scoped actions and the cursor-only repaint key. A dock that
    /// is closing may still be painted, but must not accept stale footer input.
    pub(super) fn document_status_visible(&self) -> bool {
        self.ide_status_visible()
            && self.view.side_tab == SideTab::FilePreview
            && self
                .view
                .document_preview
                .as_ref()
                .is_none_or(|preview| preview.is_source_visible())
    }

    pub(super) fn poll_ide_status(&mut self) -> bool {
        if self.view.ide_status.menu.is_none() {
            return false;
        }
        let current = ide_status_ui::document_scope(&self.view);
        let valid = self.view.ide_status.workspace.as_deref() == Some(self.view.workspace.as_str())
            && ((self.view.ide_status.menu == Some(StatusMenu::Branch)
                && self.ide_workspace_active()
                && self.ide_status_visible())
                || (self.view.ide_status.menu != Some(StatusMenu::Branch)
                    && self.document_status_visible()
                    && self.view.ide_status.scope == current
                    && current
                        .as_ref()
                        .is_some_and(|scope| self.ide_status_scope_current(scope))))
            && (self.view.ide_status.menu != Some(StatusMenu::Services)
                || self.view.ide_status.service_language == self.editor_language_scope().map(|(_, language)| language));
        if !valid {
            self.view.ide_status = IdeStatusView::default();
        }
        let connecting = valid && self.view.ide_status.menu == Some(StatusMenu::Services)
            && self.editor_language_connecting();
        let changed = self.view.ide_status.service_connecting != connecting;
        self.view.ide_status.service_connecting = connecting;
        !valid || changed
    }

    pub(super) fn editor_services_popover_current(&self) -> bool {
        self.view.ide_status.menu == Some(StatusMenu::Services)
            && self.document_status_visible()
            && self.view.ide_status.workspace.as_deref() == Some(self.view.workspace.as_str())
            && self.view.ide_status.scope == ide_status_ui::document_scope(&self.view)
            && self.view.ide_status.scope.as_ref().is_some_and(|scope| self.ide_status_scope_current(scope))
            && self.view.ide_status.service_language == self.editor_language_scope().map(|(_, language)| language)
    }

    fn set_service_path_draft(&mut self, value: &str) -> bool {
        // Preserve the entered draft, including a rejected control character,
        // so Connect can never silently fall back to a previous executable.
        // Oversized input remains invalid without retaining an unbounded copy.
        if value.len() <= 4096 {
            self.view.editor_workbench.server_path = value.into();
        }
        if value.len() > 4096 || value.chars().any(char::is_control) {
            self.view.ide_status.service_error = Some(INVALID_SERVICE_PATH.into());
            return false;
        }
        self.view.ide_status.service_error = None;
        true
    }

    fn connect_editor_service(&mut self, submitted: Option<&str>, ctx: &egui::Context) {
        let input_id = format!("{}:server_path", self.view.ide_status.scope.as_ref().expect("validated services scope").prefix());
        let raw = submitted
            .or_else(|| self.tree.input_value(&input_id))
            .unwrap_or(&self.view.editor_workbench.server_path);
        // Validate the actual field again, rather than relying on its last
        // InputChanged/error. Other asynchronous status updates cannot turn an
        // oversized draft into a request for the previous valid executable.
        if raw.len() > 4096 || raw.chars().any(char::is_control) {
            self.view.ide_status.service_error = Some(INVALID_SERVICE_PATH.into());
            ctx.request_repaint();
            return;
        }
        let path = raw.to_owned();
        if self.editor_language_connecting() {
            self.view.ide_status.service_connecting = true;
            ctx.request_repaint();
            return;
        }
        if !self.set_service_path_draft(&path) {
            ctx.request_repaint();
            return;
        }
        match self.restart_editor_language(&path) {
            Ok(()) => {
                // Keep editor/file errors separate. Immediate persistence
                // errors belong to the same popover as this configuration.
                let previous_error = self.view.editor_workbench.error.take();
                self.persist_editor_preferences(true);
                self.view.ide_status.service_error = self.view.editor_workbench.error.take();
                self.view.editor_workbench.error = previous_error;
                self.view.ide_status.service_preferences_sequence = if self.view.ide_status.service_error.is_none() {
                    self.editor_preferences_sequence()
                } else {
                    None
                };
            }
            Err(error) => self.view.ide_status.service_error = Some(error),
        }
        self.view.ide_status.service_connecting = self.editor_language_connecting();
        ctx.request_repaint();
    }

    pub(super) fn apply_ide_status_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let id = match action {
            UiAction::Clicked { id } | UiAction::InputChanged { id, .. } | UiAction::Submitted { id, .. } => id,
            _ => return false,
        };
        let clicked = matches!(action, UiAction::Clicked { .. });
        if id.starts_with("ide_status_branch") {
            if !clicked { return true; }
            if !self.ide_workspace_active() || !self.ide_status_visible() {
                return true;
            }
            if id == "ide_status_branch_open" {
                let open = self.view.ide_status.menu != Some(StatusMenu::Branch)
                    || self.view.ide_status.workspace.as_deref()
                        != Some(self.view.workspace.as_str());
                self.view.ide_status = if open {
                    IdeStatusView {
                        menu: Some(StatusMenu::Branch),
                        scope: None,
                        workspace: Some(self.view.workspace.clone()),
                        ..Default::default()
                    }
                } else {
                    IdeStatusView::default()
                };
            } else if self.view.ide_status.menu == Some(StatusMenu::Branch)
                && self.view.ide_status.workspace.as_deref() == Some(self.view.workspace.as_str())
            {
                self.view.ide_status = IdeStatusView::default();
                match id.as_str() {
                    "ide_status_branch:review" => {
                        self.reveal_ide_activity(crate::ide_workspace::IdeActivity::Git, ctx)
                    }
                    "ide_status_branch:checkout" => {
                        if !self.view.environment.open {
                            self.toggle_environment();
                        }
                        self.apply_action(
                            UiAction::Clicked {
                                id: "environment_branch_toggle".into(),
                            },
                            ctx,
                        );
                    }
                    _ => {}
                }
            }
            ctx.request_repaint();
            return true;
        }
        let Some(rest) = id.strip_prefix("ide_status:") else {
            return false;
        };
        if !self.document_status_visible() {
            return true;
        }
        let Some(scope) = ide_status_ui::document_scope(&self.view) else {
            return true;
        };
        let mut parts = rest.splitn(4, ':');
        let identity = (
            parts.next().and_then(|v| v.parse::<u64>().ok()),
            parts.next().and_then(|v| v.parse::<u64>().ok()),
            parts.next().and_then(|v| v.parse::<u64>().ok()),
        );
        let Some(operation) = parts.next() else {
            return true;
        };
        if identity != (Some(scope.tab), Some(scope.document), Some(scope.revision))
            || !self.ide_status_scope_current(&scope)
        {
            return true;
        }
        if matches!(operation, "server_path" | "server_connect") {
            if !self.editor_services_popover_current() { return true; }
            match action {
                UiAction::InputChanged { value, .. } if operation == "server_path" => {
                    self.set_service_path_draft(value);
                    ctx.request_repaint();
                }
                UiAction::Submitted { value, .. } if operation == "server_path" => {
                    if self.set_service_path_draft(value) { self.connect_editor_service(Some(value), ctx); }
                    ctx.request_repaint();
                }
                UiAction::Clicked { .. } if operation == "server_connect" => self.connect_editor_service(None, ctx),
                _ => {},
            }
            return true;
        }
        if !clicked { return true; }
        if let Some(kind) = operation.strip_prefix("open:").and_then(|key| match key {
            "indent" => Some(StatusMenu::Indent),
            "ending" => Some(StatusMenu::Ending),
            "language" => Some(StatusMenu::Language),
            "overflow" => Some(StatusMenu::Overflow),
            "services" => Some(StatusMenu::Services),
            _ => None,
        }) {
            let open = self.view.ide_status.menu != Some(kind)
                || self.view.ide_status.scope.as_ref() != Some(&scope);
            self.view.ide_status = if open {
                IdeStatusView {
                    menu: Some(kind),
                    scope: Some(scope.clone()),
                    workspace: Some(self.view.workspace.clone()),
                    ..Default::default()
                }
            } else {
                IdeStatusView::default()
            };
            if open && kind == StatusMenu::Services {
                self.view.ide_status.service_language = self.editor_language_scope().map(|(_, language)| language);
                self.view.ide_status.service_connecting = self.editor_language_connecting();
                self.view.editor_workbench.server_path = self.editor_language_server_path();
                self.tree.set_input_value(&format!("{}:server_path", scope.prefix()), &self.view.editor_workbench.server_path);
                self.view.editor_workbench.settings_open = false;
            }
            self.view.editor_workbench.menu_open = false;
            self.view.editor_workbench.breadcrumb_open = false;
            self.view.editor_workbench.language_menu_open = false;
            self.view.show_dock_add_menu = false;
            self.view.show_dock_tabs_menu = false;
            self.view.ide.menu_open = None;
            ctx.request_repaint();
            return true;
        }
        if matches!(operation, "goto" | "diagnostics" | "settings") {
            self.view.ide_status = IdeStatusView::default();
            self.apply_action(
                UiAction::Clicked {
                    id: format!("editor_{operation}"),
                },
                ctx,
            );
            return true;
        }
        // Selection actions are only valid while their originating menu is open.
        // A revision or workspace change invalidates both the popup and its IDs.
        if self.view.ide_status.scope.as_ref() != Some(&scope)
            || self.view.ide_status.workspace.as_deref() != Some(scope.workspace.as_str())
        {
            return true;
        }
        let menu = self.view.ide_status.menu;
        let allows = |specific| menu == Some(specific) || menu == Some(StatusMenu::Overflow);
        let mut handled = false;
        if operation == "services_dismiss" {
            if self.editor_services_popover_current() { handled = true; }
        } else if operation == "dismiss" {
            handled = true;
        } else if let Some(language) = operation
            .strip_prefix("language:")
            .filter(|_| allows(StatusMenu::Language))
        {
            if matches!(
                language,
                "auto"
                    | "plain"
                    | "markdown"
                    | "rust"
                    | "javascript"
                    | "typescript"
                    | "tsx"
                    | "python"
                    | "json"
                    | "toml"
                    | "css"
                    | "html"
            ) {
                self.apply_editor_workbench_action(
                    &UiAction::Clicked {
                        id: format!("editor_language:{language}"),
                    },
                    ctx,
                );
                handled = true;
            }
        } else if let Some(ending) = operation
            .strip_prefix("ending:")
            .filter(|_| allows(StatusMenu::Ending))
        {
            if let Some(ending) = match ending {
                "lf" => Some(LineEnding::Lf),
                "crlf" => Some(LineEnding::CrLf),
                "cr" => Some(LineEnding::Cr),
                _ => None,
            } {
                if let Err(error) = self.editor_command(EditorCommand::SetLineEnding(ending), ctx) {
                    self.view.editor_workbench.error = Some(error);
                }
                handled = true;
            }
        } else if allows(StatusMenu::Indent) {
            let current = self.view.editor_workbench.indent;
            let next = match operation {
                "indent:spaces" => Some(IndentStyle::Spaces(current.width())),
                "indent:tabs" => Some(IndentStyle::Tabs(current.width())),
                "width:2" | "width:4" | "width:8" => {
                    let width = operation[6..].parse().expect("matched tab width");
                    Some(match current {
                        IndentStyle::Spaces(_) => IndentStyle::Spaces(width),
                        IndentStyle::Tabs(_) => IndentStyle::Tabs(width),
                    })
                }
                _ => None,
            };
            if let Some(next) = next {
                self.view.editor_workbench.indent = next;
                self.persist_editor_preferences(false);
                handled = true;
            }
        }
        if handled {
            self.view.ide_status = IdeStatusView::default();
            if !matches!(operation, "dismiss" | "services_dismiss") {
                self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(scope.tab));
            }
            ctx.request_repaint();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::WindowMode;
    use openide_editor::{EditKind, Selection};

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        fixture_in_mode(WindowMode::Ide)
    }

    fn fixture_in_mode(mode: WindowMode) -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("one.rs"), "one\r\ntwo\r\n").unwrap();
        std::fs::write(workspace.join("two.rs"), "other\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.view.ide.mode = mode;
        app.view.selected_session = None;
        app.viewport_width = 1200.0;
        app.viewport_height = 800.0;
        app.dock = DockState::default();
        open(&mut app, "one.rs");
        (root, app)
    }

    fn open(app: &mut AgentsWindow, path: &str) {
        let scope = app.dock_scope();
        app.dock
            .open(&scope, DockTabKind::FilePreview { path: path.into() });
        app.dock.set_collapsed(&scope, false);
        app.sync_dock_view();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_code_editor(&egui::Context::default());
            if app
                .view
                .code_editor
                .as_ref()
                .is_some_and(|view| !view.loading)
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        app.poll_editor_workbench(&egui::Context::default());
    }

    fn action(app: &mut AgentsWindow, operation: &str) -> String {
        let id = format!(
            "{}:{operation}",
            ide_status_ui::document_scope(&app.view).unwrap().prefix()
        );
        assert!(app.apply_ide_status_action(
            &UiAction::Clicked { id: id.clone() },
            &egui::Context::default()
        ));
        id
    }

    #[test]
    fn branch_review_reveals_git_without_hiding_an_already_open_panel() {
        let (_root, mut app) = fixture();
        let ctx = egui::Context::default();
        app.view.ide.activity = crate::ide_workspace::IdeActivity::Git;
        app.view.ide.explorer_open = true;
        for _ in 0..2 {
            assert!(app.apply_ide_status_action(
                &UiAction::Clicked {
                    id: "ide_status_branch_open".into()
                },
                &ctx
            ));
            assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Branch));
            assert!(app.apply_ide_status_action(
                &UiAction::Clicked {
                    id: "ide_status_branch:review".into()
                },
                &ctx
            ));
            assert_eq!(
                app.view.ide.activity,
                crate::ide_workspace::IdeActivity::Git
            );
            assert!(app.view.ide.explorer_open);
            assert!(app.view.ide_status.menu.is_none());
        }
    }

    #[test]
    fn footer_line_endings_are_real_undoable_edits_and_do_not_save() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_line_endings(mode);
        }
    }

    fn check_line_endings(mode: WindowMode) {
        let (_root, mut app) = fixture_in_mode(mode);
        let controller = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .clone()
            .unwrap();
        for (key, ending, expected) in [
            ("lf", LineEnding::Lf, "one\ntwo\n"),
            ("cr", LineEnding::Cr, "one\rtwo\r"),
            ("crlf", LineEnding::CrLf, "one\r\ntwo\r\n"),
        ] {
            action(&mut app, "open:ending");
            action(&mut app, &format!("ending:{key}"));
            let snapshot = controller.snapshot().unwrap();
            assert_eq!(snapshot.preferred_line_ending(), ending);
            assert_eq!(
                snapshot.text_range(0..snapshot.char_count()).unwrap(),
                expected
            );
            assert_eq!(
                std::fs::read_to_string(Path::new(&app.view.workspace).join("one.rs")).unwrap(),
                "one\r\ntwo\r\n"
            );
        }
        app.editor_command(EditorCommand::Undo, &egui::Context::default())
            .unwrap();
        assert_eq!(
            controller.snapshot().unwrap().preferred_line_ending(),
            LineEnding::Cr
        );
    }

    #[test]
    fn footer_actions_reject_stale_revision_tab_and_workspace() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_stale_scope(mode);
        }
    }

    fn check_stale_scope(mode: WindowMode) {
        let (_root, mut app) = fixture_in_mode(mode);
        action(&mut app, "open:ending");
        let scope = ide_status_ui::document_scope(&app.view).unwrap();
        let stale = format!("{}:ending:lf", scope.prefix());
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
            buffer.set_selection(Selection::caret(0)).unwrap();
            buffer.insert("x", EditKind::Paste, 1).unwrap();
        }
        assert!(app.poll_ide_status());
        app.apply_ide_status_action(
            &UiAction::Clicked { id: stale.clone() },
            &egui::Context::default(),
        );
        assert_eq!(
            controller.snapshot().unwrap().preferred_line_ending(),
            LineEnding::CrLf
        );
        action(&mut app, "open:ending");
        let stale = format!(
            "{}:ending:lf",
            ide_status_ui::document_scope(&app.view).unwrap().prefix()
        );
        open(&mut app, "two.rs");
        assert!(app.poll_ide_status());
        app.apply_ide_status_action(&UiAction::Clicked { id: stale }, &egui::Context::default());
        assert_eq!(
            controller.snapshot().unwrap().preferred_line_ending(),
            LineEnding::CrLf
        );
        action(&mut app, "open:indent");
        app.view.workspace.push_str("/other");
        assert!(app.poll_ide_status());
        assert!(app.view.ide_status.menu.is_none());
    }

    #[test]
    fn indentation_language_and_cursor_use_existing_editor_paths() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_document_actions(mode);
        }
    }

    fn check_document_actions(mode: WindowMode) {
        let (_root, mut app) = fixture_in_mode(mode);
        let before = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap();
        action(&mut app, "open:indent");
        action(&mut app, "indent:tabs");
        action(&mut app, "open:indent");
        action(&mut app, "width:8");
        assert_eq!(app.view.editor_workbench.indent, IndentStyle::Tabs(8));
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
        assert_eq!(
            after.revision(),
            before.revision(),
            "indent preferences must not rewrite source"
        );
        action(&mut app, "open:language");
        action(&mut app, "language:markdown");
        assert_eq!(
            app.view.editor_workbench.language_override,
            Some(openide_ui::CodeLanguage::Markdown)
        );
        action(&mut app, "open:language");
        action(&mut app, "language:auto");
        assert!(app.view.editor_workbench.language_override.is_none());
        action(&mut app, "goto");
        assert_eq!(
            app.view.editor_workbench.form,
            Some(crate::editor_workbench_ui::EditorForm::Goto)
        );
        action(&mut app, "settings");
        assert!(app.view.editor_workbench.settings_open);
        action(&mut app, "open:services");
        assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Services));
        assert!(!app.view.editor_workbench.settings_open);
    }

    #[test]
    fn agents_document_footer_invalidates_hidden_or_replaced_surfaces() {
        let scenarios: &[(&str, fn(&mut AgentsWindow))] = &[
            ("collapsed", |app| {
                app.dock.set_collapsed(&app.dock_scope(), true);
                app.sync_dock_view();
                // Closing animation can still paint the dock for a few frames.
                app.view.right_presentation_width = Some(380.0);
            }),
            ("narrow viewport", |app| app.viewport_width = 600.0),
            ("environment", |app| app.open_environment()),
            ("settings", |app| app.view.show_settings = true),
            ("command center", |app| app.view.show_command_center = true),
            ("marketplace", |app| app.view.main_page = MainPage::Marketplace),
            ("another dock", |app| {
                app.dock.open(&app.dock_scope(), DockTabKind::Files);
                app.sync_dock_view();
            }),
            ("another session", |app| {
                app.view.selected_session = Some("different-session".into());
            }),
        ];
        for (name, change) in scenarios {
            let (_root, mut app) = fixture_in_mode(WindowMode::Agents);
            action(&mut app, "open:indent");
            assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Indent), "{name}");
            let stale = format!(
                "{}:indent:tabs",
                ide_status_ui::document_scope(&app.view).unwrap().prefix()
            );
            let before = app.view.editor_workbench.indent;
            change(&mut app);
            app.poll_ide_status();
            assert!(app.view.ide_status.menu.is_none(), "{name}");
            app.apply_ide_status_action(&UiAction::Clicked { id: stale }, &egui::Context::default());
            assert_eq!(app.view.editor_workbench.indent, before, "{name}");
        }
    }

    #[test]
    fn preview_only_rejects_document_actions_but_source_and_split_allow_them() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            let (_root, mut app) = fixture_in_mode(mode);
            let scope = ide_status_ui::document_scope(&app.view).unwrap();
            app.view.document_preview = Some(crate::document_preview::PreviewView {
                scope: openide_preview::Scope {
                    workspace: scope.workspace.into(),
                    path: scope.path.into(),
                    tab_id: scope.tab,
                    generation: 1,
                },
                kind: openide_preview::PreviewKind::Markdown,
                mode: crate::document_preview::PreviewMode::Split,
                document: None,
                raster: None,
                loading: false,
                error: None,
                page: 0,
                pages: 0,
                zoom: None,
            });
            action(&mut app, "open:ending");
            assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Ending));
            let stale = format!("{}:goto", ide_status_ui::document_scope(&app.view).unwrap().prefix());
            app.view.document_preview.as_mut().unwrap().mode = crate::document_preview::PreviewMode::Preview;
            assert!(app.poll_ide_status());
            assert!(app.ide_editor_status_key().is_none());
            app.apply_ide_status_action(&UiAction::Clicked { id: stale }, &egui::Context::default());
            assert!(app.view.editor_workbench.form.is_none());
            app.view.document_preview.as_mut().unwrap().mode = crate::document_preview::PreviewMode::Source;
            action(&mut app, "goto");
            assert!(app.ide_editor_status_key().is_some());
            assert_eq!(app.view.editor_workbench.form, Some(crate::editor_workbench_ui::EditorForm::Goto));
        }
    }

    #[test]
    fn agents_footer_opens_bottom_problems_and_rejects_window_branch_actions() {
        let (_root, mut app) = fixture_in_mode(WindowMode::Agents);
        action(&mut app, "diagnostics");
        assert_eq!(app.view.ide.bottom_panel, crate::ide_workspace::IdeBottomPanel::Problems);
        assert!(app.view.ide.bottom_open);
        assert!(app.view.editor_workbench.hover.is_none());
        assert!(app.view.editor_workbench.results.is_empty());
        assert!(app.terminals.is_empty());
        app.apply_ide_status_action(
            &UiAction::Clicked { id: "ide_status_branch_open".into() },
            &egui::Context::default(),
        );
        assert!(app.view.ide_status.menu.is_none());

        app.view.ide.mode = WindowMode::Ide;
        app.apply_ide_status_action(
            &UiAction::Clicked { id: "ide_status_branch_open".into() },
            &egui::Context::default(),
        );
        assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Branch));
        app.view.ide.mode = WindowMode::Agents;
        assert!(app.poll_ide_status());
        assert!(app.view.ide_status.menu.is_none());
    }

    fn services_fixture(mode: WindowMode) -> (tempfile::TempDir, AgentsWindow) {
        let (root, mut app) = fixture_in_mode(mode);
        // Cancel any automatic Rust startup from the shared editor fixture.
        // Plain reaches a deterministic no-server state without starting a PTY
        // or relying on an installed language server for these offline tests.
        app.view.editor_workbench.language_override = Some(openide_ui::CodeLanguage::Plain);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_editor_language(&egui::Context::default());
            if !app.editor_language_connecting() { break; }
            assert!(Instant::now() < deadline, "fixture language startup did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
        app.view.editor_workbench.language_override = None;
        (root, app)
    }

    fn service_input(app: &mut AgentsWindow, value: &str, submit: bool) {
        let id = format!("{}:server_path", ide_status_ui::document_scope(&app.view).unwrap().prefix());
        // Native input updates its retained value before dispatching either
        // event. Reproduce that order, so Connect reads the actual field.
        app.tree.set_input_value(&id, value);
        let event = if submit {
            UiAction::Submitted { id, value: value.into() }
        } else {
            UiAction::InputChanged { id, value: value.into() }
        };
        assert!(app.apply_ide_status_action(&event, &egui::Context::default()));
    }

    #[test]
    fn services_draft_follows_language_workspace_and_reopened_native_input() {
        let (root, mut app) = services_fixture(WindowMode::Agents);
        app.view.editor_workbench.server_path = "/fixture/rust-server".into();
        app.persist_editor_preferences(true);
        app.view.editor_workbench.language_override = Some(openide_ui::CodeLanguage::Python);
        app.view.editor_workbench.server_path = "/fixture/python-server".into();
        app.persist_editor_preferences(true);
        app.view.editor_workbench.language_override = None;
        app.view.editor_workbench.settings_open = true;
        action(&mut app, "open:services");
        assert_eq!(app.view.editor_workbench.server_path, "/fixture/rust-server");
        assert_eq!(app.view.ide_status.service_language, Some(openide_ui::CodeLanguage::Rust));
        assert!(!app.view.editor_workbench.settings_open);
        let input = format!("{}:server_path", ide_status_ui::document_scope(&app.view).unwrap().prefix());
        service_input(&mut app, "/unsaved/rust-draft", false);
        action(&mut app, "services_dismiss");
        action(&mut app, "open:services");
        assert_eq!(app.tree.input_value(&input), Some("/fixture/rust-server"));
        assert_eq!(app.view.editor_workbench.server_path, "/fixture/rust-server");

        // Language changes do not change the buffer revision. The additional
        // popup language identity prevents applying the Rust draft to Python.
        app.view.editor_workbench.language_override = Some(openide_ui::CodeLanguage::Python);
        let sequence = app.editor_preferences_sequence();
        action(&mut app, "server_connect");
        assert_eq!(app.editor_preferences_sequence(), sequence);
        assert!(app.poll_ide_status());
        action(&mut app, "open:services");
        assert_eq!(app.view.editor_workbench.server_path, "/fixture/python-server");
        assert_eq!(app.tree.input_value(&input), Some("/fixture/python-server"));

        let other = root.path().join("other-workspace");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("one.rs"), "other\n").unwrap();
        app.view.workspace = other.to_string_lossy().into_owned();
        assert!(app.poll_ide_status());
        open(&mut app, "one.rs");
        action(&mut app, "open:services");
        assert!(app.view.editor_workbench.server_path.is_empty());
    }

    #[test]
    fn services_reject_invalid_visible_drafts_without_connecting_or_persisting() {
        let (_root, mut app) = services_fixture(WindowMode::Agents);
        app.view.editor_workbench.server_path = "/fixture/saved-server".into();
        app.persist_editor_preferences(true);
        action(&mut app, "open:services");
        let sequence = app.editor_preferences_sequence();
        for value in ["bad\0server".into(), "bad\tserver".into(), "bad\nserver".into(), "x".repeat(4097)] {
            service_input(&mut app, &value, false);
            assert_eq!(app.view.ide_status.service_error.as_deref(), Some(INVALID_SERVICE_PATH));
            // A separate asynchronous error may replace the validation text;
            // Connect must still validate the visible input, not the message.
            app.view.ide_status.service_error = Some("Error al guardar preferencias".into());
            action(&mut app, "server_connect");
            assert_eq!(app.view.ide_status.service_error.as_deref(), Some(INVALID_SERVICE_PATH));
            assert_eq!(app.editor_preferences_sequence(), sequence);
            assert_eq!(app.editor_language_server_path(), "/fixture/saved-server");
            assert!(!app.editor_language_connecting());
            assert!(app.view.editor_workbench.error.is_none());
        }
        service_input(&mut app, "also\rinvalid", true);
        assert_eq!(app.editor_preferences_sequence(), sequence);
        assert!(app.view.editor_workbench.error.is_none());
    }

    #[test]
    fn services_connect_uses_literal_executable_async_and_ignores_duplicate_requests() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            let (root, mut app) = services_fixture(mode);
            action(&mut app, "open:services");
            let sentinel = root.path().join("must-not-exist");
            let executable = format!("{}; touch {}", root.path().join("missing-server").display(), sentinel.display());
            service_input(&mut app, &executable, true);
            assert_eq!(app.editor_language_server_path(), executable);
            assert_eq!(app.editor_server_executable_preference(openide_ui::CodeLanguage::Rust).as_deref(), Some(executable.as_str()));
            assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Services));
            assert!(app.view.ide_status.service_connecting);
            let sequence = app.editor_preferences_sequence();
            assert!(sequence.is_some());
            action(&mut app, "server_connect");
            assert_eq!(app.editor_preferences_sequence(), sequence, "a duplicate must not restart or resubmit preferences");
            assert!(app.view.editor_workbench.error.is_none());

            let deadline = Instant::now() + Duration::from_secs(5);
            while app.editor_language_connecting() {
                app.poll_editor_language(&egui::Context::default());
                app.poll_ide_status();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            app.poll_ide_status();
            assert!(!app.view.ide_status.service_connecting);
            assert!(app.view.editor_workbench.language_status.contains("no disponible"));
            assert!(!sentinel.exists(), "an executable string is never interpreted by a shell");
            assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Services));
            assert!(app.view.editor_workbench.error.is_none());

            service_input(&mut app, "", true);
            assert!(app.editor_server_executable_preference(openide_ui::CodeLanguage::Rust).is_none());
            assert!(app.editor_language_server_path().is_empty());
        }
    }

    #[test]
    fn services_input_and_connect_reject_closed_stale_and_hidden_scopes() {
        let changes: &[(&str, fn(&mut AgentsWindow))] = &[
            ("dismissed", |app| { action(app, "services_dismiss"); }),
            ("other menu", |app| { action(app, "open:indent"); }),
            ("collapsed", |app| { app.dock.set_collapsed(&app.dock_scope(), true); app.sync_dock_view(); }),
            ("environment", |app| app.open_environment()),
            ("language", |app| app.view.editor_workbench.language_override = Some(openide_ui::CodeLanguage::Python)),
            ("workspace", |app| app.view.workspace.push_str("/other")),
            ("tab", |app| { app.dock.open(&app.dock_scope(), DockTabKind::Files); }),
            ("revision", |app| { app.active_editor().unwrap().2.execute(EditorCommand::InsertText("edit".into())).unwrap(); }),
        ];
        for (name, change) in changes {
            let (_root, mut app) = services_fixture(WindowMode::Agents);
            action(&mut app, "open:services");
            let prefix = ide_status_ui::document_scope(&app.view).unwrap().prefix();
            let before = app.view.editor_workbench.server_path.clone();
            let sequence = app.editor_preferences_sequence();
            change(&mut app);
            for event in [
                UiAction::InputChanged { id: format!("{prefix}:server_path"), value: "/wrong/scope".into() },
                UiAction::Submitted { id: format!("{prefix}:server_path"), value: "/wrong/scope".into() },
                UiAction::Clicked { id: format!("{prefix}:server_connect") },
            ] {
                assert!(app.apply_ide_status_action(&event, &egui::Context::default()), "{name}");
                assert_eq!(app.view.editor_workbench.server_path, before, "{name}");
                assert_eq!(app.editor_preferences_sequence(), sequence, "{name}");
            }
        }
    }

    #[test]
    fn services_persistence_errors_stay_in_current_popover_and_plain_cannot_connect() {
        let (root, mut app) = services_fixture(WindowMode::Agents);
        std::fs::create_dir_all(root.path().join("data/editor-preferences.json")).unwrap();
        action(&mut app, "open:services");
        service_input(&mut app, &root.path().join("missing-server").to_string_lossy(), true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.view.ide_status.service_error.is_none() {
            app.poll_editor_workbench(&egui::Context::default());
            app.poll_ide_status();
            assert!(Instant::now() < deadline, "missing scoped preference failure");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.view.editor_workbench.error.is_none());
        assert_eq!(app.view.ide_status.menu, Some(StatusMenu::Services));

        app.view.editor_workbench.language_override = Some(openide_ui::CodeLanguage::Plain);
        app.poll_ide_status();
        action(&mut app, "open:services");
        service_input(&mut app, "plain-server", true);
        assert_eq!(app.view.ide_status.service_error.as_deref(), Some("Elegí un lenguaje antes de conectar un servidor"));
        assert!(app.editor_server_executable_preference(openide_ui::CodeLanguage::Plain).is_none());
        assert!(app.view.editor_workbench.error.is_none());
    }
}
