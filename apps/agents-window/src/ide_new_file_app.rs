//! Workspace-scoped creation reuses the editor's guarded save-as worker.
use super::*;
use crate::ide_workspace::IdeNewFile;

impl AgentsWindow {
    pub(super) fn apply_ide_new_file_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        match action {
            UiAction::Clicked { id } if id == "ide_new_file" => {
                if !self.ide_workspace_active() {
                    return true;
                }
                self.view.ide.menu_open = None;
                self.view.ide.sessions_open = false;
                self.view.ide.compact_panel = None;
                self.view.editor_workbench.menu_open = false;
                self.close_composer_floating_controls();
                self.view.ide.new_file = Some(IdeNewFile {
                    workspace: self.view.workspace.clone(),
                    path: String::new(),
                    error: None,
                });
                self.tree.set_input_value("ide_new_file_path", "");
                ctx.request_repaint();
                return true;
            }
            UiAction::Clicked { id } if id == "ide_new_file_cancel" => {
                self.view.ide.new_file = None;
                ctx.request_repaint();
                return true;
            }
            UiAction::InputChanged { id, value } if id == "ide_new_file_path" => {
                if let Some(draft) = self.view.ide.new_file.as_mut() {
                    draft.path = value.clone();
                    draft.error = None;
                }
                return true;
            }
            UiAction::Submitted { id, value } if id == "ide_new_file_path" => {
                if let Some(draft) = self.view.ide.new_file.as_mut() {
                    draft.path = value.clone();
                }
            }
            UiAction::Clicked { id } if id == "ide_new_file_submit" => {}
            _ => return false,
        }
        if !self.ide_workspace_active() {
            return true;
        }
        let Some(draft) = self.view.ide.new_file.as_ref() else {
            return true;
        };
        if draft.workspace != self.view.workspace {
            self.view.ide.new_file.as_mut().unwrap().error =
                Some("El proyecto cambió. Volvé a abrir Nuevo archivo.".into());
            return true;
        }
        let relative = PathBuf::from(draft.path.trim());
        match self.code_editor_create(relative, String::new()) {
            Ok(()) => {
                self.view.ide.new_file = None;
                self.refresh_file_rows();
                if let Some(editor) = &self.view.code_editor {
                    self.editor_services.focus_next =
                        Some(crate::code_editor_ui::view_id(editor.tab_id));
                }
            }
            Err(error) => self.view.ide.new_file.as_mut().unwrap().error = Some(error),
        }
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pump(app: &mut AgentsWindow, done: impl Fn(&AgentsWindow) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while {
            app.poll_code_editor(&egui::Context::default());
            !done(app)
        } {
            assert!(Instant::now() < deadline, "editor worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.workspace_draft = app.view.workspace.clone();
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.viewport_width = 900.0;
        app.init_code_editor_recovery();
        // Creation queues behind recovery on the same editor worker. The
        // fixture starts with an empty store; the public pending-saves state
        // used by each test is sufficient to await the resulting file write.
        (root, app)
    }

    fn open(app: &mut AgentsWindow) {
        assert!(app.apply_ide_new_file_action(
            &UiAction::Clicked {
                id: "ide_new_file".into()
            },
            &egui::Context::default(),
        ));
    }

    fn submit(app: &mut AgentsWindow, path: &str) {
        assert!(app.apply_ide_new_file_action(
            &UiAction::Submitted {
                id: "ide_new_file_path".into(),
                value: path.into()
            },
            &egui::Context::default(),
        ));
    }

    #[test]
    fn ide_new_file_creates_the_first_file_and_opens_its_editor() {
        let (root, mut app) = fixture();
        assert!(app.view.code_editor.is_none());
        open(&mut app);
        submit(&mut app, "first.rs");
        assert!(app.view.ide.new_file.is_none());
        pump(&mut app, |app| !app.view.code_editor_pending_saves);
        assert_eq!(
            std::fs::read(root.path().join("project/first.rs")).unwrap(),
            b""
        );
        let editor = app.view.code_editor.as_ref().unwrap();
        assert_eq!(editor.path, "first.rs");
        assert_eq!(app.view.side_tab, SideTab::FilePreview);
        assert_eq!(
            app.editor_services.focus_next,
            Some(crate::code_editor_ui::view_id(editor.tab_id)),
        );
    }

    #[test]
    fn ide_new_file_rejects_invalid_paths_and_stale_workspaces() {
        let (root, mut app) = fixture();
        open(&mut app);
        for path in ["", "../outside.rs", "/tmp/absolute.rs"] {
            submit(&mut app, path);
            assert!(app.view.ide.new_file.as_ref().unwrap().error.is_some());
            assert!(app.view.code_editor.is_none());
        }
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        app.view.workspace = other.to_string_lossy().into_owned();
        submit(&mut app, "stale.rs");
        assert!(app.view.ide.new_file.as_ref().unwrap().error.is_some());
        assert!(!root.path().join("project/stale.rs").exists());
        assert!(!other.join("stale.rs").exists());
    }

    #[test]
    fn ide_new_file_never_overwrites_an_existing_file() {
        let (root, mut app) = fixture();
        let existing = root.path().join("project/existing.rs");
        std::fs::write(&existing, "keep this").unwrap();
        open(&mut app);
        submit(&mut app, "existing.rs");
        pump(&mut app, |app| !app.view.code_editor_pending_saves);
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep this");
        assert!(app.view.code_editor.as_ref().unwrap().error.is_some());
    }
}
