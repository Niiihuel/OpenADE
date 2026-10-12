use super::*;
use crate::environment_git::{Operation, Request, Worker};
use crate::environment_git_ui::EnvironmentGitPanel;

const COMMIT_BUSY: &str = "Creando commit…";

impl AgentsWindow {
    pub(super) fn request_environment_git(&mut self, operation: Operation, ctx: &egui::Context) {
        let busy = match &operation {
            Operation::Refresh => None,
            Operation::Compare { .. } => Some("Comparando ramas…"),
            Operation::Commit { .. } => Some(COMMIT_BUSY),
            Operation::Push { .. } => Some("Enviando commits…"),
            Operation::Fetch { .. } => Some("Buscando cambios en el remoto…"),
            Operation::Pull { .. } => Some("Recibiendo commits…"),
            Operation::CreateBranch { .. } | Operation::Switch { .. } => Some("Cambiando de rama…"),
            Operation::CreateWorktree { .. } => Some("Creando worktree…"),
        };
        if self.view.environment.workflow.busy.is_some() {
            return;
        }
        match &operation {
            Operation::Refresh | Operation::Compare { .. } | Operation::Fetch { .. } => {}
            Operation::Pull { snapshot }
            | Operation::Switch { snapshot, .. }
            | Operation::CreateBranch { snapshot, .. }
            | Operation::CreateWorktree { snapshot, .. }
            | Operation::Commit { snapshot, .. }
            | Operation::Push { snapshot, .. } => {
                if let Some(reason) = self.environment_git_blocked_reason(snapshot) {
                    self.view.environment.workflow.error = Some(reason);
                    return;
                }
            }
        }
        self.environment_git_revision = self.environment_git_revision.wrapping_add(1);
        self.environment_git_workspace = Some(PathBuf::from(&self.view.workspace));
        let request = Request {
            workspace: PathBuf::from(&self.view.workspace),
            revision: self.environment_git_revision,
            operation,
        };
        let result = (|| {
            if self.environment_git_worker.is_none() {
                self.environment_git_worker =
                    Some(Worker::spawn().map_err(|error| error.to_string())?);
            }
            self.environment_git_worker
                .as_ref()
                .unwrap()
                .submit(request)
        })();
        let workflow = &mut self.view.environment.workflow;
        workflow.error = None;
        workflow.notice = None;
        workflow.comparison = None;
        workflow.compare_url = None;
        match result {
            Ok(()) => {
                workflow.loading = true;
                workflow.busy = busy.map(str::to_owned);
            }
            Err(error) => {
                workflow.loading = false;
                workflow.busy = None;
                workflow.error = Some(error);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    pub(super) fn poll_environment_git(&mut self, ctx: &egui::Context) -> bool {
        let workspace = PathBuf::from(&self.view.workspace);
        let mut changed = false;
        if self
            .environment_git_workspace
            .as_ref()
            .is_some_and(|previous| previous != &workspace)
        {
            self.environment_git_revision = self.environment_git_revision.wrapping_add(1);
            self.environment_git_workspace = None;
            self.environment_git_queued = None;
            self.view.environment.workflow = Default::default();
            changed = true;
        }
        if (self.view.environment.open
            || self.view.environment.composer_local_open
            || self.git_sidebar_visible())
            && self.environment_git_workspace.is_none()
        {
            self.request_environment_git(Operation::Refresh, ctx);
            changed = true;
        }
        if !self.view.environment.workflow.loading {
            return changed;
        }
        let response = self
            .environment_git_worker
            .as_ref()
            .map(|worker| worker.poll_latest(self.environment_git_revision, &workspace));
        let response = match response {
            Some(Ok(Some(response))) => response,
            Some(Ok(None)) => {
                ctx.request_repaint_after(Duration::from_millis(50));
                return changed;
            }
            Some(Err(error)) => {
                self.environment_git_queued = None;
                self.view.environment.workflow.loading = false;
                self.view.environment.workflow.busy = None;
                self.view.environment.workflow.error = Some(error);
                return true;
            }
            None => {
                self.environment_git_queued = None;
                self.view.environment.workflow.loading = false;
                self.view.environment.workflow.busy = None;
                self.view.environment.workflow.error =
                    Some("El proceso de Git no está disponible".into());
                return true;
            }
        };
        let had_effect = response.outcome.is_some();
        let workflow = &mut self.view.environment.workflow;
        workflow.snapshot_revision = response.revision;
        let completed_commit = workflow.busy.as_deref() == Some(COMMIT_BUSY);
        let previous_head = workflow
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.head.clone());
        let previous_branch = workflow
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.current_branch().map(str::to_owned));
        workflow.loading = false;
        workflow.busy = None;
        if let Some(result) = response.outcome {
            match result {
                Ok(notice) => {
                    workflow.notice = Some(notice);
                    if completed_commit {
                        workflow.commit_message.clear();
                        self.tree
                            .set_input_value("environment_git_commit_message", "");
                    }
                }
                Err(error) => workflow.error = Some(error),
            }
        }
        match response.snapshot {
            Ok(snapshot) => {
                self.view.project_branch = Some(snapshot.branches.label.clone());
                if !snapshot.remotes.contains(&workflow.push_remote) {
                    workflow.push_remote = snapshot.default_remote().unwrap_or_default().to_owned();
                }
                let branch_changed = previous_branch.as_deref() != snapshot.current_branch();
                if workflow.push_branch.is_empty() || branch_changed {
                    workflow.push_branch = snapshot
                        .default_push_branch()
                        .unwrap_or_default()
                        .to_owned();
                    self.tree
                        .set_input_value("environment_git_push_branch", &workflow.push_branch);
                }
                if !snapshot
                    .branches
                    .branches
                    .iter()
                    .any(|branch| branch.reference == workflow.compare_reference)
                {
                    workflow.compare_reference = snapshot
                        .branches
                        .branches
                        .iter()
                        .find(|branch| {
                            !branch.current
                                && matches!(
                                    branch.name.as_str(),
                                    "origin/main" | "main" | "master" | "origin/master"
                                )
                        })
                        .or_else(|| {
                            snapshot
                                .branches
                                .branches
                                .iter()
                                .find(|branch| !branch.current)
                        })
                        .map(|branch| branch.reference.clone())
                        .unwrap_or_default();
                }
                if let Some(result) = response.comparison {
                    match result {
                        Ok(comparison)
                            if comparison.reference == workflow.compare_reference
                                && previous_head == snapshot.head
                                && !branch_changed =>
                        {
                            workflow.comparison = Some(comparison);
                        }
                        Ok(_) => {
                            workflow.comparison = None;
                        }
                        Err(error) => {
                            workflow.comparison = None;
                            workflow.error = Some(error);
                        }
                    }
                }
                workflow.compare_url = snapshot.compare_url(&workflow.compare_reference);
                workflow.snapshot = Some(snapshot);
            }
            Err(error) => {
                workflow.snapshot = None;
                workflow.comparison = None;
                workflow.compare_url = None;
                workflow.error.get_or_insert(error);
            }
        }
        if had_effect {
            self.request_git_review(workspace);
            self.invalidate_workspace_files();
            self.refresh_mcp_settings();
            self.refresh_native_skills();
        }
        // An action asked for during a plain re-read runs on what it read.
        if let Some(id) = self.environment_git_queued.take() {
            if self.view.environment.workflow.snapshot.is_some() {
                self.submit_environment_git_action(&id, ctx);
            }
        }
        true
    }

    pub(super) fn apply_environment_git_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        use crate::environment_ui::EnvironmentMenu;
        match action {
            UiAction::InputChanged { id, value } => {
                let workflow = &mut self.view.environment.workflow;
                let target = match id.as_str() {
                    "environment_git_commit_message" => &mut workflow.commit_message,
                    "environment_git_push_branch" => &mut workflow.push_branch,
                    "environment_git_branch_name" => &mut workflow.branch_name,
                    "environment_git_worktree_path" => &mut workflow.worktree_path,
                    _ => return false,
                };
                if workflow.busy.is_some() {
                    return true;
                }
                *target = value.chars().take(8192).collect();
                true
            }
            // Ctrl+Enter in the message commits, as its button does.
            UiAction::Submitted { id, .. } if id == "environment_git_commit_message" => {
                self.submit_environment_git_action("environment_git_commit", ctx);
                true
            }
            UiAction::Clicked { id } => {
                match id.as_str() {
                    "composer_environment_toggle" => {
                        if self.view.show_settings || self.view.main_page != MainPage::Workspace {
                            return true;
                        }
                        let open = !self.view.environment.composer_local_open;
                        self.close_composer_floating_controls();
                        self.prompt_focus_pending = false;
                        self.composer_model_search_focus_pending = false;
                        self.composer_selector_focus_pending = None;
                        self.view.environment.composer_local_open = open;
                        if open && !self.view.environment.workflow.loading {
                            self.request_environment_git(Operation::Refresh, ctx);
                        }
                    }
                    "composer_environment_dismiss" => {
                        self.view.environment.composer_local_open = false;
                    }
                    "environment_details_toggle" => {
                        self.view.environment.details_expanded =
                            !self.view.environment.details_expanded
                    }
                    "environment_subagents_toggle" => {
                        self.view.environment.subagents_expanded =
                            !self.view.environment.subagents_expanded
                    }
                    "environment_terminals_toggle" => {
                        self.view.environment.terminals_expanded =
                            !self.view.environment.terminals_expanded
                    }
                    "environment_subagents_stop" => {
                        // Each goes through its own cancel, with the guards
                        // its card's "Cancelar" has.
                        let going: Vec<String> = crate::environment_ui::subagents(&self.view)
                            .filter(|(_, stoppable)| *stoppable)
                            .map(|(run, _)| run.child_session_id.clone())
                            .collect();
                        for child in going {
                            self.cancel_subagent(&child);
                        }
                    }
                    "environment_local_toggle" => {
                        self.view.environment.composer_local_open = false;
                        self.view.environment.menu =
                            if self.view.environment.menu == EnvironmentMenu::Local {
                                EnvironmentMenu::None
                            } else {
                                EnvironmentMenu::Local
                            };
                        self.view.composer_project_menu_open = false;
                        self.view.composer_branches.open = false;
                    }
                    "environment_local_dismiss" => {
                        self.view.environment.menu = EnvironmentMenu::None
                    }
                    "environment_choose_project" => {
                        if let Some(reason) = self.branch_change_blocked_reason() {
                            self.view.environment.workflow.error = Some(reason.into());
                        } else {
                            let from_composer = self.view.environment.composer_local_open;
                            self.view.environment.composer_local_open = false;
                            self.view.environment.menu = EnvironmentMenu::None;
                            self.apply_action(
                                UiAction::Clicked {
                                    id: if from_composer {
                                        "composer_project_toggle"
                                    } else {
                                        "environment_project_toggle"
                                    }
                                    .into(),
                                },
                                ctx,
                            );
                        }
                    }
                    "environment_local_files" => {
                        self.close_environment();
                        self.open_files_dock();
                    }
                    "environment_copy_path" => {
                        ctx.copy_text(self.view.workspace.clone());
                        self.view.environment.composer_local_open = false;
                    }
                    "environment_git_refresh" => {
                        self.request_environment_git(Operation::Refresh, ctx)
                    }
                    "environment_git_open_commit"
                    | "environment_git_open_compare"
                    | "environment_git_open_branch"
                    | "environment_git_open_worktree" => {
                        self.close_composer_floating_controls();
                        if self.ide_workspace_active() {
                            // These actions are also used by the IDE's Git
                            // sidebar and Local picker. Reveal their actual
                            // host instead of opening an invisible Environment
                            // card and hiding the conversation on the right.
                            self.select_ide_activity(crate::ide_workspace::IdeActivity::Git, false);
                        } else {
                            self.open_environment();
                        }
                        self.view.environment.workflow.panel = match id.as_str() {
                            "environment_git_open_commit" => EnvironmentGitPanel::Commit,
                            "environment_git_open_compare" => EnvironmentGitPanel::Compare,
                            "environment_git_open_branch" => EnvironmentGitPanel::CreateBranch,
                            _ => EnvironmentGitPanel::CreateWorktree,
                        };
                        self.view.environment.menu = EnvironmentMenu::None;
                        self.view.environment.workflow.error = None;
                        if !self.view.environment.workflow.loading {
                            self.request_environment_git(Operation::Refresh, ctx);
                        }
                    }
                    "environment_git_back" => {
                        self.view.environment.workflow.panel = EnvironmentGitPanel::Overview;
                        self.view.environment.workflow.remote_menu_open = false;
                        self.view.environment.workflow.compare_menu_open = false;
                    }
                    "environment_git_remote_toggle" => {
                        self.view.environment.workflow.remote_menu_open =
                            !self.view.environment.workflow.remote_menu_open
                    }
                    "environment_git_remote_dismiss" => {
                        self.view.environment.workflow.remote_menu_open = false
                    }
                    "environment_git_compare_toggle" => {
                        self.view.environment.workflow.compare_menu_open =
                            !self.view.environment.workflow.compare_menu_open
                    }
                    "environment_git_compare_dismiss" => {
                        self.view.environment.workflow.compare_menu_open = false
                    }
                    "environment_git_compare_open_web" => {
                        // Recompute from the scoped snapshot; never open a URL
                        // injected into view state or an earlier workspace.
                        if self.environment_git_workspace.as_deref()
                            == Some(Path::new(&self.view.workspace))
                        {
                            let workflow = &self.view.environment.workflow;
                            if let Some(url) = workflow.snapshot.as_ref().and_then(|snapshot| {
                                snapshot.compare_url(&workflow.compare_reference)
                            }) {
                                ctx.open_url(egui::OpenUrl::new_tab(url));
                            }
                        }
                    }
                    "environment_git_commit"
                    | "environment_git_push"
                    | "environment_git_fetch"
                    | "environment_git_pull"
                    | "environment_git_create_branch"
                    | "environment_git_create_worktree"
                    | "environment_git_compare_refresh" => {
                        self.submit_environment_git_action(id, ctx);
                    }
                    _ if id.starts_with("environment_git_remote:") => {
                        if self.view.environment.workflow.loading {
                            return true;
                        }
                        let choice = id
                            .strip_prefix("environment_git_remote:")
                            .and_then(|raw| raw.parse::<usize>().ok())
                            .and_then(|index| {
                                self.view
                                    .environment
                                    .workflow
                                    .snapshot
                                    .as_ref()?
                                    .remotes
                                    .get(index)
                                    .cloned()
                            });
                        if let Some(remote) = choice {
                            self.view.environment.workflow.push_remote = remote;
                        }
                        self.view.environment.workflow.remote_menu_open = false;
                    }
                    _ if id.starts_with("environment_git_compare:") => {
                        if self.view.environment.workflow.loading {
                            return true;
                        }
                        let choice = id
                            .strip_prefix("environment_git_compare:")
                            .and_then(|raw| raw.parse::<usize>().ok())
                            .and_then(|index| {
                                self.view
                                    .environment
                                    .workflow
                                    .snapshot
                                    .as_ref()?
                                    .branches
                                    .branches
                                    .get(index)
                                    .map(|branch| branch.reference.clone())
                            });
                        if let Some(reference) = choice {
                            self.view.environment.workflow.compare_reference = reference;
                            self.view.environment.workflow.comparison = None;
                            self.view.environment.workflow.compare_url = None;
                            self.view.environment.workflow.compare_menu_open = false;
                            self.submit_environment_git_action(
                                "environment_git_compare_refresh",
                                ctx,
                            );
                        }
                    }
                    _ if id.starts_with("environment_worktree_select:") => {
                        let Some((revision, index)) = id
                            .strip_prefix("environment_worktree_select:")
                            .and_then(|value| value.split_once(':'))
                            .and_then(|(revision, index)| {
                                Some((revision.parse::<u64>().ok()?, index.parse::<usize>().ok()?))
                            })
                        else {
                            return true;
                        };
                        if !(self.view.environment.composer_local_open
                            || (self.view.environment.open
                                && self.view.environment.menu == EnvironmentMenu::Local))
                            || self.view.environment.workflow.loading
                            || self.view.environment.workflow.snapshot_revision != revision
                            || self.environment_git_workspace.as_deref()
                                != Some(Path::new(&self.view.workspace))
                        {
                            return true;
                        }
                        let blocked = self
                            .view
                            .environment
                            .workflow
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| self.environment_git_blocked_reason(snapshot));
                        if let Some(reason) = blocked {
                            self.view.environment.workflow.error = Some(reason);
                        } else if self.environment_git_workspace.as_deref()
                            == Some(Path::new(&self.view.workspace))
                            && !self.view.environment.workflow.loading
                        {
                            let path = self
                                .view
                                .environment
                                .workflow
                                .snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.worktrees.get(index))
                                .map(|worktree| worktree.path.clone());
                            if let Some(path) = path.filter(|path| path.is_dir()) {
                                self.close_environment();
                                self.change_workspace(path.to_string_lossy().into_owned());
                                self.visit_navigation();
                            }
                        }
                    }
                    _ => return false,
                }
                true
            }
            _ => false,
        }
    }

    fn submit_environment_git_action(&mut self, id: &str, ctx: &egui::Context) {
        if self.view.environment.workflow.busy.is_some() {
            return;
        }
        if self.view.environment.workflow.loading {
            self.environment_git_queued = Some(id.to_owned());
            return;
        }
        if self.git_action_request.is_some() {
            self.view.environment.workflow.error =
                Some("Espera a que termine la preparación de archivos.".into());
            return;
        }
        let Some(snapshot) = self.view.environment.workflow.snapshot.clone() else {
            return;
        };
        if self.environment_git_workspace.as_deref() != Some(Path::new(&self.view.workspace)) {
            return;
        }
        if !matches!(id, "environment_git_compare_refresh" | "environment_git_fetch") {
            if let Some(reason) = self.branch_change_blocked_reason() {
                self.view.environment.workflow.error = Some(reason.into());
                return;
            }
        }
        let workflow = &self.view.environment.workflow;
        let operation = match id {
            "environment_git_commit" => Operation::Commit {
                snapshot,
                message: workflow.commit_message.clone(),
            },
            "environment_git_push" => {
                let set_upstream = snapshot.upstream.is_none();
                Operation::Push {
                    snapshot,
                    remote: workflow.push_remote.clone(),
                    branch: workflow.push_branch.clone(),
                    set_upstream,
                }
            }
            "environment_git_fetch" => {
                let Some(remote) = snapshot.default_remote().map(str::to_owned) else {
                    self.view.environment.workflow.error =
                        Some("Elegí un remoto: este repositorio tiene varios.".into());
                    return;
                };
                Operation::Fetch { snapshot, remote }
            }
            "environment_git_pull" => Operation::Pull { snapshot },
            "environment_git_create_branch" => Operation::CreateBranch {
                snapshot,
                name: workflow.branch_name.trim().to_owned(),
            },
            "environment_git_create_worktree" => Operation::CreateWorktree {
                snapshot,
                name: workflow.branch_name.trim().to_owned(),
                path: PathBuf::from(workflow.worktree_path.trim()),
            },
            "environment_git_compare_refresh" => Operation::Compare {
                snapshot,
                reference: workflow.compare_reference.clone(),
            },
            _ => return,
        };
        self.request_environment_git(operation, ctx);
    }

    fn environment_git_blocked_reason(
        &self,
        snapshot: &crate::environment_git::GitSnapshot,
    ) -> Option<String> {
        self.branch_change_blocked_reason()
            .map(str::to_owned)
            .or_else(|| {
                self.view
                    .composer_branches
                    .switching()
                    .then(|| "Esperá a que termine el cambio de rama.".into())
            })
            .or_else(|| {
                match self
                    .native_command_sessions
                    .has_running_in_worktree(snapshot.branches.worktree_root())
                {
                    Ok(false) => None,
                    Ok(true) => Some(
                        "Detén los comandos activos en este proyecto antes de modificar Git."
                            .into(),
                    ),
                    Err(error) => Some(format!(
                        "No se pudo comprobar el estado de los comandos: {error}"
                    )),
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment_git::load_snapshot;
    use crate::environment_ui::EnvironmentMenu;

    fn fixture() -> (tempfile::TempDir, AgentsWindow, egui::Context) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.name", "OpenIDE Test"],
            vec!["config", "user.email", "openide-test@example.invalid"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "core.hooksPath", "/dev/null"],
        ] {
            crate::git_branches::git_text(&root, &args).unwrap();
        }
        std::fs::write(root.join("tracked.txt"), "initial\n").unwrap();
        crate::git_branches::git_text(&root, &["add", "--", "tracked.txt"]).unwrap();
        crate::git_branches::git_text(&root, &["commit", "-m", "Initial"]).unwrap();
        let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
        app.view.workspace = root.canonicalize().unwrap().to_string_lossy().into_owned();
        app.environment_git_workspace = Some(PathBuf::from(&app.view.workspace));
        app.view.environment.workflow.snapshot =
            Some(load_snapshot(Path::new(&app.view.workspace)).unwrap());
        app.view.environment.open = true;
        // Host interaction fixtures do not launch a real shell.
        app.settings_preferences.terminal_shell =
            TerminalShell::Custom("fixture-shell-relative-invalid".into());
        (directory, app, egui::Context::default())
    }

    fn click(app: &mut AgentsWindow, ctx: &egui::Context, id: &str) {
        assert!(app.apply_environment_git_action(&UiAction::Clicked { id: id.into() }, ctx));
    }

    fn settle(app: &mut AgentsWindow, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while app.view.environment.workflow.loading {
            app.poll_environment_git(ctx);
            assert!(
                Instant::now() < deadline,
                "environment Git worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn opening_environment_invalidates_loading_and_failed_repository_states() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = AgentsWindow::with_data_root(directory.path().join("data"));
        app.view.workspace = directory
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned();
        app.view.environment.open = true;
        let ctx = egui::Context::default();

        assert!(app.poll_environment_git(&ctx));
        assert!(
            app.view.environment.workflow.loading || app.view.environment.workflow.error.is_some()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.view.environment.workflow.loading {
            let changed = app.poll_environment_git(&ctx);
            if !app.view.environment.workflow.loading {
                assert!(
                    changed,
                    "the repository failure must invalidate the visible frame"
                );
            }
            assert!(Instant::now() < deadline, "Git worker did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.view.environment.workflow.error.is_some());
        assert!(!app.poll_environment_git(&ctx));
    }

    #[test]
    fn successful_commit_clears_its_draft_and_failed_commit_preserves_it() {
        let (_directory, mut app, ctx) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        let message = "Commit from environment";
        app.view.environment.workflow.commit_message = message.into();
        click(&mut app, &ctx, "environment_git_commit");
        settle(&mut app, &ctx);
        assert_eq!(app.view.environment.workflow.commit_message, message);
        assert!(app.view.environment.workflow.error.is_some());
        std::fs::write(root.join("tracked.txt"), "prepared\n").unwrap();
        crate::git_branches::git_text(&root, &["add", "--", "tracked.txt"]).unwrap();
        app.view.environment.workflow.snapshot = Some(load_snapshot(&root).unwrap());
        click(&mut app, &ctx, "environment_git_commit");
        settle(&mut app, &ctx);
        assert!(app.view.environment.workflow.error.is_none());
        assert!(app.view.environment.workflow.commit_message.is_empty());
        assert_eq!(
            crate::git_branches::git_text(&root, &["log", "-1", "--format=%s"]).unwrap(),
            message
        );
    }

    #[test]
    fn successful_push_preserves_the_next_commit_message() {
        let (directory, mut app, ctx) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        let remote = directory.path().join("remote.git");
        crate::git_branches::git_text(
            &root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        )
        .unwrap();
        crate::git_branches::git_text(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();
        app.view.environment.workflow.snapshot = Some(load_snapshot(&root).unwrap());
        app.view.environment.workflow.panel = EnvironmentGitPanel::Commit;
        app.view.environment.workflow.commit_message = "My next commit".into();
        app.view.environment.workflow.push_remote = "origin".into();
        app.view.environment.workflow.push_branch = "main".into();
        click(&mut app, &ctx, "environment_git_push");
        settle(&mut app, &ctx);
        assert!(
            app.view.environment.workflow.error.is_none(),
            "{:?}",
            app.view.environment.workflow.error
        );
        assert_eq!(
            app.view.environment.workflow.commit_message,
            "My next commit"
        );
        assert_eq!(
            app.view
                .environment
                .workflow
                .snapshot
                .as_ref()
                .unwrap()
                .upstream
                .as_deref(),
            Some("origin/main")
        );
    }

    #[test]
    fn fetch_and_pull_bring_a_published_branch_up_to_date() {
        let (directory, mut app, ctx) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        let remote = directory.path().join("remote.git");
        let git = |at: &Path, args: &[&str]| crate::git_branches::git_text(at, args).unwrap();
        git(&root, &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()]);
        git(&root, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&root, &["push", "-q", "--set-upstream", "origin", "main"]);
        let clone = directory.path().join("clone");
        git(directory.path(), &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        for (key, value) in [
            ("user.name", "OpenIDE Test"),
            ("user.email", "openide-test@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            git(&clone, &["config", key, value]);
        }
        std::fs::write(clone.join("remote.txt"), "remote\n").unwrap();
        git(&clone, &["add", "--", "remote.txt"]);
        git(&clone, &["commit", "-m", "Remote"]);
        git(&clone, &["push", "-q", "origin", "main"]);

        app.view.environment.workflow.snapshot = Some(load_snapshot(&root).unwrap());
        // Looking for news is allowed while the agent works; it moves nothing.
        app.view.running = true;
        click(&mut app, &ctx, "environment_git_fetch");
        assert_eq!(
            app.view.environment.workflow.busy.as_deref(),
            Some("Buscando cambios en el remoto…")
        );
        settle(&mut app, &ctx);
        let workflow = &app.view.environment.workflow;
        assert!(workflow.error.is_none(), "{:?}", workflow.error);
        assert_eq!(workflow.notice.as_deref(), Some("Referencias actualizadas desde origin"));
        assert_eq!(workflow.snapshot.as_ref().unwrap().behind, 1);
        assert!(!root.join("remote.txt").exists());

        // Receiving changes the work tree, so it waits for the agent.
        click(&mut app, &ctx, "environment_git_pull");
        assert!(!app.view.environment.workflow.loading);
        assert!(app.view.environment.workflow.error.is_some());
        app.view.running = false;
        click(&mut app, &ctx, "environment_git_pull");
        settle(&mut app, &ctx);
        let workflow = &app.view.environment.workflow;
        assert!(workflow.error.is_none(), "{:?}", workflow.error);
        assert_eq!(workflow.notice.as_deref(), Some("Rama al día con origin/main"));
        assert_eq!(workflow.snapshot.as_ref().unwrap().behind, 0);
        assert!(root.join("remote.txt").exists());
    }

    #[test]
    fn the_message_field_commits_and_a_click_during_a_reread_is_not_lost() {
        let (_directory, mut app, ctx) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        std::fs::write(root.join("tracked.txt"), "prepared\n").unwrap();
        crate::git_branches::git_text(&root, &["add", "--", "tracked.txt"]).unwrap();
        app.view.environment.workflow.commit_message = "From the sidebar".into();
        // The snapshot in hand predates the staging: re-read it, and ask
        // for the commit before the answer is back.
        click(&mut app, &ctx, "environment_git_refresh");
        assert!(app.view.environment.workflow.loading);
        assert!(app.apply_environment_git_action(
            &UiAction::Submitted {
                id: "environment_git_commit_message".into(),
                value: "From the sidebar".into(),
            },
            &ctx
        ));
        assert_eq!(app.environment_git_queued.as_deref(), Some("environment_git_commit"));
        settle(&mut app, &ctx);
        // The re-read landed and started the commit; let that finish too.
        assert!(app.environment_git_queued.is_none());
        settle(&mut app, &ctx);
        let workflow = &app.view.environment.workflow;
        assert!(workflow.error.is_none(), "{:?}", workflow.error);
        assert!(workflow.commit_message.is_empty());
        assert_eq!(
            crate::git_branches::git_text(&root, &["log", "-1", "--format=%s"]).unwrap(),
            "From the sidebar"
        );
    }

    #[test]
    fn the_visible_git_sidebar_reads_the_repository_without_the_environment_card() {
        let (_directory, mut app, ctx) = fixture();
        app.view.environment.open = false;
        app.view.environment.workflow.snapshot = None;
        app.environment_git_workspace = None;
        assert!(!app.poll_environment_git(&ctx), "nothing asks for the repository");
        app.view.ide.mode = crate::ide_workspace::WindowMode::Ide;
        app.view.ide.activity = crate::ide_workspace::IdeActivity::Git;
        app.view.ide.explorer_open = true;
        assert!(app.git_sidebar_visible());
        assert!(app.poll_environment_git(&ctx));
        settle(&mut app, &ctx);
        assert!(app.view.environment.workflow.snapshot.is_some());

        // A re-read on the list's behalf keeps what the last operation said.
        app.view.environment.workflow.notice = Some("Commit creado: 0c1d2e3f".into());
        app.view.environment.workflow.error = Some("previous failure".into());
        app.refresh_environment_git_quietly(&ctx);
        assert!(app.view.environment.workflow.loading);
        settle(&mut app, &ctx);
        let workflow = &app.view.environment.workflow;
        assert_eq!(workflow.notice.as_deref(), Some("Commit creado: 0c1d2e3f"));
        assert_eq!(workflow.error.as_deref(), Some("previous failure"));
    }

    #[test]
    fn host_rejects_mutations_while_the_agent_is_running() {
        let (_directory, mut app, ctx) = fixture();
        app.view.running = true;
        app.view.environment.workflow.branch_name = "blocked-branch".into();
        click(&mut app, &ctx, "environment_git_create_branch");
        assert!(!app.view.environment.workflow.loading);
        assert!(app.view.environment.workflow.error.is_some());
        assert!(app.environment_git_worker.is_none());
        assert_eq!(
            crate::git_branches::git_text(
                Path::new(&app.view.workspace),
                &["branch", "--show-current"]
            )
            .unwrap(),
            "main"
        );
    }

    #[test]
    fn refresh_retires_comparison_and_does_not_guess_among_multiple_remotes() {
        let (_directory, mut app, ctx) = fixture();
        let root = PathBuf::from(&app.view.workspace);
        crate::git_branches::git_text(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ],
        )
        .unwrap();
        crate::git_branches::git_text(
            &root,
            &["remote", "add", "fork", "https://github.com/fork/repo.git"],
        )
        .unwrap();
        app.view.environment.workflow.comparison = Some(Default::default());
        app.view.environment.workflow.compare_url = Some("https://invalid.example/old".into());
        click(&mut app, &ctx, "environment_git_refresh");
        assert!(app.view.environment.workflow.comparison.is_none());
        assert!(app.view.environment.workflow.compare_url.is_none());
        settle(&mut app, &ctx);
        assert!(app.view.environment.workflow.push_remote.is_empty());
    }

    #[test]
    fn environment_aliases_open_the_project_picker_and_files_dock() {
        let (_directory, mut app, ctx) = fixture();
        app.view.environment.menu = EnvironmentMenu::Local;
        click(&mut app, &ctx, "environment_choose_project");
        assert_eq!(app.view.environment.menu, EnvironmentMenu::Project);
        assert!(app.view.composer_project_menu_open);
        click(&mut app, &ctx, "environment_local_files");
        assert!(!app.view.environment.open);
        assert_eq!(app.view.side_tab, SideTab::Files);
    }

    #[test]
    fn changing_workspace_discards_pending_comparison_and_urls() {
        let (directory, mut app, ctx) = fixture();
        app.view.environment.workflow.comparison = Some(Default::default());
        app.view.environment.workflow.compare_url = Some("https://invalid.example/old".into());
        let old_revision = app.environment_git_revision;
        app.view.environment.open = false;
        app.view.workspace = directory
            .path()
            .join("different")
            .to_string_lossy()
            .into_owned();
        assert!(app.poll_environment_git(&ctx));
        assert!(app.environment_git_revision > old_revision);
        assert!(app.view.environment.workflow.snapshot.is_none());
        assert!(app.view.environment.workflow.comparison.is_none());
        assert!(app.view.environment.workflow.compare_url.is_none());
    }

    #[test]
    fn composer_environment_loads_without_card_and_excludes_other_popovers() {
        let (_directory, mut app, ctx) = fixture();
        app.view.composer_project_menu_open = true;
        app.view.composer_branches.open = true;
        app.view.composer_model_menu_open = true;
        app.view.composer_effort_menu_open = true;
        app.view.composer_context_open = true;
        app.view.composer_permissions_open = true;
        app.view.composer_add_menu_open = true;
        app.prompt_focus_pending = true;
        app.composer_model_search_focus_pending = true;
        app.composer_selector_focus_pending = Some("composer_branch_search");

        click(&mut app, &ctx, "composer_environment_toggle");
        assert!(app.view.environment.composer_local_open);
        assert!(!app.view.environment.open);
        assert!(!app.view.composer_project_menu_open);
        assert!(!app.view.composer_branches.open);
        assert!(!app.view.composer_model_menu_open);
        assert!(!app.view.composer_effort_menu_open);
        assert!(!app.view.composer_context_open);
        assert!(!app.view.composer_permissions_open);
        assert!(!app.view.composer_add_menu_open);
        assert!(!app.prompt_focus_pending);
        assert!(!app.composer_model_search_focus_pending);
        assert!(app.composer_selector_focus_pending.is_none());
        assert!(app.view.environment.workflow.loading);
        settle(&mut app, &ctx);
        assert_eq!(
            app.view
                .environment
                .workflow
                .snapshot
                .as_ref()
                .unwrap()
                .current_branch(),
            Some("main")
        );
        assert!(app.view.environment.workflow.snapshot_revision > 0);
        assert!(app.dismiss_environment_level(&ctx));
        assert!(!app.view.environment.composer_local_open);
        assert!(!app.dismiss_environment_level(&ctx));
    }

    #[test]
    fn composer_environment_project_and_git_forms_use_visible_origins() {
        let (_directory, mut app, ctx) = fixture();
        app.view.environment.open = false;
        app.view.environment.composer_local_open = true;
        click(&mut app, &ctx, "environment_choose_project");
        assert!(app.view.composer_project_menu_open);
        assert!(!app.view.environment.open);
        assert!(!app.view.environment.composer_local_open);
        assert_eq!(app.view.environment.menu, EnvironmentMenu::None);

        for (action, panel) in [
            (
                "environment_git_open_branch",
                EnvironmentGitPanel::CreateBranch,
            ),
            (
                "environment_git_open_worktree",
                EnvironmentGitPanel::CreateWorktree,
            ),
        ] {
            app.view.environment.open = false;
            app.view.environment.composer_local_open = true;
            click(&mut app, &ctx, action);
            assert!(app.view.environment.open);
            assert!(!app.view.environment.composer_local_open);
            assert!(!app.view.composer_project_menu_open);
            assert_eq!(app.view.environment.workflow.panel, panel);
            settle(&mut app, &ctx);
        }
    }

    #[test]
    fn composer_worktree_selection_requires_current_visible_snapshot_and_idle_agent() {
        let (directory, mut app, ctx) = fixture();
        let original = app.view.workspace.clone();
        let target = directory.path().join("second-worktree");
        crate::git_branches::git_text(
            Path::new(&original),
            &[
                "worktree",
                "add",
                "-b",
                "second",
                "--",
                target.to_str().unwrap(),
            ],
        )
        .unwrap();
        let snapshot = load_snapshot(Path::new(&original)).unwrap();
        let index = snapshot
            .worktrees
            .iter()
            .position(|entry| entry.path == target)
            .unwrap();
        app.view.environment.workflow.snapshot = Some(snapshot);
        app.view.environment.workflow.snapshot_revision = 21;
        app.view.environment.open = false;
        app.view.environment.composer_local_open = true;

        click(
            &mut app,
            &ctx,
            &format!("environment_worktree_select:20:{index}"),
        );
        assert_eq!(
            app.view.workspace, original,
            "an older snapshot must not redirect an index"
        );
        let action = format!("environment_worktree_select:21:{index}");
        app.view.environment.composer_local_open = false;
        click(&mut app, &ctx, &action);
        assert_eq!(
            app.view.workspace, original,
            "a dismissed menu has no authority"
        );
        app.view.environment.composer_local_open = true;
        app.view.environment.workflow.loading = true;
        click(&mut app, &ctx, &action);
        assert_eq!(app.view.workspace, original);
        app.view.environment.workflow.loading = false;
        app.view.running = true;
        click(&mut app, &ctx, &action);
        assert_eq!(app.view.workspace, original);
        assert!(app.view.environment.workflow.error.is_some());
        app.view.running = false;
        click(&mut app, &ctx, &action);
        assert_eq!(Path::new(&app.view.workspace), target);
        assert!(!app.view.environment.composer_local_open);
        assert!(!app.view.environment.open);
        assert_eq!(app.view.project_branch.as_deref(), Some("second"));
    }

    fn browser_fixture(
        app: &mut AgentsWindow,
        id: DockTabId,
    ) -> crate::browser_runtime::tests::BrowserCommands {
        let (runtime, commands) = crate::browser_runtime::tests::fixture(
            crate::browser_runtime::BrowserScope {
                workspace: app.view.workspace.clone().into(),
                session_id: format!("draft:{}", id.get()),
            },
            crate::browser_runtime::BrowserSnapshot::default(),
        );
        app.browsers.insert(id.get(), runtime);
        commands
    }

    #[test]
    fn every_environment_entrypoint_collapses_right_dock_without_losing_resources() {
        let (_directory, mut app, ctx) = fixture();
        let scope = app.dock_scope();
        let browser = app.dock.open(&scope, DockTabKind::Browser);
        let _browser_commands = browser_fixture(&mut app, browser);
        let files = app.dock.open(&scope, DockTabKind::Files);
        let terminal = app.dock.new_terminal(&scope);
        app.terminal_controls_host
            .errors
            .insert(terminal.get(), "Fixture terminal; no subprocess".into());
        app.dock.activate(&scope, files);
        app.view.prompt = "Conservar el borrador".into();
        let tabs = app
            .dock
            .visible_tabs(&scope)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in [
            "environment_open",
            "dock_open:context",
            "side_tab:context",
            "environment_git_open_commit",
            "environment_git_open_compare",
            "environment_git_open_branch",
            "environment_git_open_worktree",
            "project_actions_open",
        ] {
            app.close_environment();
            app.dock.set_collapsed(&scope, false);
            app.sync_dock_view();
            assert!(app.view.show_side_pane);
            app.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
            assert!(app.view.environment.open, "{id}");
            assert!(!app.view.show_side_pane, "{id}");
            assert!(app.dock.is_collapsed(&scope), "{id}");
            assert_eq!(
                app.dock.active_tab(&scope).map(|tab| tab.id),
                Some(files),
                "{id}"
            );
            assert_eq!(
                app.dock
                    .visible_tabs(&scope)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                tabs,
                "{id}"
            );
            assert!(app.dock.contains_tab(browser.get()));
            assert!(
                app.browsers.contains_key(&browser.get()),
                "hiding does not release the runtime"
            );
            assert!(app.browser_starts.is_empty());
            assert!(app.dock.contains_tab(terminal.get()));
            assert!(
                app.terminals.is_empty(),
                "the fixture must not spawn a shell"
            );
            assert!(
                !app.dock.is_terminal_collapsed(&scope),
                "bottom terminal is independent"
            );
            assert_eq!(app.view.prompt, "Conservar el borrador");
            settle(&mut app, &ctx);
        }
    }

    #[test]
    fn every_right_dock_entrypoint_closes_environment_and_its_inline_selector() {
        let (_directory, mut app, ctx) = fixture();
        let scope = app.dock_scope();
        let browser = app.dock.open(&scope, DockTabKind::Browser);
        let _browser_commands = browser_fixture(&mut app, browser);
        app.dock.open(&scope, DockTabKind::Files);
        for id in [
            "toggle_side_pane_header",
            "toggle_side_pane",
            "dock_open:files",
            "dock_open:browser",
            "dock_open:review",
            "dock_open:activity",
            "side_tab:activity",
            "tool:inspection",
        ] {
            app.open_environment();
            app.view.environment.menu = EnvironmentMenu::Project;
            app.view.composer_project_menu_open = true;
            app.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
            assert!(!app.view.environment.open, "{id}");
            assert!(!app.view.composer_project_menu_open, "{id}");
            assert_eq!(app.view.environment.menu, EnvironmentMenu::None, "{id}");
            assert!(app.view.show_side_pane, "{id}");
            assert!(!app.dock.is_collapsed(&scope), "{id}");
        }
    }

    #[test]
    fn hidden_dock_refresh_and_resize_preserve_environment_but_reactivation_replaces_it() {
        let (_directory, mut app, ctx) = fixture();
        let scope = app.dock_scope();
        let files = app.dock.open(&scope, DockTabKind::Files);
        app.open_environment();
        for width in [1600.0, 520.0, 1200.0] {
            app.viewport_width = width;
            app.sync_dock_view();
            assert!(app.view.environment.open);
            assert!(app.dock.is_collapsed(&scope));
            assert!(!app.view.show_side_pane);
        }
        app.apply_action(
            UiAction::Clicked {
                id: "dock_open:terminal".into(),
            },
            &ctx,
        );
        assert!(app.view.show_terminal_panel);
        assert!(
            app.terminals.is_empty(),
            "the fixture must not spawn a shell"
        );
        assert!(
            app.view.environment.open,
            "opening the bottom terminal does not reveal the right dock"
        );
        app.dock.activate(&scope, files);
        app.sync_dock_view();
        assert!(!app.view.environment.open);
        assert!(app.view.show_side_pane);
        app.open_environment();
        app.close_environment();
        assert!(
            app.dock.is_collapsed(&scope),
            "dismiss does not resurrect a hidden sidebar"
        );
    }

    #[test]
    fn ide_agent_entrypoints_replace_environment_even_when_session_navigation_is_rejected() {
        use crate::ide_workspace::{IdePanel, WindowMode};
        let (_directory, mut app, ctx) = fixture();
        app.view.ide.mode = WindowMode::Ide;
        for id in [
            "ide_agent_toggle",
            "toggle_side_pane_header",
            "session:missing",
            "new_session_header",
        ] {
            app.view.running = true;
            app.open_environment();
            assert!(!app.view.ide.agent_open);
            app.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
            assert!(!app.view.environment.open, "{id}");
            assert!(app.view.ide.agent_open, "{id}");
        }
        app.view.running = false;
        app.open_environment();
        app.reveal_ide_agent();
        assert!(!app.view.environment.open);
        assert!(app.view.ide.agent_open);
        app.open_environment();
        assert!(!app.view.ide.agent_open);
        assert_ne!(app.view.ide.compact_panel, Some(IdePanel::Agent));
    }

    #[test]
    fn switching_window_modes_clears_environment_animation_without_erasing_workflow_draft() {
        let (_directory, mut app, ctx) = fixture();
        app.view.environment.workflow.commit_message = "Cambios pendientes".into();
        app.view.environment.workflow.panel = EnvironmentGitPanel::Actions;
        app.view.project_actions.editor_open = true;
        app.view.project_actions.label_draft = "Verificar".into();
        app.view.project_actions.command_draft = "cargo check".into();
        app.open_environment();
        app.update_environment_motion(Instant::now());
        app.view.environment_presentation_progress = Some(0.5);
        for _ in 0..2 {
            app.apply_action(
                UiAction::Clicked {
                    id: "window_mode_toggle".into(),
                },
                &ctx,
            );
            assert!(!app.view.environment.open);
            assert!(app.environment_motion.is_none());
            assert!(app.view.environment_presentation_progress.is_none());
            assert_eq!(
                app.view.environment.workflow.panel,
                EnvironmentGitPanel::Overview
            );
            assert!(app.view.project_actions.editor_open);
            assert_eq!(app.view.project_actions.label_draft, "Verificar");
            assert_eq!(app.view.project_actions.command_draft, "cargo check");
            assert_eq!(
                app.view.environment.workflow.commit_message,
                "Cambios pendientes"
            );
        }
    }

    #[test]
    fn ide_environment_git_forms_use_the_left_sidebar_and_keep_agent_visible() {
        use crate::ide_workspace::{IdeActivity, WindowMode};
        let (_directory, mut app, ctx) = fixture();
        app.view.ide.mode = WindowMode::Ide;
        let scope = app.dock_scope();
        let editor = app.dock.open(
            &scope,
            DockTabKind::FilePreview {
                path: PathBuf::from("tracked.txt").into(),
            },
        );
        for initial_activity in [IdeActivity::Files, IdeActivity::Git] {
            for (id, panel) in [
                ("environment_git_open_commit", EnvironmentGitPanel::Commit),
                ("environment_git_open_compare", EnvironmentGitPanel::Compare),
                (
                    "environment_git_open_branch",
                    EnvironmentGitPanel::CreateBranch,
                ),
                (
                    "environment_git_open_worktree",
                    EnvironmentGitPanel::CreateWorktree,
                ),
            ] {
                app.view.ide.activity = initial_activity;
                app.view.ide.agent_open = true;
                app.view.environment.open = false;
                app.view.environment.workflow.commit_message = "Conservar borrador".into();
                app.apply_action(UiAction::Clicked { id: id.into() }, &ctx);
                assert_eq!(app.view.ide.activity, IdeActivity::Git, "{id}");
                assert!(app.view.ide.explorer_open, "{id}");
                assert!(app.view.ide.agent_open, "{id}");
                assert!(!app.view.environment.open, "{id}");
                assert_eq!(app.view.environment.workflow.panel, panel, "{id}");
                assert_eq!(app.dock.active_tab(&scope).map(|tab| tab.id), Some(editor));
                assert_eq!(
                    app.view.environment.workflow.commit_message,
                    "Conservar borrador"
                );
                settle(&mut app, &ctx);
            }
        }
    }
}
