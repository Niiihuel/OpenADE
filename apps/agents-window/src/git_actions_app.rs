use super::*;
use crate::git_actions::{GitActionKind, GitActionRequest, GitActionWorker};

impl AgentsWindow {
    pub(super) fn handle_git_action(&mut self, id: &str) -> bool {
        if id == "review_detail_open_editor" {
            if self.review_active_workspace.as_deref() != Some(Path::new(&self.view.workspace)) {
                return true;
            }
            if let Some(detail) = &self.view.review_detail {
                let path = PathBuf::from(&detail.path);
                let owner = self.view.selected_session.as_deref().unwrap_or("");
                if self
                    .dock
                    .open_file(Path::new(&self.view.workspace), owner, &path)
                    .is_some()
                {
                    self.sync_dock_view();
                }
            }
            return true;
        }
        let (kind, target, untracked) =
            if let Some(target) = id.strip_prefix("review_stage_untracked:") {
                (GitActionKind::Stage, target, true)
            } else if let Some(target) = id.strip_prefix("review_stage:") {
                (GitActionKind::Stage, target, false)
            } else if let Some(target) = id.strip_prefix("review_unstage:") {
                (GitActionKind::Unstage, target, false)
            } else {
                return false;
            };
        if self.view.environment.workflow.busy.is_some() || self.git_action_request.is_some() {
            return true;
        }
        let Some(Ok(review)) = &self.view.git_review else {
            return true;
        };
        if self.review_active_workspace.as_deref() != Some(Path::new(&self.view.workspace)) {
            return true;
        }
        let paths = if untracked {
            let Some(path) = target
                .parse::<usize>()
                .ok()
                .and_then(|index| review.untracked.get(index))
            else {
                return true;
            };
            vec![PathBuf::from(path)]
        } else {
            let Some((section, file)) = target.split_once(':').and_then(|(section, file)| {
                Some((section.parse::<usize>().ok()?, file.parse::<usize>().ok()?))
            }) else {
                return true;
            };
            let Some(section) = review.sections.get(section) else {
                return true;
            };
            let expected = if kind == GitActionKind::Stage {
                "CAMBIOS SIN PREPARAR"
            } else {
                "CAMBIOS PREPARADOS"
            };
            if section.title != expected {
                return true;
            }
            let Some(file) = section.files.get(file) else {
                return true;
            };
            let mut paths = vec![PathBuf::from(&file.path)];
            if let Some(old) = &file.old_path {
                if old != &file.path {
                    paths.push(PathBuf::from(old));
                }
            }
            paths
        };
        self.git_action_generation = self.git_action_generation.wrapping_add(1);
        let request = GitActionRequest {
            workspace: PathBuf::from(&self.view.workspace),
            generation: self.git_action_generation,
            kind,
            paths,
        };
        if self.git_action_worker.is_none() {
            self.git_action_worker = GitActionWorker::spawn(self.file_trash).ok();
        }
        self.view.git_action_error = None;
        if self
            .git_action_worker
            .as_ref()
            .is_some_and(|worker| worker.submit(request.clone()).is_ok())
        {
            self.git_action_request = Some(request);
            self.view.git_action_pending = true;
        } else {
            self.view.git_action_error = Some("No se pudo iniciar la operación de Git".into());
        }
        true
    }

    pub(super) fn update_git_actions(&mut self, ctx: &egui::Context) -> bool {
        let response = self
            .git_action_worker
            .as_ref()
            .and_then(GitActionWorker::poll);
        let mut changed = false;
        if let Some(response) = response {
            if self.git_action_request.as_ref() == Some(&response.request) {
                self.git_action_request = None;
                self.view.git_action_pending = false;
                changed = true;
                if response.request.workspace == Path::new(&self.view.workspace) {
                    match response.result {
                        Ok(()) => {
                            self.request_git_review(response.request.workspace);
                            if self.view.environment.open {
                                self.request_environment_git(
                                    crate::environment_git::Operation::Refresh,
                                    ctx,
                                );
                            } else {
                                // A later open must not offer a commit using
                                // the index fingerprint from before staging.
                                self.environment_git_revision =
                                    self.environment_git_revision.wrapping_add(1);
                                self.environment_git_workspace = None;
                                self.view.environment.workflow.snapshot = None;
                                self.view.environment.workflow.loading = false;
                                self.view.environment.workflow.comparison = None;
                                self.view.environment.workflow.compare_url = None;
                            }
                        }
                        Err(error) => self.view.git_action_error = Some(error),
                    }
                }
            }
        }
        if self.git_action_request.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        child_process::command_no_window,
        git_review::{load_git_review, GitReview},
    };
    use std::fs;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        fs::create_dir(&repository).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.name", "Test"],
            &["config", "user.email", "test@example.invalid"],
        ] {
            assert!(command_no_window("git")
                .args(args)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success());
        }
        fs::write(repository.join("tracked.txt"), "initial\n").unwrap();
        for args in [&["add", "."][..], &["commit", "-qm", "initial"]] {
            assert!(command_no_window("git")
                .args(args)
                .current_dir(&repository)
                .status()
                .unwrap()
                .success());
        }
        fs::write(repository.join("tracked.txt"), "changed\n").unwrap();
        let mut app = AgentsWindow::with_data_root(temp.path().join("data"));
        app.view.workspace = repository
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.side_tab = SideTab::Files;
        // with_data_root starts a scan of the initial workspace. Switch via
        // the production request path so its in-flight generation is retired.
        app.request_git_review(PathBuf::from(&app.view.workspace));
        settle(&mut app);
        (temp, app)
    }

    fn settle(app: &mut AgentsWindow) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let context = egui::Context::default();
        loop {
            app.update_git_actions(&context);
            app.update_git_review(&context);
            if app.git_action_request.is_none() && !app.review_in_flight {
                break;
            }
            assert!(Instant::now() < deadline, "Git workers did not complete");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn stage_and_unstage_actions_refresh_the_same_repository_after_worker_completion() {
        let (_temp, mut app) = fixture();
        assert!(app.handle_git_action("review_stage:0:0"));
        assert!(app.view.git_action_pending);
        let generation = app.git_action_generation;
        // Repeated clicks while pending cannot enqueue duplicate mutations.
        assert!(app.handle_git_action("review_stage:0:0"));
        assert_eq!(app.git_action_generation, generation);
        settle(&mut app);
        assert!(!app.view.git_action_pending);
        assert!(app.view.git_action_error.is_none());
        assert_eq!(
            app.view
                .git_review
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .sections[0]
                .title,
            "CAMBIOS PREPARADOS"
        );
        assert!(app.handle_git_action("review_unstage:0:0"));
        settle(&mut app);
        assert_eq!(
            app.view
                .git_review
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .sections[0]
                .title,
            "CAMBIOS SIN PREPARAR"
        );
        assert_eq!(
            fs::read_to_string(Path::new(&app.view.workspace).join("tracked.txt")).unwrap(),
            "changed\n"
        );
    }

    #[test]
    fn completed_action_from_previous_workspace_does_not_replace_the_new_repository_view() {
        let (temp, mut app) = fixture();
        let previous_workspace = PathBuf::from(&app.view.workspace);
        assert!(app.handle_git_action("review_stage:0:0"));
        let other = temp.path().join("other");
        fs::create_dir(&other).unwrap();
        app.view.workspace = other.to_string_lossy().into_owned();
        app.review_active_workspace = Some(other);
        app.view.git_review = Some(Ok(GitReview {
            branch: "new workspace".into(),
            ..Default::default()
        }
        .into()));
        app.view.git_action_error = Some("new workspace notice".into());
        let generation = app.review_generation;
        settle(&mut app);
        assert_eq!(app.review_generation, generation);
        assert_eq!(
            app.view
                .git_review
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .branch,
            "new workspace"
        );
        assert_eq!(
            app.view.git_action_error.as_deref(),
            Some("new workspace notice")
        );
        assert!(!app.view.git_action_pending);
        assert_eq!(
            load_git_review(&previous_workspace).unwrap().sections[0].title,
            "CAMBIOS PREPARADOS"
        );
    }

    #[test]
    fn stage_actions_reject_stale_scope_wrong_stage_and_missing_rows() {
        let (_temp, mut app) = fixture();
        assert!(app.handle_git_action("review_unstage:0:0"));
        assert!(app.git_action_request.is_none());
        assert!(app.handle_git_action("review_stage:99:99"));
        assert!(app.git_action_request.is_none());
        app.review_active_workspace = Some(PathBuf::from("another repository"));
        assert!(app.handle_git_action("review_stage:0:0"));
        assert!(app.git_action_request.is_none());
        assert_eq!(app.git_action_generation, 0);
    }
}
