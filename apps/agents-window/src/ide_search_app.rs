//! Search actions keep results tied to the current project and hand navigation
//! back to the existing document/selection pipeline.
use super::*;
use crate::ide_search::{IdeSearchWorker, MAX_QUERY_CHARS};

impl AgentsWindow {
    pub(super) fn poll_ide_search(&mut self, ctx: &egui::Context) -> bool {
        if let Some(hit) = self.view.ide_search.pending_navigation.as_ref() {
            let same_navigation = self
                .editor_services
                .pending_navigation
                .as_ref()
                .is_some_and(|(path, position)| {
                    path == &hit.path
                        && position.line == hit.line.saturating_sub(1)
                        && position.character == hit.character
                });
            if self.view.ide_search.workspace != self.view.workspace && same_navigation {
                self.editor_services.pending_navigation = None;
            }
            if !same_navigation {
                self.view.ide_search.pending_navigation = None;
            }
        }
        let mut changed = self.view.ide_search.change_workspace(&self.view.workspace);
        if changed {
            self.tree.set_input_value("ide_search_query", "");
            if let Some(worker) = &self.ide_search_worker {
                worker.cancel(self.view.ide_search.generation);
            }
        }
        if let Some(result) = self
            .ide_search_worker
            .as_ref()
            .and_then(IdeSearchWorker::poll)
        {
            changed |= self.view.ide_search.apply(result);
        }
        if self.view.ide_search.running {
            ctx.request_repaint_after(Duration::from_millis(32));
        }
        changed
    }

    pub(super) fn apply_ide_search_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        let is_ours = match action {
            UiAction::Clicked { id } => {
                matches!(id.as_str(), "ide_search_submit" | "ide_search_case")
                    || id.starts_with("ide_search_result:")
            }
            UiAction::InputChanged { id, .. } | UiAction::Submitted { id, .. } => {
                id == "ide_search_query"
            }
            _ => false,
        };
        if !is_ours {
            return false;
        }
        if !self.ide_workspace_active() {
            return true;
        }
        self.poll_ide_search(ctx);
        match action {
            UiAction::InputChanged { value, .. } => {
                self.view.ide_search.invalidate();
                self.view.ide_search.query = value.chars().take(MAX_QUERY_CHARS).collect();
                self.tree
                    .set_input_value("ide_search_query", &self.view.ide_search.query);
                if let Some(worker) = &self.ide_search_worker {
                    worker.cancel(self.view.ide_search.generation);
                }
            }
            UiAction::Clicked { id } if id == "ide_search_case" => {
                self.view.ide_search.case_sensitive = !self.view.ide_search.case_sensitive;
                self.start_ide_search();
            }
            UiAction::Clicked { id } if id.starts_with("ide_search_result:") => {
                if let Some(hit) = self.view.ide_search.navigation(id, &self.view.workspace) {
                    let scope = self.dock_scope();
                    self.dock.open(
                        &scope,
                        DockTabKind::FilePreview {
                            path: hit.path.clone(),
                        },
                    );
                    self.dock.set_collapsed(&scope, false);
                    self.view.ide.compact_panel = None;
                    self.view.ide.extensions_open = false;
                    self.sync_dock_view();
                    self.view.ide_search.pending_navigation = Some(hit.clone());
                    self.editor_services.pending_navigation = Some((
                        hit.path,
                        openide_language::Position {
                            line: hit.line.saturating_sub(1),
                            character: hit.character,
                        },
                    ));
                }
            }
            UiAction::Submitted { value, .. } => {
                self.view.ide_search.query = value.chars().take(MAX_QUERY_CHARS).collect();
                self.start_ide_search();
            }
            UiAction::Clicked { id } if id == "ide_search_submit" => self.start_ide_search(),
            _ => {}
        }
        ctx.request_repaint();
        true
    }

    fn start_ide_search(&mut self) {
        if self.view.ide_search.query.trim().is_empty() {
            self.view.ide_search.invalidate();
            if let Some(worker) = &self.ide_search_worker {
                worker.cancel(self.view.ide_search.generation);
            }
            return;
        }
        let request = self.view.ide_search.request();
        if self.ide_search_worker.is_none() {
            match IdeSearchWorker::spawn() {
                Ok(worker) => self.ide_search_worker = Some(worker),
                Err(error) => {
                    self.view.ide_search.running = false;
                    self.view.ide_search.error =
                        Some(format!("No se pudo iniciar la búsqueda: {error}"));
                    return;
                }
            }
        }
        if let Some(worker) = &self.ide_search_worker {
            worker.submit(request);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_search_result_opens_document_and_targets_real_line() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("a.rs"), "first\nsecond\nthird\n").unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.main_page = MainPage::Workspace;
        app.view.show_settings = false;
        app.poll_ide_search(&egui::Context::default());
        app.view.ide_search.hits.push(crate::ide_search::SearchHit {
            path: "a.rs".into(),
            line: 3,
            character: 2,
            snippet: "third".into(),
        });
        let action = UiAction::Clicked {
            id: app.view.ide_search.result_action(0),
        };
        assert!(app.apply_ide_search_action(&action, &egui::Context::default()));
        assert!(matches!(
            app.dock.active_tab(&app.dock_scope()).map(|tab| &tab.kind),
            Some(DockTabKind::FilePreview { path }) if path == Path::new("a.rs")
        ));
        let (path, position) = app.editor_services.pending_navigation.as_ref().unwrap();
        assert_eq!(path, &PathBuf::from("a.rs"));
        assert_eq!(position.line, 2);
        assert_eq!(position.character, 2);
        app.view.ide_search.pending_navigation = None;
        app.view.ide_search.invalidate();
        app.editor_services.pending_navigation = None;
        app.apply_ide_search_action(&action, &egui::Context::default());
        assert!(app.editor_services.pending_navigation.is_none());
    }

    #[test]
    fn ide_search_workspace_change_cancels_its_pending_navigation() {
        let root = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = "/new-project".into();
        app.view.ide_search.workspace = "/previous-project".into();
        let hit = crate::ide_search::SearchHit {
            path: "same.rs".into(),
            line: 10,
            character: 3,
            snippet: String::new(),
        };
        app.editor_services.pending_navigation = Some((
            hit.path.clone(),
            openide_language::Position {
                line: 9,
                character: 3,
            },
        ));
        app.view.ide_search.pending_navigation = Some(hit);
        assert!(app.poll_ide_search(&egui::Context::default()));
        assert!(app.editor_services.pending_navigation.is_none());
        assert!(app.view.ide_search.pending_navigation.is_none());
    }
}
