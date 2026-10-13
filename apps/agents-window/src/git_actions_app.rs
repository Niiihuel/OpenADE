use super::*;
use crate::git_actions::{GitActionKind, GitActionRequest, GitActionWorker};
use crate::git_review::ChangeGroup;

impl AgentsWindow {
    pub(super) fn handle_git_action(&mut self, id: &str, ctx: &egui::Context) -> bool {
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
        match id {
            "review_error_dismiss" => {
                self.view.git_action_error = None;
                self.view.environment.workflow.error = None;
                self.view.environment.workflow.notice = None;
                return true;
            }
            "review_discard_cancel" => {
                self.view.git_panel.confirm = None;
                return true;
            }
            "review_hunk_previous" => {
                self.view.git_panel.hunk = self.view.git_panel.hunk.saturating_sub(1);
                return true;
            }
            "review_hunk_next" | "review_hunk_discard" => {
                // The blocks are those of the diff that is open, as it was read.
                let open = self.view.review_detail.as_ref().and_then(|detail| match &detail.content {
                    Some(Ok(Some(diff))) if detail.stage == DiffStage::Unstaged => Some((
                        detail.path.clone(),
                        diff.file.lines.iter().filter(|line| line.kind == crate::git_review::ReviewLineKind::Hunk).count(),
                    )),
                    _ => None,
                });
                let Some((path, blocks)) = open.filter(|(_, blocks)| *blocks > 0) else {
                    return true;
                };
                let current = self.view.git_panel.hunk.min(blocks - 1);
                if id == "review_hunk_next" {
                    self.view.git_panel.hunk = (current + 1).min(blocks - 1);
                } else if self.view.environment.workflow.busy.is_none() && self.git_action_request.is_none() {
                    // Never at once: the dialog names the block first.
                    self.view.git_panel.confirm = Some(crate::view::GitConfirmView {
                        workspace: self.view.workspace.clone(),
                        trash: false,
                        paths: vec![PathBuf::from(path)],
                        hunk: Some(current),
                    });
                }
                return true;
            }
            "review_discard_confirm" => {
                // What was named in the dialog is what is discarded, and only
                // in the repository it was asked for.
                if let Some(confirm) = self.view.git_panel.confirm.take() {
                    if confirm.workspace == self.view.workspace {
                        let kind = match (confirm.trash, confirm.hunk) {
                            (true, _) => GitActionKind::Trash,
                            (false, Some(hunk)) => GitActionKind::DiscardHunk(hunk),
                            (false, None) => GitActionKind::Discard,
                        };
                        self.submit_git_action(kind, confirm.paths);
                    }
                }
                return true;
            }
            "ide_git_branch_toggle" => {
                // The open card would show the same picker: it gives way.
                // Closed, it must not close the picker this click toggles.
                if self.view.environment.open {
                    self.move_git_form_to_ide_sidebar();
                }
                self.apply_action(
                    UiAction::Clicked {
                        id: "environment_branch_toggle".into(),
                    },
                    ctx,
                );
                return true;
            }
            _ => {}
        }
        let Some((operation, target)) = id.split_once(':') else {
            return false;
        };
        if !matches!(
            operation,
            "review_group_toggle"
                | "review_stage"
                | "review_stage_untracked"
                | "review_unstage"
                | "review_discard"
                | "review_open_file"
                | "review_copy_path"
                | "review_copy_relative"
                | "review_stage_all"
                | "review_unstage_all"
                | "review_discard_all"
        ) {
            return false;
        }
        if operation == "review_group_toggle" {
            if let Some(group) = ChangeGroup::from_key(target) {
                let collapsed = &mut self.view.git_panel.collapsed[group as usize];
                *collapsed = !*collapsed;
            }
            return true;
        }
        let Some(Ok(review)) = &self.view.git_review else {
            return true;
        };
        if self.review_active_workspace.as_deref() != Some(Path::new(&self.view.workspace)) {
            return true;
        }
        // A whole group, or the one row the action names.
        let (group, paths) = if operation.ends_with("_all") {
            let Some(group) = ChangeGroup::from_key(target) else {
                return true;
            };
            let paths = (0..review.group(group).len)
                .flat_map(|index| review.paths(group, index))
                .collect::<Vec<_>>();
            (group, paths)
        } else {
            let target = if operation == "review_stage_untracked" {
                format!("n:{target}")
            } else {
                target.to_owned()
            };
            let Some((group, index)) = review.locate(&target) else {
                return true;
            };
            (group, review.paths(group, index))
        };
        if paths.is_empty() {
            return true;
        }
        use ChangeGroup::{Changes, Conflicts, Staged, Untracked};
        match (operation, group) {
            ("review_open_file", _) => {
                let owner = self.view.selected_session.as_deref().unwrap_or("");
                if self
                    .dock
                    .open_file(Path::new(&self.view.workspace), owner, &paths[0])
                    .is_some()
                {
                    self.sync_dock_view();
                }
            }
            ("review_copy_path", _) => {
                ctx.copy_text(
                    Path::new(&self.view.workspace)
                        .join(&paths[0])
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            ("review_copy_relative", _) => {
                ctx.copy_text(paths[0].to_string_lossy().into_owned());
            }
            ("review_stage", Conflicts | Changes)
            | ("review_stage_untracked", Untracked)
            | ("review_stage_all", Changes | Untracked) => {
                self.submit_git_action(GitActionKind::Stage, paths);
            }
            ("review_unstage" | "review_unstage_all", Staged) => {
                self.submit_git_action(GitActionKind::Unstage, paths);
            }
            // Never at once: the dialog names the files first.
            ("review_discard" | "review_discard_all", Changes | Untracked) => {
                if self.view.environment.workflow.busy.is_none()
                    && self.git_action_request.is_none()
                {
                    self.view.git_panel.confirm = Some(crate::view::GitConfirmView {
                        workspace: self.view.workspace.clone(),
                        trash: group == Untracked,
                        paths,
                        hunk: None,
                    });
                }
            }
            _ => {}
        }
        true
    }

    fn submit_git_action(&mut self, kind: GitActionKind, paths: Vec<PathBuf>) {
        if self.view.environment.workflow.busy.is_some() || self.git_action_request.is_some() {
            return;
        }
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
    }

    /// The Git sidebar of the IDE is on screen, so its commit and sync
    /// controls need the repository snapshot as fresh as its list.
    pub(super) fn git_sidebar_visible(&self) -> bool {
        self.ide_workspace_active()
            && self.view.ide.activity == crate::ide_workspace::IdeActivity::Git
            && self.view.ide.explorer_open
    }

    /// Re-read the repository without clearing what the last operation said.
    pub(super) fn refresh_environment_git_quietly(&mut self, ctx: &egui::Context) {
        let workflow = &mut self.view.environment.workflow;
        if workflow.loading || workflow.busy.is_some() {
            return;
        }
        let (error, notice) = (workflow.error.take(), workflow.notice.take());
        self.request_environment_git(crate::environment_git::Operation::Refresh, ctx);
        let workflow = &mut self.view.environment.workflow;
        workflow.error = workflow.error.take().or(error);
        workflow.notice = notice;
    }

    /// A row shows its actions while the pointer or the keyboard is on it.
    pub(super) fn sync_git_panel_reveal(&mut self) -> bool {
        let revealed = [self.tree.hovered_id(), self.tree.focused_id()]
            .map(|id| id.and_then(crate::view::git_reveal_key));
        let changed = self.view.git_panel.revealed != revealed;
        self.view.git_panel.revealed = revealed;
        changed
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
                            if matches!(
                                response.request.kind,
                                GitActionKind::Discard | GitActionKind::Trash | GitActionKind::DiscardHunk(_)
                            ) {
                                self.invalidate_workspace_files();
                                if self.view.side_tab == SideTab::Files
                                    || self.ide_workspace_active()
                                {
                                    self.refresh_file_rows();
                                }
                            }
                            if matches!(response.request.kind, GitActionKind::DiscardHunk(_)) {
                                // The diff that is open lost a block.
                                self.request_review_detail();
                            }
                            self.request_git_review(response.request.workspace);
                            if self.view.environment.open || self.git_sidebar_visible() {
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
    fn the_file_tree_is_decorated_without_the_source_control_panel_and_follows_a_refresh() {
        let (_temp, mut app) = fixture();
        assert_eq!(app.view.side_tab, SideTab::Files);
        let letter = |app: &AgentsWindow, path: &str| {
            crate::git_decorations::for_state(&app.view)
                .and_then(|decorations| decorations.file(path))
                .map(|mark| mark.letter)
        };
        assert_eq!(letter(&app, "tracked.txt"), Some("M"));
        let first = crate::git_decorations::for_state(&app.view).unwrap();
        // The same snapshot is not indexed again by another frame.
        assert!(Arc::ptr_eq(&first, &crate::git_decorations::for_state(&app.view).unwrap()));
        // What the tree's watcher, a finished tool, a save and the window's
        // focus all do: mark the environment stale.
        fs::write(Path::new(&app.view.workspace).join("new.txt"), "new\n").unwrap();
        let context = egui::Context::default();
        // The first pass adopts the workspace; a stale mark is for later ones.
        app.update_environment(&context);
        app.environment_dirty = true;
        app.environment_last_refresh = Instant::now() - Duration::from_secs(1);
        app.update_environment(&context);
        settle(&mut app);
        assert_eq!(letter(&app, "new.txt"), Some("U"));
        assert_eq!(letter(&app, "tracked.txt"), Some("M"));
        assert!(!Arc::ptr_eq(&first, &crate::git_decorations::for_state(&app.view).unwrap()));
    }

    #[test]
    fn stage_and_unstage_actions_refresh_the_same_repository_after_worker_completion() {
        let (_temp, mut app) = fixture();
        assert!(app.handle_git_action("review_stage:0:0", &egui::Context::default()));
        assert!(app.view.git_action_pending);
        let generation = app.git_action_generation;
        // Repeated clicks while pending cannot enqueue duplicate mutations.
        assert!(app.handle_git_action("review_stage:0:0", &egui::Context::default()));
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
        assert!(app.handle_git_action("review_unstage:0:0", &egui::Context::default()));
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
        assert!(app.handle_git_action("review_stage:0:0", &egui::Context::default()));
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

    /// Stands in for the system trash: the file leaves the worktree.
    fn remove(path: &Path) -> Result<(), String> {
        fs::remove_file(path).map_err(|error| error.to_string())
    }

    fn click(app: &mut AgentsWindow, id: &str) {
        assert!(app.handle_git_action(id, &egui::Context::default()), "{id}");
    }

    fn group(app: &AgentsWindow, group: ChangeGroup) -> Vec<PathBuf> {
        let review = app.view.git_review.clone().unwrap().unwrap();
        (0..review.group(group).len)
            .flat_map(|index| review.paths(group, index))
            .collect()
    }

    /// `tracked.txt` and `second.txt` changed, `new.txt` and `notes.txt`
    /// without tracking.
    fn several_changes() -> (tempfile::TempDir, AgentsWindow) {
        let (temp, mut app) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        fs::write(root.join("second.txt"), "initial\n").unwrap();
        for args in [&["add", "second.txt"][..], &["commit", "-qm", "second"]] {
            assert!(command_no_window("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success());
        }
        fs::write(root.join("second.txt"), "changed\n").unwrap();
        fs::write(root.join("new.txt"), "new\n").unwrap();
        fs::write(root.join("notes.txt"), "notes\n").unwrap();
        app.file_trash = remove;
        app.request_git_review(root);
        settle(&mut app);
        (temp, app)
    }

    #[test]
    fn group_actions_take_every_file_of_their_group_and_only_those() {
        let (_temp, mut app) = several_changes();
        assert_eq!(group(&app, ChangeGroup::Changes).len(), 2);
        assert_eq!(group(&app, ChangeGroup::Untracked).len(), 2);
        // Unstaging a group that is not the staged one does nothing.
        click(&mut app, "review_unstage_all:u");
        assert!(app.git_action_request.is_none());

        click(&mut app, "review_stage_all:u");
        assert_eq!(
            app.git_action_request.as_ref().unwrap().paths,
            vec![PathBuf::from("second.txt"), PathBuf::from("tracked.txt")]
        );
        settle(&mut app);
        assert_eq!(group(&app, ChangeGroup::Staged).len(), 2);
        assert!(group(&app, ChangeGroup::Changes).is_empty());
        assert_eq!(group(&app, ChangeGroup::Untracked).len(), 2, "new files were not asked for");

        click(&mut app, "review_stage_all:n");
        settle(&mut app);
        assert_eq!(group(&app, ChangeGroup::Staged).len(), 4);
        assert!(group(&app, ChangeGroup::Untracked).is_empty());

        click(&mut app, "review_unstage_all:s");
        assert_eq!(app.git_action_request.as_ref().unwrap().kind, GitActionKind::Unstage);
        settle(&mut app);
        assert!(group(&app, ChangeGroup::Staged).is_empty());
        assert_eq!(group(&app, ChangeGroup::Changes).len(), 2);
        assert_eq!(group(&app, ChangeGroup::Untracked).len(), 2);
        assert!(app.view.git_action_error.is_none());
    }

    #[test]
    fn discarding_waits_for_the_answer_and_cancel_leaves_the_file() {
        let (_temp, mut app) = several_changes();
        let root = PathBuf::from(&app.view.workspace);
        let target = {
            let review = app.view.git_review.clone().unwrap().unwrap();
            let index = review
                .paths(ChangeGroup::Changes, 0)
                .first()
                .map(|path| path == Path::new("tracked.txt"))
                .map_or(0, |first| usize::from(!first));
            review.target(ChangeGroup::Changes, index)
        };
        click(&mut app, &format!("review_discard:{target}"));
        assert!(app.git_action_request.is_none(), "nothing runs before the answer");
        let confirm = app.view.git_panel.confirm.clone().unwrap();
        assert_eq!(confirm.paths, vec![PathBuf::from("tracked.txt")]);
        assert!(!confirm.trash);
        assert_eq!(fs::read_to_string(root.join("tracked.txt")).unwrap(), "changed\n");

        click(&mut app, "review_discard_cancel");
        assert!(app.view.git_panel.confirm.is_none());
        assert!(app.git_action_request.is_none());
        assert_eq!(fs::read_to_string(root.join("tracked.txt")).unwrap(), "changed\n");

        click(&mut app, &format!("review_discard:{target}"));
        click(&mut app, "review_discard_confirm");
        assert_eq!(app.git_action_request.as_ref().unwrap().kind, GitActionKind::Discard);
        assert!(app.view.git_panel.confirm.is_none());
        settle(&mut app);
        assert!(app.view.git_action_error.is_none(), "{:?}", app.view.git_action_error);
        assert_eq!(fs::read_to_string(root.join("tracked.txt")).unwrap(), "initial\n");
        assert_eq!(fs::read_to_string(root.join("second.txt")).unwrap(), "changed\n");
        assert_eq!(group(&app, ChangeGroup::Changes), vec![PathBuf::from("second.txt")]);
    }

    #[test]
    fn untracked_files_go_to_the_trash_one_or_all_after_the_answer() {
        let (_temp, mut app) = several_changes();
        let root = PathBuf::from(&app.view.workspace);
        click(&mut app, "review_discard:n:0");
        let confirm = app.view.git_panel.confirm.clone().unwrap();
        assert!(confirm.trash);
        assert_eq!(confirm.paths, vec![PathBuf::from("new.txt")]);
        click(&mut app, "review_discard_confirm");
        assert_eq!(app.git_action_request.as_ref().unwrap().kind, GitActionKind::Trash);
        settle(&mut app);
        assert!(!root.join("new.txt").exists());
        assert!(root.join("notes.txt").exists());

        fs::write(root.join("new.txt"), "again\n").unwrap();
        app.request_git_review(root.clone());
        settle(&mut app);
        click(&mut app, "review_discard_all:n");
        assert_eq!(app.view.git_panel.confirm.as_ref().unwrap().paths.len(), 2);
        assert!(root.join("new.txt").exists() && root.join("notes.txt").exists());
        click(&mut app, "review_discard_confirm");
        settle(&mut app);
        assert!(!root.join("new.txt").exists() && !root.join("notes.txt").exists());
        // Tracked files were never part of it.
        assert_eq!(fs::read_to_string(root.join("tracked.txt")).unwrap(), "changed\n");

        // All tracked changes: asked as one question, restored together.
        click(&mut app, "review_discard_all:u");
        let confirm = app.view.git_panel.confirm.clone().unwrap();
        assert!(!confirm.trash);
        assert_eq!(confirm.paths.len(), 2);
        click(&mut app, "review_discard_confirm");
        settle(&mut app);
        assert_eq!(fs::read_to_string(root.join("tracked.txt")).unwrap(), "initial\n");
        assert_eq!(fs::read_to_string(root.join("second.txt")).unwrap(), "initial\n");
    }

    #[test]
    fn staged_files_cannot_be_discarded_and_an_answer_for_another_project_is_dropped() {
        let (_temp, mut app) = several_changes();
        click(&mut app, "review_stage_all:u");
        settle(&mut app);
        let staged = app
            .view
            .git_review
            .clone()
            .unwrap()
            .unwrap()
            .target(ChangeGroup::Staged, 0);
        click(&mut app, &format!("review_discard:{staged}"));
        click(&mut app, "review_discard_all:s");
        assert!(app.view.git_panel.confirm.is_none());

        click(&mut app, "review_discard:n:0");
        assert!(app.view.git_panel.confirm.is_some());
        app.view.workspace = "/another/project".into();
        click(&mut app, "review_discard_confirm");
        assert!(app.git_action_request.is_none());
        assert!(app.view.git_panel.confirm.is_none());
    }

    #[test]
    fn rows_open_their_file_fold_their_group_and_dismiss_what_git_said() {
        let (_temp, mut app) = several_changes();
        click(&mut app, "review_group_toggle:u");
        assert_eq!(app.view.git_panel.collapsed, [false, false, true, false]);
        click(&mut app, "review_group_toggle:u");
        assert_eq!(app.view.git_panel.collapsed, [false; 4]);
        // Opening, copying and folding never start a Git operation.
        for id in [
            "review_open_file:n:0",
            "review_copy_path:n:0",
            "review_copy_relative:n:0",
            "review_open_file:n:99",
            "review_group_toggle:x",
        ] {
            click(&mut app, id);
            assert!(app.git_action_request.is_none(), "{id}");
            assert!(app.view.git_panel.confirm.is_none(), "{id}");
        }
        app.view.git_action_error = Some("fatal: failed".into());
        app.view.environment.workflow.error = Some("rejected".into());
        app.view.environment.workflow.notice = Some("done".into());
        click(&mut app, "review_error_dismiss");
        assert!(app.view.git_action_error.is_none());
        assert!(app.view.environment.workflow.error.is_none());
        assert!(app.view.environment.workflow.notice.is_none());
        // Actions of other panels are not this handler's.
        let ctx = egui::Context::default();
        assert!(!app.handle_git_action("review_file_open:0:0", &ctx));
        assert!(!app.handle_git_action("review_refresh", &ctx));
    }

    #[test]
    fn the_branch_control_opens_the_shared_picker_as_the_sidebar_s_own() {
        use crate::environment_ui::EnvironmentMenu;
        let (_temp, mut app) = fixture();
        let ctx = egui::Context::default();
        // The card that could show the same picker gives way.
        app.view.environment.open = true;
        click(&mut app, "ide_git_branch_toggle");
        assert!(app.view.composer_branches.open);
        assert_eq!(app.view.environment.menu, EnvironmentMenu::Branch);
        assert!(!app.view.environment.open);
        app.apply_action(
            UiAction::Clicked {
                id: "ide_git_branch_toggle".into(),
            },
            &ctx,
        );
        assert!(!app.view.composer_branches.open);
        assert_eq!(app.view.environment.menu, EnvironmentMenu::None);
    }

    #[test]
    fn stage_actions_reject_stale_scope_wrong_stage_and_missing_rows() {
        let (_temp, mut app) = fixture();
        assert!(app.handle_git_action("review_unstage:0:0", &egui::Context::default()));
        assert!(app.git_action_request.is_none());
        assert!(app.handle_git_action("review_stage:99:99", &egui::Context::default()));
        assert!(app.git_action_request.is_none());
        app.review_active_workspace = Some(PathBuf::from("another repository"));
        assert!(app.handle_git_action("review_stage:0:0", &egui::Context::default()));
        assert!(app.git_action_request.is_none());
        assert_eq!(app.git_action_generation, 0);
    }
}
