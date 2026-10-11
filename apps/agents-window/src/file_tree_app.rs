//! The file tree's menu. Every action names its target by workspace-relative
//! path and is checked against the tree again when it runs.
use super::*;
use crate::file_ops::{self, Failure, Op};
use crate::view::{FileDraftKind, FileDraftView};

/// What a finished operation hands back: the workspace it ran in, the
/// operation, and the relative paths it created or why it stopped.
pub(super) type FileOpDone = (PathBuf, Op, Result<Vec<PathBuf>, Failure>);

impl AgentsWindow {
    pub(super) fn apply_file_tree_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        match action {
            UiAction::InputChanged { id, value } if id == "file_tree_draft" => {
                if let Some(draft) = &mut self.view.file_draft {
                    draft.value = value.clone();
                    draft.error = None;
                }
                return true;
            }
            UiAction::Submitted { id, value } if id == "file_tree_draft" => {
                self.commit_file_draft(value, ctx);
                return true;
            }
            _ => {}
        }
        let UiAction::Clicked { id } = action else {
            return false;
        };
        match id.as_str() {
            "file_tree_delete_cancel" => {
                self.view.file_delete = None;
                return true;
            }
            "file_tree_delete_confirm" => {
                if let Some((path, _)) = self.view.file_delete.take() {
                    // The dialog named this path; it must still be in the tree.
                    if self.file_tree_row(Path::new(&path)).is_some() {
                        self.start_file_op(Op::Trash { path: path.into() }, ctx);
                    } else {
                        self.view.status = "Ese archivo ya no está en el árbol".into();
                    }
                }
                return true;
            }
            _ => {}
        }
        let Some((operation, path)) = id
            .strip_prefix("file_tree:")
            .and_then(|rest| rest.split_once(':'))
        else {
            return false;
        };
        let relative = PathBuf::from(path);
        // The root is the empty path; anything else must still be a row.
        let row = self.file_tree_row(&relative);
        let is_dir = row.map_or(true, |(_, is_dir)| is_dir);
        let row = row.map(|(index, _)| index);
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
            ("new_file" | "new_folder", _) => {
                // Beside a file, inside a folder or the root.
                let folder = if is_dir {
                    relative
                } else {
                    relative.parent().map(Path::to_owned).unwrap_or_default()
                };
                let kind = if operation == "new_file" {
                    FileDraftKind::NewFile
                } else {
                    FileDraftKind::NewFolder
                };
                self.open_file_draft(kind, &folder);
            }
            ("rename", Some(_)) => self.open_file_draft(FileDraftKind::Rename, &relative),
            ("delete", Some(_)) => self.view.file_delete = Some((path.to_owned(), is_dir)),
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

    /// The row index of `relative` in the tree as it is now, and whether it
    /// is a folder.
    fn file_tree_row(&self, relative: &Path) -> Option<(usize, bool)> {
        let rows = self.file_explorer.as_ref()?.rows();
        let index = rows.iter().position(|row| row.relative == relative)?;
        Some((index, rows[index].is_dir))
    }

    /// Puts the name field in the tree: first in `target` for a new entry,
    /// in place of `target` for a rename.
    fn open_file_draft(&mut self, kind: FileDraftKind, target: &Path) {
        let rename = kind == FileDraftKind::Rename;
        let row = self.file_tree_row(target).map(|(index, _)| index);
        if !rename && row.is_some() {
            // The new row is a child: its folder has to be open to show it.
            let expanded = self
                .file_explorer
                .as_ref()
                .is_some_and(|explorer| explorer.rows()[row.unwrap()].expanded);
            if !expanded {
                let opened = self
                    .file_explorer
                    .as_mut()
                    .map(|explorer| explorer.toggle(target));
                self.refresh_file_rows();
                if let Some(Err(error)) = opened {
                    self.view.files_status = format!("No se pudo abrir carpeta: {error}");
                    return;
                }
            }
        }
        let value = if rename {
            target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.tree.set_input_value("file_tree_draft", value.clone());
        self.view.file_draft = Some(FileDraftView {
            target: target.to_string_lossy().replace('\\', "/"),
            kind,
            value,
            error: None,
        });
        self.file_draft_focus_pending = true;
        // The row is one of a virtualized list: it has to be in view to take focus.
        let index = row.map_or(0, |index| index + usize::from(!rename));
        self.tree.scroll_to_index("file_tree", index);
    }

    fn commit_file_draft(&mut self, value: &str, ctx: &egui::Context) {
        let Some(draft) = &mut self.view.file_draft else {
            return;
        };
        let name = value.trim().to_owned();
        draft.value = name.clone();
        let target = PathBuf::from(&draft.target);
        let op = match draft.kind {
            FileDraftKind::NewFile => Op::CreateFile {
                parent: target,
                name,
            },
            FileDraftKind::NewFolder => Op::CreateDir {
                parent: target,
                name,
            },
            FileDraftKind::Rename => {
                if target.file_name().is_some_and(|current| current == name.as_str()) {
                    // The same name is not a rename.
                    self.view.file_draft = None;
                    return;
                }
                Op::Rename { path: target, name }
            }
        };
        let (Op::CreateFile { name, .. } | Op::CreateDir { name, .. } | Op::Rename { name, .. }) =
            &op
        else {
            return;
        };
        let refusal = file_ops::valid_name(name).err().or_else(|| {
            let Op::Rename { path, .. } = &op else {
                return None;
            };
            // The editor keeps a document by its path and cannot follow a move.
            self.code_editor
                .has_unsaved_under(Path::new(&self.view.workspace), path)
                .then(|| "Guarda los cambios abiertos antes de renombrar".to_owned())
        });
        match refusal {
            Some(error) => {
                if let Some(draft) = &mut self.view.file_draft {
                    draft.error = Some(error);
                }
            }
            None => self.start_file_op(op, ctx),
        }
    }

    /// Runs `op` off the UI thread; `poll_file_op` collects its result.
    fn start_file_op(&mut self, op: Op, ctx: &egui::Context) {
        if self.file_op.is_some() {
            self.view.status = "Espera a que termine la operación de archivos en curso".into();
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let (root, trash, ctx) = (PathBuf::from(&self.view.workspace), self.file_trash, ctx.clone());
        std::thread::spawn(move || {
            let result = file_ops::run(&root, &op, trash);
            let _ = sender.send((root, op, result));
            ctx.request_repaint();
        });
        self.file_op = Some(receiver);
    }

    pub(super) fn poll_file_op(&mut self, ctx: &egui::Context) -> bool {
        let Some(receiver) = &self.file_op else {
            return false;
        };
        let (root, op, result) = match receiver.try_recv() {
            Ok(done) => done,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.file_op = None;
                self.view.status = "La operación de archivos se interrumpió".into();
                return true;
            }
        };
        self.file_op = None;
        // The project changed meanwhile: there is no tree left to report to.
        if root != Path::new(&self.view.workspace) {
            return true;
        }
        if let Some(explorer) = &mut self.file_explorer {
            let _ = explorer.refresh();
        }
        self.refresh_file_rows();
        let named = !matches!(op, Op::Trash { .. });
        match result {
            Ok(created) => {
                if named {
                    self.view.file_draft = None;
                }
                let row = created.first().and_then(|path| self.file_tree_row(path));
                if let (Op::CreateFile { .. }, Some((index, _))) = (&op, row) {
                    self.apply_action(
                        UiAction::Clicked {
                            id: format!("file_row:{index}"),
                        },
                        ctx,
                    );
                }
            }
            // A name that failed stays on its row to be corrected.
            Err(failure) => match &mut self.view.file_draft {
                Some(draft) if named => {
                    draft.error = Some(failure.message);
                    self.file_draft_focus_pending = true;
                }
                _ => self.view.status = failure.message,
            },
        }
        true
    }

    pub(super) fn cancel_file_draft(&mut self) {
        self.view.file_draft = None;
        self.file_draft_focus_pending = false;
    }

    pub(super) fn focus_file_draft_if_pending(&mut self) {
        let Some(draft) = &self.view.file_draft else {
            self.file_draft_focus_pending = false;
            return;
        };
        if self.file_draft_focus_pending {
            if self.tree.focus("file_tree_draft") {
                self.file_draft_focus_pending = false;
            }
            return;
        }
        // Left blank or unchanged, the field goes away with its focus.
        let name = draft.value.trim();
        let untouched = name.is_empty()
            || draft.kind == FileDraftKind::Rename
                && Path::new(&draft.target)
                    .file_name()
                    .is_some_and(|current| current == name);
        if untouched && self.file_op.is_none() && self.tree.focused_id() != Some("file_tree_draft")
        {
            self.view.file_draft = None;
        }
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

    fn clicked(app: &mut AgentsWindow, id: &str) {
        let action = UiAction::Clicked { id: id.into() };
        assert!(app.apply_file_tree_action(&action, &egui::Context::default()));
    }

    /// Waits for the operation running off the UI thread, if any.
    pub(super) fn pump(app: &mut AgentsWindow) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while app.file_op.is_some() {
            app.poll_file_op(&egui::Context::default());
            assert!(Instant::now() < deadline, "file operation timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn typed(app: &mut AgentsWindow, value: &str) {
        let action = UiAction::InputChanged {
            id: "file_tree_draft".into(),
            value: value.into(),
        };
        assert!(app.apply_file_tree_action(&action, &egui::Context::default()));
    }

    /// Enter in the name field, and the operation it starts.
    fn submit(app: &mut AgentsWindow, value: &str) {
        let action = UiAction::Submitted {
            id: "file_tree_draft".into(),
            value: value.into(),
        };
        assert!(app.apply_file_tree_action(&action, &egui::Context::default()));
        pump(app);
    }

    fn draft(app: &AgentsWindow) -> Option<(&str, FileDraftKind, &str)> {
        let draft = app.view.file_draft.as_ref()?;
        Some((&draft.target, draft.kind, &draft.value))
    }

    fn draft_error(app: &AgentsWindow) -> &str {
        app.view
            .file_draft
            .as_ref()
            .and_then(|draft| draft.error.as_deref())
            .unwrap_or("")
    }

    #[test]
    fn new_entries_are_named_in_a_row_of_their_folder_and_never_replace_anything() {
        let (_root, mut app) = fixture();
        let project = PathBuf::from(&app.view.workspace);
        // On a collapsed folder: it opens, and the field goes inside it.
        act(&mut app, "new_file", "src");
        assert_eq!(names(&app), ["src", "deep", "main.rs", "README.md"]);
        assert_eq!(draft(&app), Some(("src", FileDraftKind::NewFile, "")));
        assert!(app.file_draft_focus_pending);
        submit(&mut app, "  lib.rs ");
        assert_eq!(draft(&app), None);
        assert_eq!(std::fs::read_to_string(project.join("src/lib.rs")).unwrap(), "");
        assert_eq!(names(&app), ["src", "deep", "lib.rs", "main.rs", "README.md"]);
        let scope = app.dock_scope();
        assert_eq!(
            app.dock.active_tab(&scope).map(|tab| tab.kind.clone()),
            Some(DockTabKind::FilePreview {
                path: "src/lib.rs".into(),
            }),
            "a new file opens"
        );
        // On a file: beside it. On the root: at the top level.
        act(&mut app, "new_folder", "src/main.rs");
        assert_eq!(draft(&app), Some(("src", FileDraftKind::NewFolder, "")));
        submit(&mut app, "docs");
        assert!(project.join("src/docs").is_dir());
        act(&mut app, "new_folder", "");
        assert_eq!(draft(&app), Some(("", FileDraftKind::NewFolder, "")));
        // An existing name is an error on the row and replaces nothing.
        submit(&mut app, "README.md");
        assert_eq!(draft(&app), Some(("", FileDraftKind::NewFolder, "README.md")));
        assert_eq!(draft_error(&app), "Ya existe un elemento con ese nombre");
        assert!(app.file_draft_focus_pending, "the field takes the focus back");
        assert_eq!(
            std::fs::read_to_string(project.join("README.md")).unwrap(),
            "# Hola"
        );
        typed(&mut app, "x");
        assert_eq!(draft_error(&app), "");
        for name in ["", "  ", ".", "..", "a/b", "a\\b", "nul\0"] {
            submit(&mut app, name);
            assert!(!draft_error(&app).is_empty(), "{name:?}");
        }
        assert_eq!(std::fs::read_dir(&project).unwrap().count(), 2);
    }

    #[test]
    fn rename_keeps_the_parent_refuses_existing_names_and_unsaved_documents() {
        let (_root, mut app) = fixture();
        let project = PathBuf::from(&app.view.workspace);
        act(&mut app, "toggle", "src");
        act(&mut app, "rename", "src/main.rs");
        assert_eq!(
            draft(&app),
            Some(("src/main.rs", FileDraftKind::Rename, "main.rs"))
        );
        // The same name is not a rename: the field just closes.
        submit(&mut app, "main.rs");
        assert_eq!(draft(&app), None);
        act(&mut app, "rename", "src/main.rs");
        submit(&mut app, "deep");
        assert_eq!(draft_error(&app), "Ya existe un elemento con ese nombre");
        assert!(project.join("src/main.rs").is_file() && project.join("src/deep").is_dir());
        submit(&mut app, "../main.rs");
        assert!(draft_error(&app).contains("separadores"));
        submit(&mut app, "app.rs");
        assert_eq!(draft(&app), None);
        assert_eq!(
            std::fs::read_to_string(project.join("src/app.rs")).unwrap(),
            "fn main() {}"
        );
        assert!(!project.join("src/main.rs").exists());
        assert_eq!(names(&app), ["src", "deep", "app.rs", "README.md"]);
        // The root and a path that left the tree have nothing to rename.
        act(&mut app, "rename", "");
        act(&mut app, "rename", "src/main.rs");
        assert_eq!(draft(&app), None);
        // A document still being written under the folder blocks its rename.
        app.init_code_editor_recovery();
        app.code_editor_create("src/draft.rs".into(), "texto".into())
            .unwrap();
        act(&mut app, "rename", "src");
        submit(&mut app, "lib");
        assert_eq!(
            draft_error(&app),
            "Guarda los cambios abiertos antes de renombrar"
        );
        assert!(project.join("src").is_dir() && !project.join("lib").exists());
    }

    #[test]
    fn delete_asks_first_and_only_ever_moves_to_the_trash() {
        let (_root, mut app) = fixture();
        let project = PathBuf::from(&app.view.workspace);
        // The root is never offered, and an action naming it does nothing.
        act(&mut app, "delete", "");
        assert_eq!(app.view.file_delete, None);
        act(&mut app, "delete", "src");
        assert_eq!(app.view.file_delete, Some(("src".into(), true)));
        assert!(app.file_op.is_none() && project.join("src/main.rs").is_file());
        clicked(&mut app, "file_tree_delete_cancel");
        assert_eq!(app.view.file_delete, None);
        pump(&mut app);
        assert!(project.join("src/main.rs").is_file());
        // A trash that fails reports it and nothing else happens.
        act(&mut app, "delete", "src");
        clicked(&mut app, "file_tree_delete_confirm");
        pump(&mut app);
        assert_eq!(
            app.view.status,
            "No se pudo enviar a la papelera: la papelera del sistema no está disponible"
        );
        assert!(project.join("src/main.rs").is_file());
        assert_eq!(names(&app), ["src", "README.md"]);
        // Confirmed for a path that left the tree meanwhile: nothing runs.
        act(&mut app, "delete", "README.md");
        assert_eq!(app.view.file_delete, Some(("README.md".into(), false)));
        std::fs::rename(project.join("README.md"), project.join("LEEME.md")).unwrap();
        app.file_explorer.as_mut().unwrap().refresh().unwrap();
        app.refresh_file_rows();
        clicked(&mut app, "file_tree_delete_confirm");
        assert!(app.file_op.is_none());
        assert_eq!(app.view.status, "Ese archivo ya no está en el árbol");
        app.file_trash = crate::file_ops::tests::fake_trash;
        act(&mut app, "delete", "src");
        clicked(&mut app, "file_tree_delete_confirm");
        pump(&mut app);
        assert!(!project.join("src").exists());
        assert!(project.join("src.trashed/main.rs").is_file());
        assert_eq!(names(&app), ["src.trashed", "LEEME.md"]);
    }

    #[test]
    fn the_name_field_takes_the_focus_and_leaves_with_it_when_blank_or_unchanged() {
        let (_root, mut app) = fixture();
        let project = PathBuf::from(&app.view.workspace);
        app.open_files_dock();
        let frame = |app: &mut AgentsWindow| {
            app.tree.set_root(build_view(&app.view, 1200.0));
            app.tree.layout(Size::new(1200.0, 720.0));
            app.focus_file_draft_if_pending();
        };
        let leave = |app: &mut AgentsWindow| {
            assert!(app.tree.focus("files_search"));
            frame(app);
        };
        act(&mut app, "new_file", "");
        frame(&mut app);
        assert_eq!(app.tree.focused_id(), Some("file_tree_draft"));
        // A typed name survives losing the focus; a blank one does not.
        typed(&mut app, "notas.md");
        leave(&mut app);
        assert!(app.view.file_draft.is_some());
        typed(&mut app, "  ");
        frame(&mut app);
        assert_eq!(draft(&app), None);
        act(&mut app, "rename", "README.md");
        frame(&mut app);
        assert_eq!(app.tree.focused_id(), Some("file_tree_draft"));
        assert_eq!(app.tree.input_value("file_tree_draft"), Some("README.md"));
        leave(&mut app);
        assert_eq!(draft(&app), None, "an unchanged name goes with the focus");
        act(&mut app, "rename", "README.md");
        frame(&mut app);
        typed(&mut app, "LEEME.md");
        leave(&mut app);
        assert!(app.view.file_draft.is_some());
        // Escape cancels whatever was typed, and nothing reached the disk.
        app.cancel_file_draft();
        frame(&mut app);
        assert_eq!(draft(&app), None);
        assert!(project.join("README.md").is_file() && !project.join("LEEME.md").exists());
        assert!(!project.join("notas.md").exists());
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
