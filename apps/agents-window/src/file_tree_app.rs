//! The file tree's menu. Every action names its target by workspace-relative
//! path and is checked against the tree again when it runs.
use super::*;

impl AgentsWindow {
    pub(super) fn apply_file_tree_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let UiAction::Clicked { id } = action else {
            return false;
        };
        let Some((operation, path)) = id
            .strip_prefix("file_tree:")
            .and_then(|rest| rest.split_once(':'))
        else {
            return false;
        };
        let relative = PathBuf::from(path);
        // The root is the empty path; anything else must still be a row.
        let row = self.file_explorer.as_ref().and_then(|explorer| {
            explorer
                .rows()
                .iter()
                .position(|row| row.relative == relative)
        });
        if !path.is_empty() && row.is_none() {
            self.view.status = "Ese archivo ya no está en el árbol".into();
            return true;
        }
        let workspace = PathBuf::from(&self.view.workspace);
        match (operation, row) {
            ("open" | "toggle" | "preview", Some(index)) => {
                self.apply_action(
                    UiAction::Clicked {
                        id: format!("file_row:{index}"),
                    },
                    ctx,
                );
                if operation == "preview" {
                    self.open_markdown_preview(ctx);
                }
            }
            ("copy_relative", Some(_)) => ctx.copy_text(path.to_owned()),
            ("copy_path", _) => ctx.copy_text(
                // Joining the empty root path would leave a trailing separator.
                row.map_or(workspace.clone(), |_| workspace.join(&relative))
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("mention", Some(_)) => self.mention_in_composer(&workspace, &relative),
            ("collapse_all", _) => {
                if let Some(explorer) = &mut self.file_explorer {
                    explorer.collapse_all();
                }
                self.refresh_file_rows();
            }
            ("refresh", _) => self.apply_action(
                UiAction::Clicked {
                    id: "files_refresh".into(),
                },
                ctx,
            ),
            _ => {}
        }
        ctx.request_repaint();
        true
    }

    /// The same reference and attachment the composer's `@` suggestion adds.
    fn mention_in_composer(&mut self, workspace: &Path, relative: &Path) {
        let file = match composer_attachment_for_path(workspace, relative) {
            Ok(file) => file,
            Err(error) => {
                self.view.composer_error = Some(error);
                return;
            }
        };
        let attached = self
            .view
            .composer_attachments
            .iter()
            .any(|existing| existing.path == file.path);
        if !attached && self.view.composer_attachments.len() >= MAX_COMPOSER_ATTACHMENTS {
            self.view.composer_error = Some("Se alcanzó el límite de adjuntos del mensaje".into());
            return;
        }
        let mut prompt = self.view.prompt.clone();
        if !prompt.is_empty() && !prompt.ends_with(char::is_whitespace) {
            prompt.push(' ');
        }
        prompt.push_str(&format!(
            "@{} ",
            serde_json::to_string(&file.path).unwrap_or_default()
        ));
        self.view.prompt = prompt.clone();
        self.tree.set_input_value("prompt", prompt);
        if !attached {
            self.view.composer_attachments.push(file);
            self.attachment_drafts
                .insert(self.draft_key(), self.view.composer_attachments.clone());
        }
        self.draft_dirty_at = Some(Instant::now());
        self.prompt_focus_pending = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(project.join("src/deep")).unwrap();
        std::fs::write(project.join("src/deep/mod.rs"), "x").unwrap();
        std::fs::write(project.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(project.join("README.md"), "# Hola").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.change_workspace(
            project
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        app.refresh_file_rows();
        (root, app)
    }

    /// Runs one menu action and returns what it put on the clipboard.
    pub(super) fn act(app: &mut AgentsWindow, operation: &str, path: &str) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let action = UiAction::Clicked {
                id: crate::files_panel_ui::tree_action(operation, path),
            };
            assert!(app.apply_file_tree_action(&action, ui.ctx()));
        });
        output.textures_delta.clear();
        output
            .platform_output
            .commands
            .into_iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    pub(super) fn names(app: &AgentsWindow) -> Vec<&str> {
        app.view
            .file_rows
            .iter()
            .map(|row| row.name.as_str())
            .collect()
    }

    #[test]
    fn menu_actions_act_on_the_path_they_name_even_after_the_rows_move() {
        let (_root, mut app) = fixture();
        act(&mut app, "toggle", "src");
        assert_eq!(names(&app), ["src", "deep", "main.rs", "README.md"]);
        // A new first entry shifts every row; the path still finds its folder.
        std::fs::create_dir(Path::new(&app.view.workspace).join("aaa")).unwrap();
        app.file_explorer.as_mut().unwrap().refresh().unwrap();
        app.refresh_file_rows();
        act(&mut app, "toggle", "src/deep");
        assert_eq!(
            names(&app),
            ["aaa", "src", "deep", "mod.rs", "main.rs", "README.md"]
        );
        act(&mut app, "collapse_all", "");
        assert_eq!(names(&app), ["aaa", "src", "README.md"]);
        act(&mut app, "refresh", "");
        assert_eq!(names(&app), ["aaa", "src", "README.md"]);
    }

    #[test]
    fn a_path_that_left_the_tree_does_nothing_but_say_so() {
        let (_root, mut app) = fixture();
        // `src` is collapsed, so its file is not a row either.
        for path in ["gone.rs", "../outside", "/etc/passwd", "src/main.rs"] {
            for operation in ["open", "toggle", "mention", "copy_path", "copy_relative"] {
                assert!(
                    act(&mut app, operation, path).is_empty(),
                    "{operation} {path}"
                );
            }
        }
        assert_eq!(app.view.status, "Ese archivo ya no está en el árbol");
        assert_eq!(names(&app), ["src", "README.md"]);
        assert!(app.view.prompt.is_empty() && app.view.composer_attachments.is_empty());
    }

    #[test]
    fn paths_are_copied_absolute_and_relative_and_the_root_has_no_trailing_separator() {
        let (_root, mut app) = fixture();
        act(&mut app, "toggle", "src");
        let workspace = app.view.workspace.clone();
        assert_eq!(
            act(&mut app, "copy_relative", "src/main.rs"),
            ["src/main.rs"]
        );
        assert_eq!(
            act(&mut app, "copy_path", "src/main.rs"),
            [format!("{workspace}/src/main.rs")]
        );
        assert_eq!(act(&mut app, "copy_path", ""), [workspace]);
    }

    #[test]
    fn open_opens_the_file_the_menu_named() {
        let (_root, mut app) = fixture();
        act(&mut app, "open", "README.md");
        assert_eq!(app.view.side_tab, SideTab::FilePreview);
        let scope = app.dock_scope();
        assert_eq!(
            app.dock.active_tab(&scope).map(|tab| tab.kind.clone()),
            Some(DockTabKind::FilePreview {
                path: "README.md".into(),
            })
        );
    }

    #[test]
    fn mention_references_the_path_once_and_respects_the_attachment_limit() {
        let (_root, mut app) = fixture();
        app.view.prompt = "Mirá".into();
        act(&mut app, "mention", "README.md");
        assert_eq!(app.view.prompt, "Mirá @\"README.md\" ");
        assert_eq!(app.view.composer_attachments.len(), 1);
        assert!(app.prompt_focus_pending);
        act(&mut app, "mention", "src");
        assert_eq!(app.view.composer_attachments[1].path, "src");
        // Mentioning again adds the reference, not a second attachment.
        act(&mut app, "mention", "src");
        assert_eq!(app.view.composer_attachments.len(), 2);
        let filler = app.view.composer_attachments[0].clone();
        app.view.composer_attachments = (0..MAX_COMPOSER_ATTACHMENTS)
            .map(|index| ComposerAttachmentView {
                path: format!("other-{index}"),
                ..filler.clone()
            })
            .collect();
        let prompt = app.view.prompt.clone();
        act(&mut app, "mention", "README.md");
        assert_eq!(app.view.prompt, prompt);
        assert_eq!(
            app.view.composer_attachments.len(),
            MAX_COMPOSER_ATTACHMENTS
        );
        assert!(app
            .view
            .composer_error
            .as_deref()
            .unwrap()
            .contains("límite"));
    }
}
