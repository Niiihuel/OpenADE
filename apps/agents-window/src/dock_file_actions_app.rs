//! File-tab actions stay bound to the project, task and tab that opened a menu.
use super::*;
use openide_ui::Key;

impl AgentsWindow {
    fn file_tab_controls_visible(&self) -> bool {
        self.ide_workspace_active()
            && self.resource_pane_visible()
            && self.view.main_page == MainPage::Workspace
            && !self.view.show_settings
            && !self.ide_modal_panel_visible()
            && !self.view.environment.open
            && !self.view.show_command_center
            && self.view.composer_full_access_confirm.is_none()
    }

    fn focus_current_file_group(&mut self, ctx: &egui::Context) {
        if let Some(editor) = self
            .view
            .code_editor
            .as_ref()
            .filter(|view| view.controller.is_some())
        {
            self.editor_services.focus_next = Some(crate::code_editor_ui::view_id(editor.tab_id));
        } else if let Some(preview) = &self.view.markdown_preview {
            self.apply_markdown_preview_action(
                &UiAction::Clicked {
                    id: format!("{}:tab", preview.action_prefix()),
                },
                ctx,
            );
        }
    }

    pub(super) fn apply_dock_file_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        if id == "dock_reopen_last_file" {
            return self.handle_dock_file_key(
                &UiEvent::Key {
                    key: Key::T,
                    ctrl: true,
                    shift: true,
                },
                ctx,
            );
        }
        let Some(payload) = id.strip_prefix("dock_file_action:") else {
            return false;
        };
        let Ok((workspace, owner, raw, operation)) =
            serde_json::from_str::<(String, String, u64, String)>(payload)
        else {
            return true;
        };
        if !self.file_tab_controls_visible()
            || workspace != self.view.workspace
            || owner != self.view.selected_session.as_deref().unwrap_or("")
        {
            return true;
        }
        let scope = self.dock_scope();
        let Some(tab) = self
            .dock
            .right_tabs(&scope)
            .into_iter()
            .find(|tab| tab.id.get() == raw)
        else {
            return true;
        };
        let DockTabKind::FilePreview { path } = &tab.kind else {
            return true;
        };
        let path = path.clone();
        let tab_id = tab.id;
        match operation.as_str() {
            "save" => self.code_editor.save_for_path(
                Path::new(&workspace),
                &path,
                self.store
                    .as_ref()
                    .map(|store| store.storage_root().to_path_buf()),
            ),
            "copy_relative" => ctx.copy_text(path.to_string_lossy().into_owned()),
            "copy_absolute" => ctx.copy_text(
                Path::new(&workspace)
                    .join(&path)
                    .to_string_lossy()
                    .into_owned(),
            ),
            "reveal" => {
                if let Err(error) = self.reveal_editor_folder(&path) {
                    self.view.status = error;
                }
            }
            "preview" => {
                if openide_preview::kind_for_path(&path)
                    == Some(openide_preview::PreviewKind::Markdown)
                {
                    self.dock.activate(&scope, tab_id);
                    self.sync_dock_view();
                    self.open_markdown_preview(ctx);
                }
            }
            "close" | "close_others" | "close_all" => {
                let id = match operation.as_str() {
                    "close" => format!("dock_close:{raw}"),
                    "close_others" => format!("dock_close_others:{raw}"),
                    _ => "dock_close_all".into(),
                };
                self.apply_action(UiAction::Clicked { id }, ctx);
                self.focus_current_file_group(ctx);
            }
            _ => {}
        }
        ctx.request_repaint();
        true
    }

    pub(super) fn handle_dock_file_key(&mut self, event: &UiEvent, ctx: &egui::Context) -> bool {
        if !self.file_tab_controls_visible() {
            return false;
        }
        match event {
            UiEvent::Key {
                key: Key::W,
                ctrl: true,
                shift: false,
            } => {
                let focused = self.tree.focused_id().unwrap_or("");
                if let Some(tab) = self
                    .view
                    .markdown_preview
                    .as_ref()
                    .filter(|tab| focused.starts_with(&format!("{}:", tab.action_prefix())))
                {
                    return self.apply_markdown_preview_action(
                        &UiAction::Clicked {
                            id: format!("{}:close", tab.action_prefix()),
                        },
                        ctx,
                    );
                }
                if ![
                    "code_editor:",
                    "dock_tab:",
                    "editor_",
                    "document_preview_view:",
                    "document_preview:",
                ]
                .iter()
                .any(|prefix| focused.starts_with(prefix))
                {
                    return false;
                }
                let scope = self.dock_scope();
                let Some(tab) = self
                    .dock
                    .active_tab(&scope)
                    .filter(|tab| matches!(tab.kind, DockTabKind::FilePreview { .. }))
                else {
                    return false;
                };
                let id = crate::dock_tab_ui::file_action(&self.view, tab.id.get(), "close");
                self.apply_dock_file_action(&UiAction::Clicked { id }, ctx)
            }
            UiEvent::Key {
                key: Key::T,
                ctrl: true,
                shift: true,
            } => {
                let scope = self.dock_scope();
                let recent = self
                    .dock
                    .recently_closed(&scope)
                    .into_iter()
                    .find(|entry| matches!(entry.tab.kind, DockTabKind::FilePreview { .. }))
                    .map(|entry| entry.tab.id);
                if let Some(id) = recent {
                    self.apply_action(
                        UiAction::Clicked {
                            id: format!("dock_reopen:{}", id.get()),
                        },
                        ctx,
                    );
                    self.focus_current_file_group(ctx);
                }
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_editor::{EditKind, Selection};

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("first.md"), "# First\n").unwrap();
        std::fs::write(workspace.join("second.md"), "# Second\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace.display().to_string();
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.show_settings = false;
        app.view.main_page = MainPage::Workspace;
        app.dock = DockState::default();
        (root, app)
    }

    fn open(app: &mut AgentsWindow, path: &str) -> u64 {
        let scope = app.dock_scope();
        let id = app
            .dock
            .open(&scope, DockTabKind::FilePreview { path: path.into() });
        app.dock.set_collapsed(&scope, false);
        app.sync_dock_view();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app
            .view
            .code_editor
            .as_ref()
            .is_some_and(|editor| editor.loading)
        {
            app.poll_code_editor(&egui::Context::default());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        app.poll_editor_workbench(&egui::Context::default());
        id.get()
    }

    #[test]
    fn close_and_reopen_shortcuts_keep_dirty_buffer_and_do_not_capture_composer() {
        let (_root, mut app) = fixture();
        let tab = open(&mut app, "first.md");
        let controller = app
            .view
            .code_editor
            .as_ref()
            .unwrap()
            .controller
            .clone()
            .unwrap();
        let shared = controller.shared_buffer();
        let mut buffer = shared.lock().unwrap();
        buffer.set_selection(Selection::caret(2)).unwrap();
        buffer.insert("Draft ", EditKind::Paste, 1).unwrap();
        let before = buffer.snapshot();
        drop(buffer);
        let ctx = egui::Context::default();
        let close = UiEvent::Key {
            key: Key::W,
            ctrl: true,
            shift: false,
        };
        app.tree.set_root(Element::button("prompt", "Agente"));
        app.tree.layout(Size::new(600.0, 400.0));
        app.tree.focus("prompt");
        assert!(!app.handle_dock_file_key(&close, &ctx));
        app.tree.set_root(crate::code_editor_ui::build_ide(
            app.view.code_editor.as_ref().unwrap(),
            &app.view.editor_workbench,
            700.0,
            app.view.keymap_profile,
            app.view.vim_mode,
        ));
        app.tree.layout(Size::new(700.0, 500.0));
        assert!(app.tree.focus(&crate::code_editor_ui::view_id(tab)));
        assert!(app.handle_dock_file_key(&close, &ctx));
        assert!(app.dock.active_tab(&app.dock_scope()).is_none());
        assert!(app.handle_dock_file_key(
            &UiEvent::Key {
                key: Key::T,
                ctrl: true,
                shift: true
            },
            &ctx
        ));
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
        assert_eq!(
            std::fs::read_to_string(Path::new(&app.view.workspace).join("first.md")).unwrap(),
            "# First\n"
        );
    }

    #[test]
    fn context_actions_target_the_clicked_tab_and_reject_changed_task() {
        let (_root, mut app) = fixture();
        let first = open(&mut app, "first.md");
        let second = open(&mut app, "second.md");
        let close = UiAction::Clicked {
            id: crate::dock_tab_ui::file_action(&app.view, first, "close"),
        };
        let ctx = egui::Context::default();
        let previous = app.view.selected_session.clone();
        app.view.selected_session = Some("another-task".into());
        assert!(app.apply_dock_file_action(&close, &ctx));
        app.view.selected_session = previous;
        assert!(
            app.dock
                .right_tabs(&app.dock_scope())
                .iter()
                .any(|tab| tab.id.get() == first)
        );
        assert!(app.apply_dock_file_action(&close, &ctx));
        assert!(
            !app.dock
                .right_tabs(&app.dock_scope())
                .iter()
                .any(|tab| tab.id.get() == first)
        );
        assert_eq!(
            app.dock.active_tab(&app.dock_scope()).unwrap().id.get(),
            second
        );
    }
}
