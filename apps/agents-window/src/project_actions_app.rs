//! Host routing for workspace actions; only an explicit run click starts a PTY.
use super::*;
use crate::{
    environment_git_ui::EnvironmentGitPanel,
    environment_ui::EnvironmentMenu,
    project_actions::{
        ProjectActionsOperation, ProjectActionsRequest, ProjectActionsState, ProjectActionsWorker,
    },
};

#[derive(Default)]
pub(super) struct ProjectActionsHost {
    worker: Option<ProjectActionsWorker>,
    generation: u64,
    pending: Option<PendingActions>,
}
struct PendingActions {
    workspace: PathBuf,
    generation: u64,
    rollback: Option<ProjectActionsState>,
}

impl AgentsWindow {
    fn scope_project_actions(&mut self) -> bool {
        let workspace = PathBuf::from(&self.view.workspace);
        if self.view.project_actions.workspace.as_ref() == Some(&workspace) {
            return false;
        }
        self.project_actions.generation = self.project_actions.generation.wrapping_add(1);
        self.project_actions.pending = None;
        self.view.project_actions = ProjectActionsState {
            workspace: Some(workspace),
            ..Default::default()
        };
        self.tree.set_input_value("project_action_label", "");
        self.tree.set_input_value("project_action_command", "");
        true
    }

    fn request_project_actions(
        &mut self,
        operation: ProjectActionsOperation,
        rollback: Option<ProjectActionsState>,
        ctx: &egui::Context,
    ) {
        self.project_actions.generation = self.project_actions.generation.wrapping_add(1);
        let workspace = PathBuf::from(&self.view.workspace);
        let saving = matches!(&operation, ProjectActionsOperation::Save(_));
        let request = ProjectActionsRequest {
            data_root: self.data_root.clone(),
            workspace: workspace.clone(),
            generation: self.project_actions.generation,
            operation,
        };
        let result = (|| -> Result<(), String> {
            if self.project_actions.worker.is_none() {
                self.project_actions.worker =
                    Some(ProjectActionsWorker::spawn().map_err(|error| error.to_string())?);
            }
            self.project_actions
                .worker
                .as_ref()
                .unwrap()
                .submit(request)
                .map_err(|error| error.to_string())
        })();
        match result {
            Ok(()) => {
                self.view.project_actions.loading = !saving;
                self.view.project_actions.saving = saving;
                self.view.project_actions.error = None;
                self.project_actions.pending = Some(PendingActions {
                    workspace,
                    generation: self.project_actions.generation,
                    rollback,
                });
                ctx.request_repaint_after(Duration::from_millis(40));
            }
            Err(error) => {
                if let Some(previous) = rollback {
                    self.view.project_actions = previous;
                }
                self.view.project_actions.loading = false;
                self.view.project_actions.saving = false;
                self.view.project_actions.error = Some(error);
                self.sync_project_action_drafts();
            }
        }
    }

    fn sync_project_action_drafts(&mut self) {
        self.tree.set_input_value(
            "project_action_label",
            &self.view.project_actions.label_draft,
        );
        self.tree.set_input_value(
            "project_action_command",
            &self.view.project_actions.command_draft,
        );
    }

    fn open_project_actions(&mut self, new: bool, ctx: &egui::Context) {
        let changed = self.scope_project_actions();
        self.open_environment();
        self.view.environment.menu = EnvironmentMenu::None;
        self.view.environment.workflow.panel = EnvironmentGitPanel::Actions;
        if !self.view.project_actions.saving
            && (changed
                || (!self.view.project_actions.loading && !self.view.project_actions.editor_open))
        {
            self.request_project_actions(ProjectActionsOperation::Load, None, ctx);
        }
        if new {
            self.view.project_actions.begin_new();
            self.sync_project_action_drafts();
        }
    }

    fn save_project_action_draft(&mut self, ctx: &egui::Context) {
        if self.view.project_actions.loading
            || self.view.project_actions.saving
            || !self.view.project_actions.editor_open
        {
            return;
        }
        let previous = self.view.project_actions.clone();
        match self.view.project_actions.save_draft() {
            Ok(()) => {
                let actions = self.view.project_actions.actions.clone();
                self.request_project_actions(
                    ProjectActionsOperation::Save(actions),
                    Some(previous),
                    ctx,
                );
            }
            Err(error) => self.view.project_actions.error = Some(error),
        }
    }

    pub(super) fn poll_project_actions(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = self.scope_project_actions();
        // Switching workspaces never carries commands or unsaved drafts over.
        if changed
            && self.view.environment.open
            && self.view.environment.workflow.panel == EnvironmentGitPanel::Actions
        {
            self.request_project_actions(ProjectActionsOperation::Load, None, ctx);
        }
        while let Some(response) = self
            .project_actions
            .worker
            .as_ref()
            .and_then(ProjectActionsWorker::poll)
        {
            let matches = self
                .project_actions
                .pending
                .as_ref()
                .is_some_and(|pending| {
                    pending.generation == response.generation
                        && pending.workspace == response.workspace
                })
                && response.workspace == Path::new(&self.view.workspace);
            if !matches {
                continue;
            }
            let pending = self.project_actions.pending.take().unwrap();
            match response.result {
                Ok(snapshot) => {
                    self.view.project_actions.actions = snapshot.actions;
                    self.view.project_actions.presets = snapshot.presets;
                    self.view.project_actions.error = None;
                }
                Err(error) => {
                    if let Some(previous) = pending.rollback {
                        self.view.project_actions = previous;
                        self.sync_project_action_drafts();
                    }
                    self.view.project_actions.error = Some(error);
                }
            }
            self.view.project_actions.loading = false;
            self.view.project_actions.saving = false;
            changed = true;
        }
        if self.project_actions.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        changed
    }

    fn run_project_action(&mut self, id: &str) {
        let state = &self.view.project_actions;
        if state.loading
            || state.saving
            || state.workspace.as_deref() != Some(Path::new(&self.view.workspace))
        {
            return;
        }
        let Some(action) = state.actions.iter().find(|action| action.id == id).cloned() else {
            return;
        };
        if action.command.trim().is_empty() || action.command.chars().any(char::is_control) {
            self.view.project_actions.error =
                Some("La acción requiere un comando en una sola línea.".into());
            return;
        }
        let scope = self.dock_scope();
        let tab = self.dock.new_terminal(&scope);
        self.dock.activate(&scope, tab);
        self.view.show_dock_overview = false;
        self.view.show_dock_add_menu = false;
        self.close_environment();
        self.sync_dock_view();
        let result = self
            .terminals
            .get_mut(&tab.get())
            .ok_or_else(|| {
                self.view
                    .terminal_error
                    .clone()
                    .unwrap_or_else(|| "No se pudo abrir la terminal.".into())
            })
            .and_then(|terminal| {
                terminal
                    .send_command(&action.command)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                self.terminal_focus_pending = true;
                self.view.status = format!("Ejecutando {} en una terminal nueva", action.label);
            }
            Err(error) => {
                self.view.terminal_error = Some(error.clone());
                self.view.project_actions.error = Some(error);
            }
        }
    }

    pub(super) fn apply_project_actions_action(
        &mut self,
        action: &UiAction,
        ctx: &egui::Context,
    ) -> bool {
        match action {
            UiAction::InputChanged { id, value } | UiAction::Submitted { id, value }
                if matches!(
                    id.as_str(),
                    "project_action_label" | "project_action_command"
                ) =>
            {
                if self.view.project_actions.workspace.as_deref()
                    != Some(Path::new(&self.view.workspace))
                {
                    return true;
                }
                if self.view.project_actions.editor_open && !self.view.project_actions.saving {
                    if id == "project_action_label" {
                        self.view.project_actions.label_draft = value.chars().take(128).collect();
                    } else {
                        self.view.project_actions.command_draft =
                            value.chars().take(4097).collect();
                    }
                    if matches!(action, UiAction::Submitted { .. }) {
                        self.save_project_action_draft(ctx);
                    }
                }
                true
            }
            UiAction::Clicked { id } if id == "project_actions_open" => {
                self.open_project_actions(false, ctx);
                true
            }
            UiAction::Clicked { id } if id == "project_action_new" => {
                if !self.view.project_actions.saving {
                    self.open_project_actions(true, ctx);
                }
                true
            }
            UiAction::Clicked { id } if id.starts_with("project_action_") => {
                if self.view.project_actions.workspace.as_deref()
                    != Some(Path::new(&self.view.workspace))
                {
                    return true;
                }
                if id == "project_action_cancel" {
                    self.view.project_actions.cancel_edit();
                    self.sync_project_action_drafts();
                    return true;
                }
                if self.view.project_actions.loading || self.view.project_actions.saving {
                    return true;
                }
                if id == "project_action_save" {
                    self.save_project_action_draft(ctx);
                } else if let Some(id) = id.strip_prefix("project_action_edit:") {
                    if self.view.project_actions.begin_edit(id) {
                        self.view.environment.workflow.panel = EnvironmentGitPanel::Actions;
                        self.sync_project_action_drafts();
                    }
                } else if let Some(id) = id.strip_prefix("project_action_run:") {
                    self.run_project_action(id);
                } else {
                    let previous = self.view.project_actions.clone();
                    let result = if let Some(id) = id.strip_prefix("project_action_delete:") {
                        Ok(self.view.project_actions.remove(id))
                    } else if let Some(id) = id.strip_prefix("project_action_preset:") {
                        self.view.project_actions.add_preset(id).map(|()| true)
                    } else {
                        return true;
                    };
                    match result {
                        Ok(true) => self.request_project_actions(
                            ProjectActionsOperation::Save(
                                self.view.project_actions.actions.clone(),
                            ),
                            Some(previous),
                            ctx,
                        ),
                        Ok(false) => {}
                        Err(error) => self.view.project_actions.error = Some(error),
                    }
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
    use crate::project_actions::{self, ProjectAction};
    use std::fs;

    fn fixture() -> (tempfile::TempDir, AgentsWindow) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        fs::create_dir(&workspace).unwrap();
        let mut app = AgentsWindow::with_data_root(root.path().join("data"));
        app.view.workspace = workspace
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.view.selected_session = None;
        app.viewport_width = 1400.0;
        app.viewport_height = 800.0;
        (root, app)
    }
    fn click(app: &mut AgentsWindow, id: &str) {
        app.apply_action(
            UiAction::Clicked { id: id.into() },
            &egui::Context::default(),
        );
    }
    fn input(app: &mut AgentsWindow, id: &str, value: &str) {
        app.apply_action(
            UiAction::InputChanged {
                id: id.into(),
                value: value.into(),
            },
            &egui::Context::default(),
        );
    }
    fn settle(app: &mut AgentsWindow) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll_project_actions(&egui::Context::default());
            if app.project_actions.pending.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Project actions worker did not complete"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn load_save_edit_and_delete_do_not_start_commands_and_preserve_drafts_during_loading() {
        let (_root, mut app) = fixture();
        let terminals = app.terminals.len();
        click(&mut app, "project_action_new");
        input(&mut app, "project_action_label", "Check");
        input(&mut app, "project_action_command", "printf check");
        settle(&mut app);
        assert_eq!(app.view.project_actions.label_draft, "Check");
        assert_eq!(app.view.project_actions.command_draft, "printf check");
        click(&mut app, "project_action_save");
        settle(&mut app);
        assert_eq!(app.terminals.len(), terminals);
        let id = app.view.project_actions.actions[0].id.clone();
        assert_eq!(
            project_actions::load(&app.data_root, Path::new(&app.view.workspace))
                .unwrap()
                .actions[0]
                .command,
            "printf check"
        );
        click(&mut app, &format!("project_action_edit:{id}"));
        input(&mut app, "project_action_command", "printf edited");
        app.apply_action(
            UiAction::Submitted {
                id: "project_action_command".into(),
                value: "printf edited".into(),
            },
            &egui::Context::default(),
        );
        settle(&mut app);
        assert_eq!(app.view.project_actions.actions[0].command, "printf edited");
        assert_eq!(app.terminals.len(), terminals);
        assert!(app.active_run.is_none());
        click(&mut app, &format!("project_action_delete:{id}"));
        settle(&mut app);
        assert!(
            project_actions::load(&app.data_root, Path::new(&app.view.workspace))
                .unwrap()
                .actions
                .is_empty()
        );
    }

    #[test]
    fn failed_persistence_rolls_back_the_list_and_keeps_the_editable_draft() {
        let (root, mut app) = fixture();
        click(&mut app, "project_action_new");
        settle(&mut app);
        input(&mut app, "project_action_label", "Build");
        input(&mut app, "project_action_command", "cargo build");
        let invalid = root.path().join("not-a-directory");
        fs::write(&invalid, "file").unwrap();
        app.data_root = invalid;
        click(&mut app, "project_action_save");
        settle(&mut app);
        assert!(app.view.project_actions.actions.is_empty());
        assert!(app.view.project_actions.editor_open);
        assert_eq!(app.view.project_actions.command_draft, "cargo build");
        assert!(app.view.project_actions.error.is_some());
        assert!(!app.view.project_actions.saving);
    }

    #[test]
    fn stale_workspace_reply_and_run_click_cannot_use_previous_project_commands() {
        let (root, mut app) = fixture();
        let old = PathBuf::from(&app.view.workspace);
        project_actions::save(
            &app.data_root,
            &old,
            &[ProjectAction {
                id: "old".into(),
                label: "Old".into(),
                command: "printf old".into(),
            }],
        )
        .unwrap();
        click(&mut app, "project_actions_open");
        let other = root.path().join("other");
        fs::create_dir(&other).unwrap();
        app.view.workspace = other.to_string_lossy().into_owned();
        let terminals = app.terminals.len();
        click(&mut app, "project_action_run:old");
        assert_eq!(app.terminals.len(), terminals);
        settle(&mut app);
        assert_eq!(app.view.project_actions.workspace.as_ref(), Some(&other));
        assert!(app.view.project_actions.actions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_run_opens_a_new_terminal_and_executes_only_in_the_action_workspace() {
        let (_root, mut app) = fixture();
        app.settings_preferences.terminal_shell = crate::settings_preferences::TerminalShell::Bash;
        let workspace = PathBuf::from(&app.view.workspace);
        project_actions::save(
            &app.data_root,
            &workspace,
            &[ProjectAction {
                id: "run".into(),
                label: "Run".into(),
                command: "printf done > project-action-result.txt".into(),
            }],
        )
        .unwrap();
        click(&mut app, "project_actions_open");
        settle(&mut app);
        assert!(!workspace.join("project-action-result.txt").exists());
        let terminals = app.terminals.len();
        click(&mut app, "project_action_run:run");
        assert_eq!(
            app.terminals.len(),
            terminals + 1,
            "{:?}",
            app.view.terminal_error
        );
        assert!(app.view.show_terminal_panel);
        assert_ne!(app.view.side_tab, SideTab::Terminal);
        assert!(!app.view.environment.open);
        let deadline = Instant::now() + Duration::from_secs(5);
        // The shell creates/truncates the file before printf writes its bytes.
        // Wait for the result, not just the directory entry, under parallel load.
        while fs::read_to_string(workspace.join("project-action-result.txt"))
            .ok()
            .as_deref()
            != Some("done")
        {
            for terminal in app.terminals.values_mut() {
                terminal.poll();
            }
            assert!(
                Instant::now() < deadline,
                "Action did not run: {:?}",
                app.view.terminal_error
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fs::read_to_string(workspace.join("project-action-result.txt")).unwrap(),
            "done"
        );
        let first_terminal = app.active_terminal_id().unwrap();
        click(&mut app, "project_action_run:run");
        assert_ne!(app.active_terminal_id(), Some(first_terminal));
        assert_eq!(app.terminals.len(), terminals + 2);
    }
}
