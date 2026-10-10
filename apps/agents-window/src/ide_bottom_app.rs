//! One bottom dock owns terminal, language diagnostics and debugger output.
//! Changing its presentation never terminates a PTY or debugger session.
use super::*;
use crate::ide_workspace::IdeBottomPanel;

impl AgentsWindow {
    pub(super) fn open_ide_bottom(&mut self, panel: IdeBottomPanel, ctx: &egui::Context) {
        let scope = self.dock_scope();
        let terminal = panel == IdeBottomPanel::Terminal;
        if terminal && self.dock.active_terminal(&scope).is_none() {
            self.dock.new_terminal(&scope);
        }
        self.dock.set_terminal_collapsed(&scope, !terminal);
        self.view.ide.bottom_panel = panel;
        self.view.ide.bottom_open = !terminal;
        self.close_terminal_controls();
        if panel == IdeBottomPanel::Problems
            && self.view.editor_workbench.results_title == "Problemas del archivo"
        {
            // Diagnostic results now belong to this dock. Preserve unrelated
            // definitions, references, completions and language hover content.
            self.view.editor_workbench.results.clear();
            self.view.editor_workbench.hover = None;
            self.view.editor_workbench.result_document = None;
        }
        self.sync_dock_view();
        self.terminal_focus_pending = terminal;
        self.persist_ide_workspace();
        ctx.request_repaint();
    }

    fn hide_ide_bottom(&mut self) {
        let scope = self.dock_scope();
        self.dock.set_terminal_collapsed(&scope, true);
        self.view.ide.bottom_open = false;
        self.close_terminal_controls();
        self.sync_dock_view();
        self.terminal_focus_pending = false;
        self.editor_services.focus_next = None;
        if let Some(editor) = self.view.code_editor.as_ref().filter(|_| self.document_status_visible()) {
            self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(editor.tab_id));
        } else if !self.ide_workspace_active() {
            self.tree.focus("prompt");
        }
        self.persist_ide_workspace();
    }

    pub(super) fn apply_ide_bottom_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        if self.view.main_page != MainPage::Workspace || self.view.show_settings {
            return false;
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if id == "ide_bottom_toggle" {
            if self.view.ide.bottom_open || self.view.show_terminal_panel {
                self.hide_ide_bottom();
            } else {
                self.open_ide_bottom(self.view.ide.bottom_panel, ctx);
            }
            return true;
        }
        if matches!(id.as_str(), "ide_bottom_hide" | "terminal_panel_hide") {
            self.hide_ide_bottom();
            return true;
        }
        // Commands that create/reveal a terminal must select its surface too.
        if id == "terminal_toggle"
            || id == "terminal_new"
            || id.starts_with("terminal_new_profile:")
        {
            self.view.ide.bottom_panel = IdeBottomPanel::Terminal;
            self.view.ide.bottom_open = false;
            return false;
        }
        let panel = match id.as_str() {
            "ide_bottom:terminal" => Some(IdeBottomPanel::Terminal),
            "ide_bottom:problems" | "editor_diagnostics" => Some(IdeBottomPanel::Problems),
            "ide_bottom:output" => Some(IdeBottomPanel::Output),
            "ide_bottom:console" => Some(IdeBottomPanel::DebugConsole),
            _ => None,
        };
        if let Some(panel) = panel {
            self.open_ide_bottom(panel, ctx);
            return true;
        }
        if let Some(data) = id.strip_prefix("ide_diagnostic:") {
            if !self.view.ide.bottom_open
                || self.view.ide.bottom_panel != IdeBottomPanel::Problems
                || self.view.show_command_center
            {
                return true;
            }
            let Ok((workspace, document, revision, index)) =
                serde_json::from_str::<(String, u64, u64, usize)>(data)
            else {
                return true;
            };
            if workspace != self.view.workspace || !self.view.editor_workbench.diagnostics_current {
                return true;
            }
            let Some((tab, path, _, snapshot)) = self.active_editor() else {
                return true;
            };
            let scope = self.dock_scope();
            if self.view.side_tab != SideTab::FilePreview
                || self.view.code_editor.as_ref().is_none_or(|editor| editor.loading)
                || !self.dock.active_tab(&scope).is_some_and(|active| {
                    active.id.get() == tab
                        && matches!(&active.kind, DockTabKind::FilePreview { path: active_path }
                            if active_path == Path::new(&path))
                })
            {
                return true;
            }
            if (snapshot.document_id(), snapshot.revision()) != (document, revision)
                || self.view.editor_workbench.diagnostic_revision != Some((document, revision))
                || index >= self.view.editor_workbench.diagnostics.len().min(80)
            {
                return true;
            }
            let position = self.view.editor_workbench.diagnostics[index].range.start;
            if let Some(offset) = openide_language::utf16_to_offset(&snapshot, position) {
                self.view.show_dock_overview = false;
                self.reveal_document_preview_source();
                if !self.ide_workspace_active() && !self.view.show_side_pane {
                    self.dock.set_collapsed(&scope, false);
                    self.sync_dock_view();
                }
                if let Err(error) = self.editor_command(
                    openide_editor::EditorCommand::SetSelection(openide_editor::Selection::caret(
                        offset,
                    )),
                    ctx,
                ) {
                    self.view.status = error;
                }
                self.view.ide.extensions_open = false;
                self.editor_services.focus_next = self
                    .document_status_visible()
                    .then(|| crate::code_editor_ui::view_id(tab));
                if self.editor_services.focus_next.is_none() && !self.ide_workspace_active() {
                    self.tree.focus("prompt");
                }
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide_workspace::WindowMode;

    fn diagnostic_fixture_in_mode(mode: WindowMode) -> (tempfile::TempDir, AgentsWindow, DockTabId) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("unicode.txt"), "first\r\n😀 xx\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.ide.mode = mode;
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.viewport_width = 1600.0;
        app.viewport_height = 900.0;
        let scope = app.dock_scope();
        let tab = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "unicode.txt".into(),
            },
        );
        app.dock.set_collapsed(&scope, false);
        app.sync_dock_view();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app
            .view
            .code_editor
            .as_ref()
            .is_none_or(|editor| editor.loading)
        {
            app.poll_code_editor(&egui::Context::default());
            assert!(Instant::now() < deadline, "diagnostic fixture did not load");
            std::thread::sleep(Duration::from_millis(2));
        }
        let (_, _, _, snapshot) = app.active_editor().unwrap();
        app.view.editor_workbench.diagnostic_revision =
            Some((snapshot.document_id(), snapshot.revision()));
        app.view.editor_workbench.diagnostics_current = true;
        app.view.editor_workbench.diagnostics = vec![openide_language::lsp::Diagnostic {
            range: openide_language::Range {
                start: openide_language::Position {
                    line: 1,
                    character: 3,
                },
                end: openide_language::Position {
                    line: 1,
                    character: 5,
                },
            },
            severity: Some(1),
            code: None,
            source: Some("fixture".into()),
            message: "Example diagnostic".into(),
        }];
        app.open_ide_bottom(IdeBottomPanel::Problems, &egui::Context::default());
        app.editor_services.focus_next = None;
        (root, app, tab)
    }

    fn diagnostic_action(workspace: &str, document: u64, revision: u64) -> UiAction {
        UiAction::Clicked {
            id: format!(
                "ide_diagnostic:{}",
                serde_json::to_string(&(workspace, document, revision, 0)).unwrap()
            ),
        }
    }

    #[test]
    fn ide_bottom_diagnostic_moves_utf16_caret_without_reopening_or_deferring() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_diagnostic_navigation(mode);
        }
    }

    fn check_diagnostic_navigation(mode: WindowMode) {
        let (_root, mut app, tab) = diagnostic_fixture_in_mode(mode);
        let scope = app.dock_scope();
        let tabs_before = app
            .dock
            .right_tabs(&scope)
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        let (_, _, _, before) = app.active_editor().unwrap();
        let action =
            diagnostic_action(&app.view.workspace, before.document_id(), before.revision());
        app.view.ide.extensions_open = true;
        assert!(app.apply_ide_bottom_action(&action, &egui::Context::default()));
        let (_, _, _, after) = app.active_editor().unwrap();
        assert_eq!(after.document_id(), before.document_id());
        assert_eq!(after.revision(), before.revision());
        assert_eq!(after.selection(), openide_editor::Selection::caret(9));
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, tab);
        assert_eq!(
            app.dock
                .right_tabs(&scope)
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            tabs_before
        );
        assert!(app.editor_services.pending_navigation.is_none());
        assert_eq!(
            app.editor_services.focus_next,
            Some(crate::code_editor_ui::view_id(tab.get()))
        );
        assert!(!app.view.ide.extensions_open);
    }

    #[test]
    fn ide_bottom_diagnostic_rejects_stale_scope_document_and_revision() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_stale_diagnostic(mode);
        }
    }

    fn check_stale_diagnostic(mode: WindowMode) {
        let (_root, mut app, tab) = diagnostic_fixture_in_mode(mode);
        let (_, _, _, snapshot) = app.active_editor().unwrap();
        let document = snapshot.document_id();
        let revision = snapshot.revision();
        let actions = [
            diagnostic_action("/different-project", document, revision),
            diagnostic_action(&app.view.workspace, document + 1, revision),
            diagnostic_action(&app.view.workspace, document, revision + 1),
        ];
        for action in actions {
            assert!(app.apply_ide_bottom_action(&action, &egui::Context::default()));
            assert_eq!(
                app.active_editor().unwrap().3.selection(),
                snapshot.selection()
            );
            assert_eq!(app.dock.active_tab(&app.dock_scope()).unwrap().id, tab);
            assert!(app.editor_services.pending_navigation.is_none());
            assert!(app.editor_services.focus_next.is_none());
        }
        // The click carried valid identifiers, but the document changed since
        // those diagnostics were produced. The cached UI must remain inert.
        let action = diagnostic_action(&app.view.workspace, document, revision);
        let controller = app.active_editor().unwrap().2;
        controller
            .shared_buffer()
            .lock()
            .unwrap()
            .insert("changed ", openide_editor::EditKind::Paste, 1)
            .unwrap();
        let changed_selection = controller.snapshot().unwrap().selection();
        app.apply_ide_bottom_action(&action, &egui::Context::default());
        assert_eq!(
            controller.snapshot().unwrap().selection(),
            changed_selection
        );
        assert!(app.editor_services.pending_navigation.is_none());
    }

    #[test]
    fn ide_bottom_switch_retains_terminal_identity_and_editor() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            check_retained_terminal(mode);
        }
    }

    fn check_retained_terminal(mode: WindowMode) {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().into());
        app.view.ide.mode = mode;
        let scope = app.dock_scope();
        let terminal = app.dock.new_terminal(&scope);
        let file = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: "file.rs".into(),
            },
        );
        app.sync_dock_view();
        let ctx = egui::Context::default();
        app.open_ide_bottom(IdeBottomPanel::Problems, &ctx);
        assert!(app.view.ide.bottom_open);
        assert!(!app.view.show_terminal_panel);
        assert_eq!(app.dock.active_terminal(&scope).unwrap().id, terminal);
        assert_eq!(app.dock.active_tab(&scope).unwrap().id, file);
        app.open_ide_bottom(IdeBottomPanel::Terminal, &ctx);
        assert!(app.view.show_terminal_panel);
        assert_eq!(app.dock.active_terminal(&scope).unwrap().id, terminal);
        app.hide_ide_bottom();
        assert!(!app.view.show_terminal_panel && !app.view.ide.bottom_open);
        assert_eq!(app.dock.active_terminal(&scope).unwrap().id, terminal);
    }

    #[test]
    fn problems_actions_open_in_both_modes_without_terminal_creation_or_duplicate_results() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            let (_root, mut app, _) = diagnostic_fixture_in_mode(mode);
            app.hide_ide_bottom();
            let snapshot = app.active_editor().unwrap().3;
            app.view.editor_workbench.results_title = "Problemas del archivo".into();
            app.view.editor_workbench.hover = Some("Problemas anteriores".into());
            app.view.editor_workbench.result_document = Some((snapshot.document_id(), snapshot.revision()));
            app.view.editor_workbench.results = vec![crate::editor_workbench_ui::LanguageResult {
                label: "Diagnóstico anterior".into(),
                detail: String::new(),
                value: serde_json::Value::Null,
            }];
            app.view.terminal_controls.sessions_open = true;
            app.view.terminal_controls.profiles_open = true;
            app.view.terminal_menu_open = true;
            let ctx = egui::Context::default();
            app.apply_action(UiAction::Clicked { id: "editor_diagnostics".into() }, &ctx);
            assert!(app.view.ide.bottom_open);
            assert_eq!(app.view.ide.bottom_panel, IdeBottomPanel::Problems);
            assert!(!app.view.show_terminal_panel);
            assert!(app.dock.terminal_tabs(&app.dock_scope()).is_empty());
            assert!(app.terminals.is_empty(), "opening diagnostics must not spawn a shell");
            assert!(!app.terminal_focus_pending);
            assert!(!app.view.terminal_controls.sessions_open);
            assert!(!app.view.terminal_controls.profiles_open);
            assert!(!app.view.terminal_menu_open);
            assert!(app.view.editor_workbench.results.is_empty());
            assert!(app.view.editor_workbench.hover.is_none());
            assert!(app.view.editor_workbench.result_document.is_none());

            // Switching to the lower Problems tab must not consume a separate
            // definitions or completion request in the editor tools.
            app.view.editor_workbench.results_title = "Definiciones".into();
            app.view.editor_workbench.hover = Some("Descripción del símbolo".into());
            app.apply_action(UiAction::Clicked { id: "ide_bottom:problems".into() }, &ctx);
            assert_eq!(app.view.editor_workbench.hover.as_deref(), Some("Descripción del símbolo"));
            app.apply_action(UiAction::Clicked { id: "ide_bottom_toggle".into() }, &ctx);
            assert!(!app.view.ide.bottom_open);
            app.apply_action(UiAction::Clicked { id: "ide_bottom_toggle".into() }, &ctx);
            assert!(app.view.ide.bottom_open);
            assert_eq!(app.view.ide.bottom_panel, IdeBottomPanel::Problems);
            assert!(app.terminals.is_empty());
        }
    }

    #[test]
    fn problems_navigation_rejects_closed_panel_and_changed_active_tab() {
        for mode in [WindowMode::Ide, WindowMode::Agents] {
            let (_root, mut app, _) = diagnostic_fixture_in_mode(mode);
            let snapshot = app.active_editor().unwrap().3;
            let action = diagnostic_action(&app.view.workspace, snapshot.document_id(), snapshot.revision());
            app.hide_ide_bottom();
            app.editor_services.focus_next = None;
            app.apply_ide_bottom_action(&action, &egui::Context::default());
            assert_eq!(app.active_editor().unwrap().3.selection(), snapshot.selection());
            assert!(app.editor_services.focus_next.is_none());

            app.open_ide_bottom(IdeBottomPanel::Problems, &egui::Context::default());
            // Simulate a queued click while the view still holds the previous
            // editor: DockState is authoritative, even before sync_dock_view.
            app.dock.open(&app.dock_scope(), DockTabKind::Files);
            app.apply_ide_bottom_action(&action, &egui::Context::default());
            assert_eq!(app.active_editor().unwrap().3.selection(), snapshot.selection());
            assert!(app.editor_services.focus_next.is_none());
        }
    }

    #[test]
    fn agents_problems_restore_hidden_editor_for_navigation_and_close_to_visible_focus() {
        let (_root, mut app, tab) = diagnostic_fixture_in_mode(WindowMode::Agents);
        let snapshot = app.active_editor().unwrap().3;
        let action = diagnostic_action(&app.view.workspace, snapshot.document_id(), snapshot.revision());
        app.dock.set_collapsed(&app.dock_scope(), true);
        app.sync_dock_view();
        assert!(!app.view.show_side_pane);
        app.view.show_dock_overview = true;
        app.apply_ide_bottom_action(&action, &egui::Context::default());
        assert!(app.view.show_side_pane);
        assert!(!app.view.show_dock_overview);
        assert_eq!(app.active_editor().unwrap().3.selection(), openide_editor::Selection::caret(9));
        assert_eq!(app.editor_services.focus_next, Some(crate::code_editor_ui::view_id(tab.get())));
        app.hide_ide_bottom();
        assert_eq!(app.editor_services.focus_next, Some(crate::code_editor_ui::view_id(tab.get())));

        app.open_ide_bottom(IdeBottomPanel::Problems, &egui::Context::default());
        app.dock.set_collapsed(&app.dock_scope(), true);
        app.sync_dock_view();
        app.tree.set_root(crate::view::build_window_view(&app.view, 1600.0, 900.0));
        app.tree.layout(Size::new(1600.0, 900.0));
        app.hide_ide_bottom();
        assert!(app.editor_services.focus_next.is_none());
        assert_eq!(app.tree.focused_id(), Some("prompt"));
        assert!(app.terminals.is_empty());
    }
}
